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
  , WorkerResult (..)
  , RunSummary (..)
  , initialState
  , render
  ) where

import qualified Data.Text as T
import GHC.Generics (Generic)
import Tidepool.Aeson (FromJSON, ToJSON)
import Tidepool.Aeson.Schema (JsonSchema)
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

-- | A payload constructor in a SUM must use record syntax: generic JSON has no
-- key to put a positional field under, and 'State' is checkpointed through
-- 'ToJSON'\/'FromJSON'.  A positional @Blocked Text@ typechecks as a plain ADT
-- and fails only when the derive is demanded.
data Phase
  = Ready
  | Completed
  | Blocked { blockedReason :: Text }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | The tree is authored data, not a runtime workflow graph.  'Harness.loop'
-- interprets it with ordinary recursive Haskell.
data DevPlan = DevPlan
  { nodeName   :: Text
  , nodeTask   :: Text
  , childPlans :: [DevPlan]
  }
  deriving (Generic, ToJSON, FromJSON, Show, Eq)

-- | What every worker in this dogfood must finish its turn with.
--
-- This type IS the worker's @outputSchema@: @spawnAgent \@WorkerResult@ derives
-- the schema the backend holds the worker to from this declaration's own
-- 'Generic' metadata, and decodes the terminal payload back through the same
-- 'FromJSON'.  There is no second description of the result shape to drift
-- from the one the caller pattern-matches on, which is why 'JsonSchema' is in
-- the derive set and not optional.
--
-- SINGLE-CONSTRUCTOR RECORD, deliberately: a sum renders @oneOf@ at the schema
-- root and the backend refuses the turn whole at request validation.  An
-- alternative is modelled as a field ('readyForIntegration'), never as a
-- constructor.
data WorkerResult = WorkerResult
  { workSummary         :: Text
  , evidence            :: [Text]
  , readyForIntegration :: Bool
  }
  deriving (Generic, ToJSON, FromJSON, JsonSchema, Show, Eq)

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

-- | @render :: State -> Text@ — the LOCKED signature (see
-- @examples\/harness\/HarnessTypes.hs@).  Domain policy only: the driver
-- composes this output with the loop-iteration count, the prior compaction
-- summary, and capability\/finalization instructions.  This function does not
-- take the compaction summary as an argument, because that is a runtime fact
-- and the runtime's to supply.
-- `[fmt|{hole}|]` renders a hole through `Tidepool.Render.Render`, which has
-- instances for Text/String/Int/Double/Bool/Char and nothing else — an
-- author-defined type has no rendering the quoter could guess. So 'Phase' and
-- 'RunSummary' get explicit ones here, which is also where they belong: how a
-- phase reads to the model is domain policy, not a `Show` accident.
render :: State -> Text
render st =
  [fmt|You are operating a typed recursive software-development tree.
Goal: {goal st}
Phase: {phaseLine}
Resident cycle: {cycleCount st}

Plan:
{renderPlan 0 (plan st)}

Dirty source snapshot allowed: {snapshotDirtySource st}
{lastRunBlock}

The Haskell resident owns orchestration. Headless coding agents retain their
native edit, shell, test, and Git tools. Repository events are authoritative;
agent summaries are not.|]
  where
    phaseLine = case phase st of
      Ready -> "ready" :: Text
      Completed -> "completed"
      Blocked {blockedReason = reason} -> "blocked — " <> reason
    lastRunBlock = case lastRun st of
      Nothing -> "No development-tree run has completed yet." :: Text
      Just summary ->
        T.intercalate "\n" $
          ["Last run:"]
            <> map ("  implementation: " <>) (implementationSummaries summary)
            <> map ("  integration: " <>) (integrationSummaries summary)
            <> map ("  retained worktree: " <>) (retainedWorktrees summary)

renderPlan :: Int -> DevPlan -> Text
renderPlan depth p =
  indent <> "- " <> nodeName p <> ": " <> nodeTask p <> children
  where
    indent = T.replicate depth "  "
    children = case childPlans p of
      [] -> ""
      xs -> "\n" <> T.intercalate "\n" (map (renderPlan (depth + 1)) xs)
