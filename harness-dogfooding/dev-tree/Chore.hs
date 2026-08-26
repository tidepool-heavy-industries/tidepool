{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The chore: what this dev-tree run is actually asked to do, xmonad-style
-- (a sibling config module the harness proper consumes rather than
-- hardcodes). Types stay in "HarnessTypes"; this module owns only the
-- VALUES — the goal, the mode, the budget, and the dirty-source flag —
-- so swapping a chore is an edit here, never a change to the harness's own
-- logic.
--
-- The shipped chore is SPRINT 25 — the first SprintBacklog run: a
-- parallelized task-set (operator directive: a run is a sprint), with
-- everything harness-editing serialized inside ONE micro-split correctness
-- campaign and the genuinely disjoint items running concurrently beside it.
module Chore
  ( choreGoal
  , choreMode
  , chorePlan
  , choreBudget
  , choreSnapshotDirtySource
  ) where

import HarnessTypes (Budget (..), ChoreMode (..), DevPlan (..), OnFailure (..), SprintItem (..))
import Tidepool.Prelude

choreGoal :: Text
choreGoal = "Sprint 25: harness correctness campaign + disjoint infrastructure items. See the per-item goals in choreMode."

choreMode :: ChoreMode
choreMode =
  SprintBacklog
    { sprintItems =
        [ SprintItem
            { itemGoal =
                "Harness correctness campaign over harness-dogfooding/dev-tree/ (the dev-tree harness's own source, a multi-module Haskell project) plus its pin tests (tidepool-harness/tests/probes/*.hs and tidepool-harness/tests/dogfood_harness_typecheck.rs). This item MUST be one leaf with nodeSplit set (micro-decomposition: sequential typed microtasks in one worktree) because every sub-task edits the same modules. The micros, in order: (1) AMENDMENT STATE MACHINE: today a ReplanEvent's consumption is implicit (newest-wins vs later outcomes) — give amendments durable identity: an amendment id + target branch + status Pending|Consumed|Abandoned as journal events (DevTreeJournal: append new kinds at the END for tag stability), consume-on-use in Resume/Fold, and key the rescue re-entry bound (Harness.rescueCount) by amendment generation instead of a global integer. Decode of OLD journals without these events must behave exactly as today. (2) WIRE HONESTY: journalOutcome currently journals the PRE-ladder outcome, so a failed node's wire is a bare Done-shaped receipt and every reader must re-apply the fold ladder; make the fold journal the LADDERED outcome (Failed wire with failure + receipt) while decodeOutcomeValue keeps reading both shapes (old journals stay foldable); update the wire-faithful fixtures in ResumeDecisionProbe.hs to cover BOTH generations. (3) SHA-BOUND RECEIPTS: before mergeChild folds a branch, require the branch HEAD to equal the receipt's receiptHead (a receipt is a claim about a specific sha); after any rebase in the cascade, re-run that node's checks + boundary and journal a REPLACEMENT outcome at the new sha. (4) HARNESS-DRIVEN REBASE: the mechanical rebase tier runs git rebase / git rebase --continue itself via Exec in the worktree, spawning a resolution agent ONLY to edit conflicted files (never to run git); delete the resolution-prompt text that currently asks the agent to drive git. (5) PROBES FINISH: move the remaining string-spliced Haskell probe decls in dogfood_harness_typecheck.rs into per-harness Probes.hs files (companion, recursive-companion, dev-tree — precedent: tests/probes/ResumeDecisionProbe.hs, include_str! wiring); byte-equivalent probe coverage, no string-concat Haskell left beyond module lists. Every micro ends with scripts/worker-typecheck.sh green; the mandatory self-consistency sweep closes the sequence."
            , itemPlan = Nothing
            , itemCycles = 9
            }
        , SprintItem
            { itemGoal =
                "worker-typecheck v2: scripts/worker-typecheck.sh currently discovers only the newest generated Tidepool/Effects.hs dir and MISSES the separate stable Tidepool.Effects.Core generated dir, so worker in-sandbox typechecks fail on imports of the stable module; fix the discovery to include both dirs. Also add a scoped-format helper mode (check only the files the worker touched, never cargo fmt --all — workspace-wide fmt drift in unrelated crates repeatedly blocked workers). Keep the script self-contained POSIX shell; boundary is scripts/worker-typecheck.sh only."
            , itemPlan = Nothing
            , itemCycles = 2
            }
        , SprintItem
            { itemGoal =
                "Journal timestamps: every line the shared durable-JSONL machinery writes should carry ts (milliseconds since epoch) so the run journal, the trace stream, and any future stream merge into one timeline. Add ts to the journal entry wire in tidepool-handlers/src/handlers/journal.rs (JournalEntry + to_json/from_json with ts OPTIONAL on decode so old segments load; bump the segment version via the existing journal_version ladder with a migration that defaults ts), stamp it in append alongside seq, and extend the existing unit tests. Do NOT touch the Haskell side or the resume fold semantics — ts is provenance, never what any fold sorts on (position stays the ordering truth; see last_by_kind_key's doc). Boundary: tidepool-handlers/src/handlers/journal.rs, tidepool-handlers/src/handlers/journal_version.rs."
            , itemPlan = Nothing
            , itemCycles = 3
            }
        , SprintItem
            { itemGoal =
                "Self-hosted CI: design and build a pre-merge CI entry point this repository's own harness runs can invoke. This is deliberately underspecified — YOU design it against what exists: the test tiers in the root CLAUDE.md (fast tier cargo nextest; sharded GHC batteries via scripts/battery-shard.sh with a ~380s per-invocation budget; the ghc-slots.sh box-wide semaphore), the pin battery, and cargo fmt/clippy. Deliverable shape: a ci/ directory with an entry script (e.g. ci/gate.sh) that runs an ordered, fail-fast gate sequence sized to the budget, a ci/README.md stating what runs when and why, and nothing that mutates the repo. Surface a decision to the operator (askUser with a small derived form) if you face a genuine fork about scope or tier selection. Boundary: ci/ (new directory); tolerated: docs/."
            , itemPlan = Nothing
            , itemCycles = 4
            }
        ]
    }

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
    , nodeCycles = Nothing
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 3, maxAgentCycles = 20, gateWiderThan = 5}

-- | Clean-tree protocol (operator, 2026-08-25): the chore config is
-- COMMITTED before launch, so runs fork from a real commit and fold back
-- by ordinary git merge.
choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = False
