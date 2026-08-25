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
choreGoal = "Teach dev-tree a two-tier boundary: product paths that must contain the diff, and tolerated paths a worker may touch without failing the fold."

chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "two-tier-boundary"
    , nodeTask =
        "Give dev-tree's boundary vocabulary a second tier, staying inside harness-dogfooding/dev-tree/ plus the one Rust fixture file. Today DevPlan.nodeBoundary is an exact-or-directory-prefix allowlist and every path outside it is a fold-failing violation — four live runs failed on benign hygiene files (README.md, __pycache__, .gitignore). Add `nodeTolerated :: [Text]` to DevPlan in HarnessTypes.hs (same prefix semantics, haddock explaining: paths a node MAY touch without failing, reported as info, never product paths). In Harness.hs, boundaryViolations gains the tolerated list: a diff path inside nodeBoundary is inside; a path inside nodeTolerated is TOLERATED — excluded from the violation list but returned separately so the fold receipt can carry each as a 'tolerated: <path>' evidence line (wire that into finishFold's receiptEvidence). An empty nodeTolerated is byte-for-byte today's behavior. Update every DevPlan construction site: this repo's harness-dogfooding/dev-tree/Chore.hs record literals, and the six embedded Haskell fixture literals in tidepool-harness/tests/dogfood_harness_typecheck.rs (add nodeTolerated = [] to each, mirroring how nodeSplit = Nothing was added there). Mention the tolerated tier in the worker prompt templates' boundary paragraph (one sentence). Follow existing haddock and naming style throughout."
    , nodeChecks =
        [ "grep -q 'nodeTolerated' harness-dogfooding/dev-tree/HarnessTypes.hs"
        , "grep -q 'nodeTolerated' harness-dogfooding/dev-tree/Harness.hs"
        , "grep -q 'nodeTolerated' tidepool-harness/tests/dogfood_harness_typecheck.rs"
        ]
    , nodeBoundary =
        ["harness-dogfooding/dev-tree", "tidepool-harness/tests/dogfood_harness_typecheck.rs"]
    , nodeTolerated = []
    , nodeOnFailure = AskOperator
    , nodeSplit =
        Just
          SplitSpec
            { splitHints =
                "Split into 2-3 sequential microtasks: the type + every construction site first (Haskell record literals and the Rust fixture literals together, so nothing is ever mid-broken), then the boundaryViolations/finishFold mechanics, then the prompt-template sentence. Cheap grep-class checks per microtask; GHC compilation is the orchestrator's own gate, do not attempt it here."
            , splitMaxTasks = 3
            }
    , childPlans = []
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 1, maxAgentCycles = 6, gateWiderThan = 4}

-- | The chore edit itself rides as uncommitted state in the dev tree.
choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = True
