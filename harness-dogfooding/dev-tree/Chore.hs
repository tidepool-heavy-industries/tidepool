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
choreGoal = "PRD: rebuild the operator web UI (tidepool-web) around a minimal harness-agnostic node lifecycle. CONTEXT: the UI grew by accretion (tabs, note feeds, gate patches) hyperfitted to one harness; the operator's distillation: nodes have a starting prompt seed, produce notes and asks, finalize with a value, and live in a tree — that is ALL the UI needs. Confirmed gaps: no seed crosses the gate seam (node_gate carries only a label); no final value (retire_node fires ~57 lines before the finalized value exists in driver.rs's service_outer_branch); the timeline does not persist (answered asks vanish; clear_notes wipes the feed each await_continue); the tree is static (tab strip renders once; late nodes orphan-append to document.body in shell.rs applyPatch); labeled children's Haskell turns misroute to the default panel (post_turn_source uses self.gate instead of resolve_gate). REQUIRED SHAPE: (1) tidepool-harness/src/selfharness/operator.rs gains three DEFAULT-IMPLEMENTED OperatorGate methods — node_seeded(label,seed), node_finalized(label,value), node_failed(label,reason) — and driver.rs's service_outer_branch calls them at birth (after node_gate, with the authored brief), and at the exit/closure-refusal/success paths (capture the retired label before retire_node; call node_finalized with the rendered value before the Finalize event consumes it); fix the turn-source routing via resolve_gate. Extend tests/labeled_branch.rs's RoutingProbe for all three + a defaults-are-no-ops unit. (2) tidepool-web/src/server.rs: rebuild NodeSlot around seed/timeline/final_value/failure/done (timeline = append-only Note|Ask items; answered asks REPLACED IN PLACE with their submission, chronology preserved; delete clear_notes; new setters bump rev); WebGate implements the three new methods; routes/SSE/collect_form_json/formapi wire shapes UNCHANGED. (3) tidepool-web/src/render.rs: node_panel becomes one node-section (header+status badge, collapsible seed details, timeline in true order with answered asks read-only, failure pre, final value pre, turn history) — model text stays maud-escaped text only. (4) tidepool-web/src/shell.rs: outline of all nodes (path-indented, loop node pinned first, NO tabs), collapse class on a stable wrapper so SSE patches survive toggles, applyPatch inserts late nodes sorted by data-path instead of orphan-append. (5) tests: server.rs units for the timeline lifecycle; operator_gate.rs inverts await_continue_clears_the_note_feed to notes_persist_across_continue and adds seed/final-value/SSE-late-node assertions; shell.rs JS-source assertions updated; the --demo mock driver exercises seed->notes->ask->finalize plus one failure. Keep loopback trust model, wire verbs, and form-api hardening untouched. VERIFICATION per node: cargo check -p tidepool-web class checks are too slow in-worktree — use structural greps in nodeChecks; the orchestrator runs cargo gates post-fold."

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
choreBudget = Budget {maxDepth = 2, maxAgentCycles = 12, gateWiderThan = 5}

-- | Clean-tree protocol (operator, 2026-08-25): the chore config is
-- COMMITTED before launch, so runs fork from a real commit and fold back
-- by ordinary git merge.
choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = False
