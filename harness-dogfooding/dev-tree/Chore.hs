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
choreGoal = "Rung 3: the harness proposes its own plan from a goal — propose mode, typed DevPlan via runLLMTurn, operator approval form, journaled proposal, resume-safe."

chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "chore-proposer"
    , nodeTask =
        "Implement PROPOSE MODE for dev-tree, per the feasibility map in the plan (all seams verified by exploration; cite deviations in evidence). (1) HarnessTypes.hs: add `data ChoreMode = Authored { authoredPlan :: DevPlan } | ProposeFromGoal` (record-syntax payload rule for checkpointed sums) and `data PlanApproval = PlanApproval { planApproved :: Bool, revisionNote :: Text }` (flat — GForm rejects lists/recursion, so approval is approve/reject-with-note over a prose rendering, never an editable plan form); export both. (2) Chore.hs: export `choreMode :: ChoreMode` (ship it as ProposeFromGoal with a goal-only chore text: goal = build a tiny CLI stopwatch/timer utility in the scratch repo — checks python3 -m unittest class; keep chorePlan as a one-line placeholder DevPlan the propose path replaces, documented as such); Harness.hs initialState consumes choreMode (Authored p -> plan = p; ProposeFromGoal -> plan = placeholder). (3) The propose seam, in the resumeLoop path BETWEEN the phase guard and rootTree (ORDERING IS LOAD-BEARING: rootTree resolves the retained root worktree by the plan's nodeName, so the effective plan must exist first, and a resumed run must reuse the journaled proposal instead of re-proposing a differently-named root, which would orphan the retained tree): when mode is ProposeFromGoal — look up a journaled proposal first (new ProposeEvent kind in DevTreeJournal.hs carrying the DevPlan payload, SplitEvent as the precedent, decode included); if none, `proposed <- runLLMTurn @DevPlan (proposePrompt goal budget)` with a new Prompts.hs proposePrompt instructing: a DevPlan tree within the budget's depth/width (pre-validate the proposal against budget maxDepth and gateWiderThan BEFORE presenting approval, auto-reject-and-re-propose once on violation), real orchestrator-runnable nodeChecks, tight nodeBoundary per node, kebab-case names; then `say (renderPlan 0 proposed)` and `askUser @PlanApproval` — approved -> journal the ProposeEvent and proceed with the proposal as the plan for THIS run (also write it into the returned State's plan field so render shows the real plan); rejected with a note -> ONE bounded re-propose with the note appended, then a second rejection -> blocked with the note. (4) Pin coverage: extend dogfood_harness_typecheck.rs's dev-tree probe extra_decls with `__proposeDecision :: ReplanDecision -> DevPlan -> DevPlan; __proposeDecision = amendPlan`-style naming of any new pure decision function you introduce, and a `__planApprovalProbe :: PlanApproval` value forcing the new types — follow the existing probe style. Use scripts/worker-typecheck.sh on every edited .hs file before finishing (Prompts/HarnessTypes/Chore/DevTreeJournal/Harness/Unfold compile with the dev-tree include set). This is ONE worker task, deliberately unsplit — report honestly in obstacles if the scope was too large for one cycle."
    , nodeChecks =
        [ "grep -q 'ChoreMode' harness-dogfooding/dev-tree/HarnessTypes.hs"
        , "grep -q 'PlanApproval' harness-dogfooding/dev-tree/HarnessTypes.hs"
        , "grep -q 'ProposeFromGoal' harness-dogfooding/dev-tree/Chore.hs"
        , "grep -qE 'ProposeKind|propose' harness-dogfooding/dev-tree/DevTreeJournal.hs"
        , "grep -q 'proposePrompt' harness-dogfooding/dev-tree/Prompts.hs"
        , "grep -q 'PlanApproval' tidepool-harness/tests/dogfood_harness_typecheck.rs"
        ]
    , nodeBoundary = ["harness-dogfooding/dev-tree", "tidepool-harness/tests/dogfood_harness_typecheck.rs"]
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
