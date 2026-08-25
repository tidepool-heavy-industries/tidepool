{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The chore: what this dev-tree run is actually asked to do, xmonad-style
-- (a sibling config module the harness proper consumes rather than
-- hardcodes). Types stay in "HarnessTypes"; this module owns only the
-- VALUES — the goal, the plan tree, the budget, and the dirty-source flag —
-- so swapping a chore is an edit here, never a change to the harness's own
-- logic.
--
-- The shipped chore is THE RESTRUCTURE: dev-tree becomes a real
-- multi-module Haskell project (operator directive: serious,
-- well-structured, fluent Haskell — module boundaries where the section
-- headers already are), executed by dev-tree on its own source.
module Chore
  ( choreGoal
  , choreMode
  , chorePlan
  , choreBudget
  , choreSnapshotDirtySource
  ) where

import HarnessTypes (Budget (..), ChoreMode (..), DevPlan (..), OnFailure (..))
import Tidepool.Prelude

choreGoal :: Text
choreGoal = "Build a tiny CLI stopwatch/timer utility in the scratch repository. Its acceptance checks must use the python3 -m unittest class of commands."

choreMode :: ChoreMode
choreMode = ProposeFromGoal

-- | A one-line bootstrap value only. In 'ProposeFromGoal' mode the harness
-- replaces it with the approved (or resumed journaled) plan before root-tree
-- resolution and stores that effective plan back into 'State'.
chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "proposed-plan-placeholder"
    , nodeTask = "Placeholder replaced by the approved proposed plan."
    , nodeChecks = []
    , nodeBoundary = []
    , nodeTolerated = []
    , nodeOnFailure = Retry
    , nodeSplit = Nothing
    , childPlans = []
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 1, maxAgentCycles = 4, gateWiderThan = 4}

-- | Clean-tree protocol (operator, 2026-08-25): the chore config is
-- COMMITTED before launch, so runs fork from a real commit and fold back
-- by ordinary git merge.
choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = False
