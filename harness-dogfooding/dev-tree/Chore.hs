{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The chore: what this dev-tree run is actually asked to do, xmonad-style
-- (a sibling config module the harness proper consumes rather than
-- hardcodes). Types stay in "HarnessTypes"; this module owns only the
-- VALUES — the goal, the plan tree, the budget, and the dirty-source flag —
-- so swapping a chore is an edit here, never a change to the harness's own
-- logic.
--
-- The shipped chore is dev-tree EDITING ITS OWN SOURCE: a micro-split leaf
-- against the dev checkout that teaches the harness's boundary vocabulary a
-- second tier (product paths vs tolerated paths) — the #1 false-red source
-- across the live runs, chosen from the observation-driven leverage list.
module Chore
  ( choreGoal
  , chorePlan
  , choreBudget
  , choreSnapshotDirtySource
  ) where

import HarnessTypes (Budget (..), DevPlan (..), OnFailure (..), SplitSpec (..))
import Tidepool.Prelude

choreGoal :: Text
choreGoal = "Probe the worker sandbox environment through the real app-server spawn path and commit the findings as a file."

chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "env-probe"
    , nodeTask =
        "Create PROBE.md in the repository root containing, verbatim and clearly labeled, the output of each of these commands run in your shell: `which ghc || echo ghc-not-found`, `ghc --numeric-version || echo no-ghc`, `echo $PATH`, `which cabal || echo cabal-not-found`, `env | grep -E \\\"TIDEPOOL|RUSTC\\\" || echo no-tidepool-vars`. Do not interpret or summarize — paste the raw output. Change no other file."
    , nodeChecks = ["test -s PROBE.md", "grep -q PATH PROBE.md"]
    , nodeBoundary = ["PROBE.md"]
    , nodeTolerated = []
    , nodeOnFailure = Retry
    , nodeSplit = Nothing
    , childPlans = []
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 1, maxAgentCycles = 2, gateWiderThan = 4}

-- | The chore edit itself rides as uncommitted state in the dev tree.
choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = False
