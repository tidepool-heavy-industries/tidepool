{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The chore: what this dev-tree run is actually asked to do, xmonad-style
-- (a sibling config module the harness proper consumes rather than
-- hardcodes). Types stay in "HarnessTypes"; this module owns only the
-- VALUES — the goal, the mode, the budget, and the dirty-source flag —
-- so swapping a chore is an edit here, never a change to the harness's own
-- logic.
--
-- The shipped chore is SPRINT 25b — the two items sprint 25 left unfinished
-- that are genuinely disjoint from each other.  The harness-correctness
-- campaign is deliberately NOT here: its acceptance checks depend on
-- worker-typecheck-v2's deliverable (the sibling-dependency rule this very
-- failure taught), so it runs as its own follow-up once the fixed script is
-- on main.  Both items have retained branches from sprint 25 holding
-- substantial prior work — evaluate and reuse rather than restart.
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
choreGoal = "Sprint 25b: finish worker-typecheck-v2 and the self-hosted CI gate (sprint 25's unmerged items). See the per-item goals in choreMode."

choreMode :: ChoreMode
choreMode =
  SprintBacklog
    { sprintItems =
        [ SprintItem
            { itemGoal =
                "worker-typecheck v2, second attempt. Sprint 25's near-complete work is on branch tidepool/worktree/worker-typecheck-v2-wt-c3ea9840-f744-4828-b314-a111a54316d5 (visible from your worktree) — start by evaluating and cherry-picking it rather than rewriting. The task: scripts/worker-typecheck.sh discovers only the newest generated Tidepool/Effects.hs dir and MISSES the separate stable Tidepool.Effects.Core generated dir, so typechecks of files importing the stable module fail (reproduced from the dev checkout itself: `scripts/worker-typecheck.sh harness-dogfooding/dev-tree/Fold.hs` fails with 'Could not find module Tidepool.Effects.Core'); fix discovery to include both dirs. Also add a scoped-format helper mode (only the listed files, never cargo fmt --all). Keep it self-contained POSIX shell; PRESERVE the existing CLI contract (a file argument is required; bare invocation is a usage error — do not add a zero-arg mode). Checks must respect that contract: verify with `sh -n`, and with a real invocation against a tracked .hs file that imports Tidepool.Effects.Core, e.g. `scripts/worker-typecheck.sh harness-dogfooding/dev-tree/Fold.hs`. Boundary: scripts/worker-typecheck.sh only."
            , itemPlan = Nothing
            , -- 6, not 3: an item whose planner proposes an interior node
              -- with two children needs 2 (its own reservation) + 1 per
              -- child MINIMUM, and 3 floors every child's share to zero —
              -- the item then delivers nothing.  'fundingShortfall' now
              -- refuses that shape at proposal time; the allowance here is
              -- sized so the natural 2-3 child plan these goals imply is
              -- actually fundable.
              itemCycles = 6
            }
        , SprintItem
            { itemGoal =
                "Self-hosted CI gate, second attempt. Sprint 25's substantially-complete work is on branch tidepool/worktree/self-hosted-ci-gate-wt-32aab794-07ef-429c-b5b3-0a6228a6ca73 (visible from your worktree) — evaluate and cherry-pick it rather than rewriting; its design was sound and its only in-run check failure was a legitimate formatting violation it correctly caught. The deliverable: ci/gate.sh (portable fail-fast shell) with the ordered default gates fmt-check, clippy -D warnings, fast-tier cargo nextest, the dogfood pin battery, one deterministic GHC battery shard via scripts/battery-shard.sh under scripts/ghc-slots.sh; an explicit full-battery mode; ci/README.md documenting modes, gate order, budget rationale, and the no-mutation contract. IMPORTANT check design: the full default gate builds the workspace and CANNOT run inside your node's 600-second check budget — checks must be structural only (`sh -n ci/gate.sh`, `ci/gate.sh --help`, an invalid-mode exit-code probe); the operator runs the real gate against the dev checkout at fold. Boundary: ci/ (new directory); tolerated: docs/."
            , itemPlan = Nothing
            , itemCycles = 6
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
    , nodeScaffold = Nothing
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
