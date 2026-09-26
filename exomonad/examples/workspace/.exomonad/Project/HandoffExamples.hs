{-# LANGUAGE OverloadedStrings #-}

-- | Compose a compact proposal from observations the caller already holds.
-- The caller names remaining obligations; this projection grants no authority.
module Project.HandoffExamples (handoffProposal) where

import Data.Text (Text)
import qualified Data.Text as Text
import Project.CheckResults (CheckState, checksSummary)
import Project.Observe (candidateSummary, reviewSummary)
import Project.Types (Outcome, Candidate, ReviewDecision)

handoffProposal
  :: Outcome Candidate
  -> Maybe (Outcome ReviewDecision)
  -> CheckState
  -> [Text]
  -> Text
handoffProposal candidate review checks remaining = Text.unlines
  [ "Reported candidate: " <> candidateSummary candidate
  , "Reported review: " <> maybe "not supplied" reviewSummary review
  , "Observed checks: " <> checksSummary checks
  , "Remaining obligations: " <> if null remaining
      then "none supplied; this does not establish integration"
      else Text.intercalate "; " remaining
  ]
