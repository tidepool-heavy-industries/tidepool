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
choreGoal = "Give sandboxed workers real in-cycle Haskell typechecking: a repo script that drives tidepool-extract with the right includes, and a --connect client mode so a worker-started in-sandbox daemon makes repeat checks warm."

chorePlan :: DevPlan
chorePlan =
  DevPlan
    { nodeName = "worker-compile-service"
    , nodeTask = ""
    , nodeChecks =
        [ "test -x scripts/worker-typecheck.sh"
        , "grep -q 'connect' haskell/app/Main.hs"
        ]
    , nodeBoundary = ["scripts/worker-typecheck.sh", "haskell/app/Main.hs", "haskell/src/Tidepool/DaemonServer.hs", "harness-dogfooding/dev-tree/Prompts.hs"]
    , nodeTolerated = []
    , nodeOnFailure = AskOperator
    , nodeSplit = Nothing
    , childPlans =
        [ DevPlan
            { nodeName = "typecheck-script"
            , nodeTask =
                "Create scripts/worker-typecheck.sh (executable): a self-contained script a SANDBOXED codex worker runs to typecheck Haskell edits in-cycle. Contract: `scripts/worker-typecheck.sh FILE.hs [-- extra extract args]` resolves the extract binary ($TIDEPOOL_EXTRACT, hard error with a plain message if unset/unreadable), builds the include set — always the repo's haskell/lib, plus the file's own directory, plus (when the file's imports mention Tidepool.Effects) the NEWEST generated effects-module directory discoverable under the ambient cache (the content-addressed dirs the Rust engine mints; search ${XDG_CACHE_HOME:-$HOME/.cache}/tidepool*/ for dirs containing Tidepool/Effects.hs, newest mtime wins, say clearly when none is found) — and invokes the extract with --all-closed --target-module-only and an --output-dir under /tmp, forwarding diagnostics verbatim and exiting with the extract's code. Honor TIDEPOOL_EXTRACT_DAEMON_SOCKET if the extract grows daemon routing later, but do not depend on it. Keep it plain POSIX-ish bash matching scripts/ house style (set -euo pipefail, comments explaining WHY). Also add one sentence to the orchestratorChecksContract fragment in harness-dogfooding/dev-tree/Prompts.hs: workers editing Haskell SHOULD run scripts/worker-typecheck.sh on each edited file before finishing (replacing the your-shell-has-no-ghc sentence's do-not-attempt framing with do-it-via-the-script)."
            , nodeChecks =
                [ "test -x scripts/worker-typecheck.sh"
                , "bash -n scripts/worker-typecheck.sh"
                , "grep -q 'worker-typecheck' harness-dogfooding/dev-tree/Prompts.hs"
                ]
            , nodeBoundary = ["scripts/worker-typecheck.sh", "harness-dogfooding/dev-tree/Prompts.hs"]
            , nodeOnFailure = Retry
            , nodeSplit = Nothing
            , childPlans = []
            }
        , DevPlan
            { nodeName = "connect-shim"
            , nodeTask =
                "Add a daemon CLIENT mode to the extract binary: `tidepool-extract --connect <socket> <normal argv...>` sends (current working directory, the remaining argv) to a running compile daemon over its UNIX socket using the exact frame codec the daemon already speaks (haskell/src/Tidepool/DaemonServer.hs — encodeRequest/decodeResponse and the framing recvRequest expects), streams the response's stdout/stderr to the local stdout/stderr, and exits with the returned exit code. Home: haskell/app/Main.hs beside parseDaemonArgs, following its parser style (a --connect anywhere in argv splits client mode; everything after the socket path is the request argv, passed through verbatim). Export any needed codec helpers from Tidepool.DaemonServer rather than duplicating framing — one codec, one home. Connection failure is a hard, plainly-worded error (no silent fallback: the CALLER decides fallback). This enables a worker to start its OWN in-sandbox daemon ($TIDEPOOL_EXTRACT --daemon --socket .tidepool/extract.sock &) and get warm repeat checks; no server-side changes should be needed and none are in scope beyond exporting codec helpers."
            , nodeChecks =
                [ "grep -q 'connect' haskell/app/Main.hs"
                , "grep -qE 'encodeRequest|sendRequest' haskell/app/Main.hs"
                ]
            , nodeBoundary = ["haskell/app/Main.hs", "haskell/src/Tidepool/DaemonServer.hs"]
            , nodeOnFailure = Retry
            , nodeSplit = Nothing
            , childPlans = []
            }
        ]
    }

choreBudget :: Budget
choreBudget = Budget {maxDepth = 2, maxAgentCycles = 8, gateWiderThan = 4}

-- | Clean-tree protocol (operator, 2026-08-25): the chore config is
-- COMMITTED before launch, so runs fork from a real commit and fold back
-- by ordinary git merge.
choreSnapshotDirtySource :: Bool
choreSnapshotDirtySource = False
