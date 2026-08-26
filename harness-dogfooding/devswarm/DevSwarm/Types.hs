{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | Pure vocabulary shared by DevSwarm owner sessions.
--
-- A node's decomposition is dynamic: its owner reasons first, may fork child
-- owners or ask short-lived delegates, and finally returns one 'OwnerOutcome'
-- to its parent. There is no pre-authored execution tree in this module.
module DevSwarm.Types
  ( NodeBrief (..)
  , OwnerOutcome (..)
  , OwnerReport (..)
  , Blockage (..)
  , OperatorQuestion (..)
  , renderNodeBrief
  , renderOwnerOutcome
  ) where

import qualified Tidepool.Data.Text as T
import Tidepool.Prelude

data NodeBrief = NodeBrief
  { nodeObjective :: Text
  , nodeContext :: Text
  , nodeConstraints :: [Text]
  }
  deriving (Eq, Show)

-- | What one owner tells its parent. These are organizational outcomes: a
-- blocked node or request for operator intent is not disguised as a weak
-- success report.
data OwnerOutcome
  = OwnerCompleted OwnerReport
  | OwnerBlocked Blockage
  | OwnerNeedsOperator OperatorQuestion
  deriving (Eq, Show)

data OwnerReport = OwnerReport
  { ownerSummary :: Text
  , ownerDecisions :: [Text]
  , ownerFollowUps :: [Text]
  }
  deriving (Eq, Show)

newtype Blockage = Blockage
  { blockageReason :: Text
  }
  deriving (Eq, Show)

newtype OperatorQuestion = OperatorQuestion
  { operatorQuestion :: Text
  }
  deriving (Eq, Show)

renderNodeBrief :: NodeBrief -> Text
renderNodeBrief brief =
  T.unlines
    [ "Own this DevSwarm node."
    , ""
    , "Objective:"
    , nodeObjective brief
    , ""
    , "Context:"
    , nodeContext brief
    , ""
    , "Constraints:"
    , bullets (nodeConstraints brief)
    , ""
    , "Reason first. Recursively fork child owners only for genuinely durable"
    , "decomposition; use delegateTask for short-lived repository work."
    , "Finalize one OwnerOutcome."
    ]

renderOwnerOutcome :: OwnerOutcome -> Text
renderOwnerOutcome (OwnerCompleted report) =
  T.unlines
    [ ownerSummary report
    , section "Decisions" (ownerDecisions report)
    , section "Follow-ups" (ownerFollowUps report)
    ]
renderOwnerOutcome (OwnerBlocked blockage) =
  "Blocked: " <> blockageReason blockage
renderOwnerOutcome (OwnerNeedsOperator question) =
  "Needs operator input: " <> operatorQuestion question

section :: Text -> [Text] -> Text
section _ [] = ""
section heading xs = heading <> ":\n" <> bullets xs

bullets :: [Text] -> Text
bullets [] = "(none)"
bullets xs = T.unlines (map ("- " <>) xs)
