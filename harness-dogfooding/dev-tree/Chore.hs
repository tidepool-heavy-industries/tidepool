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
choreGoal = "Close the five verified fold-soundness holes from the cross-family review: micro failures must be ladder-visible, denials must fail loudly, snapshots must not lie, recon must be provably clean, and Retry must retry."

chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "fold-soundness"
    , nodeTask =
        "Fix five verified soundness holes in harness-dogfooding/dev-tree/{Harness.hs,HarnessTypes.hs} (a cross-model review confirmed each with a concrete failing scenario; fixture literals in tidepool-harness/tests/dogfood_harness_typecheck.rs may be updated ONLY if a type change forces it). (1) MICRO FAILURES MUST REACH THE LADDER: runMicrotasks currently reduces failed micro-checks to prose escalations, so foldLadder never sees them. Make it return typed per-microtask results carrying each micro's CheckResults; microLeaf must MERGE every micro CheckResult into the receiptChecks it hands finishFold (so ladder rung 2 judges them mechanically), and when the sequence stopped early the leaf must fold to Failed with a NEW FailureKind MicrotasksIncomplete naming how many of the accepted tasks ran — never a Done whose evidence quietly mentions unrun work. (2) ALL-CHILDREN-DENIED IS A FAILURE, NOT A LEAF: in integrate, a WorkReady whose plan HAS childPlans but whose workKids is empty must fold to Failed with the (currently never-constructed) FailureKind WorktreeDenied carrying the denial texts — today it falls through to leafFold and the parent quietly implements the node itself. (3) SNAPSHOTS MUST NOT LIE: snapshotWork currently ignores every git failure; make it return whether a snapshot was needed and whether it succeeded, and on failure have runWorker surface a loud evidence/escalation line AND leave the receipt honest (a dirty tree whose commit failed must not fold Done — thread the failure so finishFold's caller can fail the node with SnapshotFailed, a second new FailureKind). (4) RECON IS PROVABLY READ-ONLY: after the recon spawn in microLeaf, run git status --porcelain in the worktree; if dirty, git checkout -- . && git clean -fd (the recon contract says read-only, so nothing of value is lost), and record an evidence line that recon strayed and was reset. (5) RETRY MUST RETRY: onChildFailure's Retry arm (and the TriageRetry answer there) currently only appends an escalation; make Retry respawn the failed LEAF child's worker once — runWorker in the child's own worktree with the original workerPrompt plus a short previous-attempt-failed amendment naming the failure — then re-run that child's checks and boundary, re-judge via the same finishFold/foldLadder path, count the extra cycle in the accumulator, and merge the child only if the retried outcome is Done; a second failure keeps today's escalation behavior. Also update the render/renderPlan and any prompt sentence that a new FailureKind or changed semantics makes stale, and keep every existing green-path behavior byte-compatible (empty tolerated/denials/etc. unchanged). Follow the file's haddock style; every new mechanism gets the same comment density its neighbors have."
    , nodeChecks =
        [ "grep -q 'MicrotasksIncomplete' harness-dogfooding/dev-tree/HarnessTypes.hs"
        , "grep -q 'SnapshotFailed' harness-dogfooding/dev-tree/HarnessTypes.hs"
        , "grep -q 'WorktreeDenied' harness-dogfooding/dev-tree/Harness.hs"
        , "grep -qE 'checkout -- .|clean -fd' harness-dogfooding/dev-tree/Harness.hs"
        , "test 1 -eq $(grep -c 'do NOT run' harness-dogfooding/dev-tree/Harness.hs)"
        ]
    , nodeBoundary =
        ["harness-dogfooding/dev-tree", "tidepool-harness/tests/dogfood_harness_typecheck.rs"]
    , nodeTolerated = []
    , nodeOnFailure = AskOperator
    , nodeSplit =
        Just
          SplitSpec
            { splitHints =
                "Split into 3-5 sequential microtasks along the five fixes — types first (the two new FailureKinds + the typed micro-result record, with every construction/match site updated in the same cycle so the file never sits mid-broken), then the runMicrotasks/microLeaf rework, then denial routing + snapshot honesty, then recon reset + Retry respawn. Grep-class checks per micro; compilation is the orchestrator's post-fold gate (your shell has no ghc)."
            , splitMaxTasks = 5
            }
    , childPlans = []
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 1, maxAgentCycles = 8, gateWiderThan = 4}

-- | The chore edit itself rides as uncommitted state in the dev tree.
choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = True
