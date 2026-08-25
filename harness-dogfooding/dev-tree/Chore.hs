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
  , chorePlan
  , choreBudget
  , choreSnapshotDirtySource
  ) where

import HarnessTypes (Budget (..), DevPlan (..), OnFailure (..), SplitSpec (..))
import Tidepool.Prelude

choreGoal :: Text
choreGoal = "Make missing record fields (and their runtime-bottom kin) a COMPILE error in the extract pipeline, so config-class Haskell fails loud at compile instead of exploding mid-run."

chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "fatal-missing-fields"
    , nodeTask =
        "A live run just crashed at runtime on a Chore.hs record construction missing a field — GHC warned, the pipeline surfaced only errors, and laziness deferred the explosion mid-run. Fix at the mechanism: (1) In haskell/src/Tidepool/GhcPipeline.hs, promote missing-fields AND incomplete-patterns AND incomplete-uni-patterns to FATAL warnings in the DynFlags every pipeline variant compiles user/harness code with (the GHC API: wopt_set for the WarningFlag plus wopt_set_fatal / adding to fatal warning flags — find the exact idiom in the GHC 9.12 API; the file already customizes general flags like Opt_FullLaziness, follow that style and comment WHY: config-class modules must fail loud at compile, per the runtime missing-field crash of 2026-08-25). Make sure the promotion applies to harness/session/eval compiles uniformly and does NOT reject the stdlib itself (if any stdlib module currently has an incomplete pattern, fix that module too — it is a latent bug by this policy). (2) Add a Fidelity regression group in the extract-fidelity-test suite (haskell/test — follow the existing Fidelity.* group structure and cabal stanza wiring if a new module needs listing in haskell/tidepool-extract.cabal): a module with a missing record field must FAIL the pipeline with a diagnostic naming the field, red-then-green style. (3) In tidepool-harness/tests/dogfood_harness_typecheck.rs, add a force-probe line to the dev-tree probe's extra_decls: a binding that deep-forces the chore values (e.g. __choreForce :: Int; __choreForce = length (show chorePlan) + length (show choreBudget)) so a runtime-bottom in chore VALUES is caught at pin time even for bottoms fatal warnings cannot see. NOTE: worker-typecheck.sh does NOT apply to extract-internal sources (they need the ghc package; the orchestrator gates those with cabal) — use it only for the Rust-side probe file edits if any .hs is touched elsewhere."
    , nodeChecks =
        [ "grep -qE 'MissingFields|missing-fields' haskell/src/Tidepool/GhcPipeline.hs"
        , "grep -qE 'IncompletePatterns|incomplete-patterns' haskell/src/Tidepool/GhcPipeline.hs"
        , "grep -q '__choreForce' tidepool-harness/tests/dogfood_harness_typecheck.rs"
        ]
    , nodeBoundary =
        ["haskell/src/Tidepool/GhcPipeline.hs", "haskell/test", "haskell/lib", "haskell/tidepool-extract.cabal", "tidepool-harness/tests/dogfood_harness_typecheck.rs"]
    , nodeTolerated = []
    , nodeOnFailure = AskOperator
    , nodeSplit =
        Just
          SplitSpec
            { splitHints =
                "Split into 2-3 sequential microtasks: the DynFlags promotion + any stdlib incomplete-pattern fixes it flushes out first, then the Fidelity red-then-green regression, then the Rust force-probe + self-consistency sweep. The orchestrator gates compilation (cabal build for extract internals) after the fold."
            , splitMaxTasks = 3
            }
    , childPlans = []
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 1, maxAgentCycles = 6, gateWiderThan = 4}

-- | Clean-tree protocol (operator, 2026-08-25): the chore config is
-- COMMITTED before launch, so runs fork from a real commit and fold back
-- by ordinary git merge.
choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = False
