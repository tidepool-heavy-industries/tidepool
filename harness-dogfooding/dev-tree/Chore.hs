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
choreGoal = "Assemble a small tide-pool field guide: a haiku and a facts file."

chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "field-guide"
    , nodeTask = "Coordinate two children producing HAIKU.md and FACTS.md."
    , nodeChecks = ["test -s HAIKU.md", "test -s FACTS.md"]
    , nodeBoundary = ["HAIKU.md", "FACTS.md"]
    , nodeOnFailure = AskOperator
    , childPlans =
        [ DevPlan
            { nodeName = "haiku"
            , nodeTask = "Create HAIKU.md containing a short haiku about tide pools."
            , nodeChecks = ["test -s HAIKU.md"]
            , nodeBoundary = ["HAIKU.md"]
            , nodeOnFailure = Retry
            , childPlans = []
            }
        , DevPlan
            { nodeName = "facts"
            , nodeTask = "Create FACTS.md listing three true facts about tide pools, one per line."
            , nodeChecks = ["test -s FACTS.md"]
            , nodeBoundary = ["FACTS.md"]
            , nodeOnFailure = Retry
            , childPlans = []
            }
        ]
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 2, maxAgentCycles = 6, gateWiderThan = 4}

choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = False
