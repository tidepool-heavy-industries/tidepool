{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The chore: what this dev-tree run is actually asked to do, xmonad-style
-- (a sibling config module the harness proper consumes rather than
-- hardcodes). Types stay in "HarnessTypes"; this module owns only the
-- VALUES — the goal, the plan tree, the budget, and the dirty-source flag —
-- so swapping a chore is an edit here, never a change to the harness's own
-- logic.
--
-- The shipped chore is a TOY: a single leaf, in prep for dev-tree's
-- first-ever live attended run against a throwaway scratch repo.
module Chore
  ( choreGoal
  , chorePlan
  , choreBudget
  , choreSnapshotDirtySource
  ) where

import HarnessTypes (Budget (..), DevPlan (..), OnFailure (..))
import Tidepool.Prelude

choreGoal :: Text
choreGoal = "Create NOTES.md with a short haiku about tide pools and commit it."

chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "haiku-notes"
    , nodeTask = "Create NOTES.md containing a short haiku about tide pools, then commit it."
    , nodeChecks = ["test -s NOTES.md"]
    , nodeBoundary = ["NOTES.md"]
    , nodeOnFailure = AskOperator
    , childPlans = []
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 1, maxAgentCycles = 2, gateWiderThan = 4}

choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = False
