{-# LANGUAGE DeriveAnyClass #-}
{-# LANGUAGE DeriveGeneric #-}
{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}
{-# LANGUAGE QuasiQuotes #-}

-- | Durable vocabulary for the recursive-development-tree dogfood.
--
-- This file intentionally contains no live Agent, Event, or Worktree handles:
-- those are cycle-scoped runtime capabilities.  Only the semantic plan and
-- final receipts cross a resident-cycle boundary.
module HarnessTypes
  ( State (..)
  , Phase (..)
  , DevPlan (..)
  , DevMessage (..)
  , WorkerResult (..)
  , RunSummary (..)
  , initialState
  , render
  ) where

import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Prelude hiding (render)
import Tidepool.QQ (fmt)

data State = State
  { goal                :: Text
  , plan                :: DevPlan
  , phase               :: Phase
  , cycleCount          :: Int
  , snapshotDirtySource :: Bool
  , lastRun             :: Maybe RunSummary
  }
  deriving (Generic, ToJSON, FromJSON, Show)

data Phase
  = Ready
  | Completed
  | Blocked Text
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The tree is authored data, not a runtime workflow graph.  'Harness.loop'
-- interprets it with ordinary recursive Haskell.
data DevPlan = DevPlan
  { nodeName   :: Text
  , nodeTask   :: Text
  , childPlans :: [DevPlan]
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | Typed steering understood by every implementation worker in this dogfood.
-- The worker still uses its native shell/edit/git tools to do the work.
data DevMessage
  = RebaseWhenSafe
      { upstreamNode :: Text
      , upstreamHead :: Text
      }
  | FinishAndCommit
      { finishReason :: Text
      }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

data WorkerResult = WorkerResult
  { workSummary         :: Text
  , evidence            :: [Text]
  , readyForIntegration :: Bool
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | Model reports are useful summaries; Agent and Worktree receipts remain the
-- authoritative account of commands, changed files, commits, and HEAD moves.
data RunSummary = RunSummary
  { implementationSummaries :: [Text]
  , integrationSummaries    :: [Text]
  , retainedWorktrees       :: [Text]
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

initialState :: State
initialState =
  State
    { goal = "Land the first useful typed Worktree/Event substrate in Tidepool"
    , plan = initialPlan
    , phase = Ready
    , cycleCount = 0
    , snapshotDirtySource = False
    , lastRun = Nothing
    }

initialPlan :: DevPlan
initialPlan =
  DevPlan
    { nodeName = "integration"
    , nodeTask =
        "Own the shared seam, keep the tree coherent, and prepare to integrate "
          <> "the child branches after their workers finish."
    , childPlans =
        [ DevPlan
            { nodeName = "worktree-runtime"
            , nodeTask =
                "Implement managed worktree allocation, stable identities, "
                  <> "clean-source rejection, and the opt-in dirty snapshot."
            , childPlans =
                [ DevPlan
                    { nodeName = "commit-monitor"
                    , nodeTask =
                        "Implement reconciled commit and HEAD-change observation. "
                          <> "Start with polling and leave the hook/socket seam clean."
                    , childPlans = []
                    }
                ]
            }
        , DevPlan
            { nodeName = "haskell-surface"
            , nodeTask =
                "Implement Event and withHandler with lexical, cycle-scoped "
                  <> "handler lifetimes and parent-effect execution."
            , childPlans = []
            }
        , DevPlan
            { nodeName = "acceptance"
            , nodeTask =
                "Write end-to-end acceptance coverage for worktree isolation, "
                  <> "dirty-source policy, event delivery, and retained state."
            , childPlans = []
            }
        ]
    }

render :: State -> Maybe Text -> Text
render st lastCompaction =
  [fmt|You are operating a typed recursive software-development tree.
Goal: {goal st}
Phase: {phase st}
Resident cycle: {cycleCount st}

Plan:
{renderPlan 0 (plan st)}

Dirty source snapshot allowed: {snapshotDirtySource st}
{lastRunBlock}
{compactionBlock}

The Haskell resident owns orchestration. Headless coding agents retain their
native edit, shell, test, and Git tools. Repository events are authoritative;
agent summaries are not.|]
  where
    lastRunBlock = case lastRun st of
      Nothing -> "No development-tree run has completed yet." :: Text
      Just summary -> [fmt|Last run: {summary}|]
    compactionBlock = case lastCompaction of
      Nothing -> ""
      Just summary -> "\nPrior-window summary:\n" <> summary

renderPlan :: Int -> DevPlan -> Text
renderPlan depth p =
  indent <> "- " <> nodeName p <> ": " <> nodeTask p <> children
  where
    indent = T.replicate depth "  "
    children = case childPlans p of
      [] -> ""
      xs -> "\n" <> T.intercalate "\n" (map (renderPlan (depth + 1)) xs)
