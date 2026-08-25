{-# LANGUAGE NoImplicitPrelude #-}
{-# LANGUAGE OverloadedStrings #-}

-- | The chore: what this dev-tree run is actually asked to do, xmonad-style
-- (a sibling config module the harness proper consumes rather than
-- hardcodes). Types stay in "HarnessTypes"; this module owns only the
-- VALUES — the goal, the plan tree, the budget, and the dirty-source flag —
-- so swapping a chore is an edit here, never a change to the harness's own
-- logic.
--
-- The shipped chore is the SCALE-UP tidepool-on-tidepool run: a multi-child
-- tree against the dev checkout — an empty-task root (scaffold skipped
-- structurally), one direct leaf, one micro-split leaf, folded by the
-- integration merge. Both tasks close loops this morning's runs opened.
module Chore
  ( choreGoal
  , chorePlan
  , choreBudget
  , choreSnapshotDirtySource
  ) where

import HarnessTypes (Budget (..), DevPlan (..), OnFailure (..), SplitSpec (..))
import Tidepool.Prelude

choreGoal :: Text
choreGoal = "Harden dev-tree's event-failure story end to end: adopt the new non-fatal Tidepool.Event Try surface in the harness, and make the codex adapter's turn-deadline errors say what actually happened."

chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "event-hardening"
    , nodeTask = ""
    , nodeChecks =
        [ "grep -q 'withHandlerTry' harness-dogfooding/dev-tree/Harness.hs"
        , "grep -rq 'deadline' tidepool-agent/src/backend/codex/"
        ]
    , nodeBoundary = ["harness-dogfooding/dev-tree/Harness.hs", "tidepool-agent"]
    , nodeOnFailure = AskOperator
    , nodeSplit = Nothing
    , childPlans =
        [ DevPlan
            { nodeName = "adopt-try"
            , nodeTask =
                "In harness-dogfooding/dev-tree/Harness.hs ONLY: migrate runWorker's event observation from the throwing `withHandler (headChanged tree) (noteHeadMove name)` to the new non-fatal `withHandlerTry` from Tidepool.Event, so an EventError can no longer abort the harness loop. Semantics: the handler now receives `Either EventError (Observed HeadChangeReceipt)` — on Right, behave exactly as noteHeadMove does today; on Left, `say` a short note that head-move observation failed (include the rendered error) and continue. If withHandlerTry itself returns Left (subscribe failed, so the body never ran), fall back to running the same spawnAgent call WITHOUT any handler, after saying that observation is degraded — the worker cycle must still happen. Keep the function's public shape and everything else in the file byte-identical; follow the file's haddock and naming style."
            , nodeChecks =
                [ "grep -q 'withHandlerTry' harness-dogfooding/dev-tree/Harness.hs"
                ]
            , nodeBoundary = ["harness-dogfooding/dev-tree/Harness.hs"]
            , nodeOnFailure = Retry
            , nodeSplit = Nothing
            , childPlans = []
            }
        , DevPlan
            { nodeName = "timeout-attribution"
            , nodeTask =
                "In the tidepool-agent crate: the codex adapter's per-cycle deadline currently surfaces as a request timeout that reads as backend unavailability (SessionError's `request {method} timed out after {timeout:?}` from src/backend/codex/process.rs, produced by `pump` when a whole TURN exceeds the driver's turn_timeout — see DEFAULT_TURN_TIMEOUT in driver.rs). Observed live: a worker mid-edit was reported as 'backend unavailable: request turn/start timed out'. Make the error attribution truthful: a turn-deadline expiry must be distinguishable from a request-level/protocol timeout, and its rendered message must say the turn exceeded its deadline (naming the configured duration) and that the worker may still have been running — not that the backend was unavailable. Keep the existing variant semantics for genuine request timeouts. Add or extend a unit test pinning the new message/variant. Stay inside tidepool-agent; follow existing error-type style in process.rs."
            , nodeChecks =
                [ "grep -rq 'deadline' tidepool-agent/src/backend/codex/"
                ]
            , nodeBoundary = ["tidepool-agent"]
            , nodeOnFailure = Retry
            , nodeSplit =
                Just
                  SplitSpec
                    { splitHints =
                        "Split into 2-3 sequential microtasks: first locate and reshape the error path (variant + rendering), then the unit test. Each microtask gets one or two cheap structural shell checks (grep class); cargo builds in this worktree are cold and slow, so prefer grep/test -s checks and leave compilation to the orchestrator's own gate."
                    , splitMaxTasks = 3
                    }
            , childPlans = []
            }
        ]
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 2, maxAgentCycles = 10, gateWiderThan = 4}

-- | The chore edit itself rides as uncommitted state in the dev tree.
choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = True
