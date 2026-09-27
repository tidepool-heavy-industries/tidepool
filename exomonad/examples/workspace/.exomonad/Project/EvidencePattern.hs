{-# LANGUAGE DataKinds #-}
{-# LANGUAGE OverloadedLabels #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE TypeOperators #-}

-- | Select one useful item from evidence the caller has already retained.
-- Construction is pure. The caller owns the bounded set and the Jev call.
module Project.EvidencePattern
  ( Evidence (..)
  , EvidenceCriteria (..), defaultCriteria
  , EvidenceSelection (..)
  , EvidenceAlternatives
  , describeEvidence, evidenceOffers, evidenceQuestion, settleEvidence
  ) where

import Data.Text (Text)
import qualified Data.Text as Text
import Jev.Operators (type (::>), type (::*), type (:|:))
import qualified Jev.Operators as J
import Tidepool.Aeson.Value (Value)

-- | The exact address and excerpt stay beside the client's typed value.
-- An excerpt may be partial; its source should identify the retained artifact
-- precisely enough for the caller to expand it.
data Evidence a = Evidence
  { evidenceKey :: Text
  , evidenceSource :: Text
  , evidenceExcerpt :: Text
  , evidenceValue :: a
  } deriving (Show, Eq)

data EvidenceCriteria = EvidenceCriteria
  { selectionQuestion :: Text
  , usefulWhen :: Text
  , noneWhen :: Text
  , insufficientWhen :: Text
  } deriving (Show, Eq)

defaultCriteria :: Text -> EvidenceCriteria
defaultCriteria intent = EvidenceCriteria
  { selectionQuestion = "Which supplied evidence is most useful for " <> intent <> "?"
  , usefulWhen = "This item directly helps answer the stated intent from its supplied excerpt."
  , noneWhen = "The supplied items are sufficiently described, but none helps answer the stated intent."
  , insufficientWhen = "The supplied excerpts or task context are insufficient to decide usefulness."
  }

data EvidenceSelection a
  = Selected (Evidence a)
  | NoUsefulEvidence
  | InsufficientEvidence
  deriving (Show, Eq)

type EvidenceAlternatives a =
  ("none" ::> EvidenceSelection a)
    :|: (("insufficient" ::> EvidenceSelection a)
      :|: ("candidate" ::* Evidence a))

-- | The default candidate wording keeps the address and exact supplied
-- excerpt beside the runtime option key. A client may use this directly or
-- supply complete wording of its own when the evidence is in shared state.
describeEvidence :: Evidence a -> Text
describeEvidence row = Text.intercalate "\n"
  [ "Key: " <> evidenceKey row
  , "Source: " <> evidenceSource row
  , "Excerpt: " <> evidenceExcerpt row
  ]

-- | The wording function controls each candidate's complete wording.
-- Alternatives still carry the original typed row, never a lookup key.
evidenceOffers
  :: EvidenceCriteria -> (Evidence a -> Text) -> [Evidence a]
  -> J.Offers (EvidenceAlternatives a)
evidenceOffers criteria describe rows =
  J.alt #none (noneWhen criteria) NoUsefulEvidence
    J..| J.alt #insufficient (insufficientWhen criteria) InsufficientEvidence
    J..| J.many #candidate evidenceKey describe rows

evidenceQuestion
  :: EvidenceCriteria -> (Evidence a -> Text) -> [Evidence a]
  -> J.Q Value (J.Choice (EvidenceAlternatives a))
evidenceQuestion criteria describe rows =
  J.choice (selectionQuestion criteria <> " " <> usefulWhen criteria)
    (evidenceOffers criteria describe rows)

-- | This applies a caller-chosen confidence policy. A settled
-- 'InsufficientEvidence' remains distinct from transport failure and doubt.
-- The caller should also retain the original answer for its distribution.
settleEvidence
  :: J.Policy p -> J.Chosen (EvidenceAlternatives a)
  -> Either J.Doubt (J.Settled p (EvidenceSelection a))
settleEvidence policy answer =
  J.settle policy answer
    (#none id J..| #insufficient id J..| #candidate (\_ row -> Selected row))
