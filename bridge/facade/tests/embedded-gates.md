# Real embedded Engine and Store gates

These fixtures inject deterministic provider responses into the production
embedded host. Haskell runs through the resident compiler and native machine;
the fixture never supplies Haskell results or replaces the actor supervisor.
No live provider credentials are used.

| Gate | Exact libtest name | Required check |
|---|---|---|
| M1 browser | `actor_host::m1_host_tests::production_browser_executes_resident_haskell_retries_and_controls_root` | Playwright input and real raw Haskell output, request-pinned reload/retry, interruption of the active operation, continue and root retirement. |
| Warm production cells | `actor_host::m1_host_tests::warm_cell_performance::production_engine_store_warm_display_cells_50` | Fifty source-backed displays across ten workloads through the production HTTP/Engine/Store host, exact original operations and exclusive daemon request attribution. |
| Active cancellation | `actor_host::m1_host_tests::cancel_performance::production_engine_store_active_cancellation_50` | Fifty production interrupts of exact armed native Sleep calls, actual retained owner acknowledgment and separate durable output/round cleanup timings. |

The frozen bundle descriptor owns the exact M2 case roster, count and per-case
deadlines. Run that cohort through the [package qualification owner](../../../build/package/README.md);
this guide describes the behaviors without duplicating its executable roster.

M2 provider scenarios install the real embedded service, admitted model factory,
conversation reader, Store and Scheduler before root admission. Only
`ResponsesTransport` is replaced. Every Haskell tool call runs through the
Engine's actual dispatcher, actor, compiler and native machine. The preflight
and checkpoint-release gates use the real admitted policy directly; they do not
claim provider-driven admission.

The captured-reply and failure scenarios complete private native bindings
`capturedValue` and `capturedGetter` before creating their checkpoint. Their
children read those names, rather than only the earlier published `x` and
`getX`. Both child provider requests remain held until the original exact Store
claim is observed pending. Real Haskell `respond capturedGetter` settles each
typed reply, and the parent awaits `Right (42, 42)`. Every observed operation
retains its Engine request ID, conversation identity and provider call ID and
is checked against its recorded provider response. Child history contains
previous provider provenance and excludes the unfinished parent call.

The success and interrupt scenarios use `InvocationOwned` children. The failure
scenario explicitly gives its original two children `ActorOwned` lifetime so
they can survive the creator invocation's failure. That failure retains `NotPublished Failed`: neither its
native prefix nor its declarations or unrun suffix become public. Earlier
`x` and `getX` remain readable; `capturedValue`, `capturedGetter`,
`privateCapturedHelper` and `capturedSuffix` remain absent. The parent then
publishes new bindings with values `(99, 100)`, while both retained children's
reads must still return `(41, 42)`.

Before failure, the creator transfers its checkpoint and original group handle
to typed record services installed by the earlier setup cell. After the failed
operation settles, a third child uses that checkpoint to evaluate
`privateCapturedHelper capturedValue` inside a fresh `CapturedReuseValue` and
reply `42`. This executes the retained helper instead of reading a memoized
getter. Checkpoint reuse checks double release and refusal of further admission.
Typed cleanup handles the third child and the original group; each known
child's retained cleanup outcome must identify its owner and be confirmed.

The nominal join holds A's `ActorOwned` children behind the same reply barrier while a
provider-issued B tool call publishes on the same root. B must settle with A's
exact original claim still pending. After releasing the children, A publishes
its original result. The final provider-issued Haskell probe checks both results
and the current B shadow, including the different original/current reply values.
This checks independent cell publication on one root actor.

The interrupt gate obtains the exact root identity and active round from the
real browser projection, then submits `HostCommand::Interrupt`. Its admitted
parent operation must retain `NotPublished Cancelled`; bare scheduler
cancellation is insufficient. Both invocation-owned children must settle with
confirmed cleanup, and the root must remain live and return to waiting without
an active round. A durable `HostCommand::Input` wakes the interrupted driver
while a queued scripted step supplies a new Haskell tool call. The resumed
provider request must contain that input, the new binding must commit, and the
cancelled prefix must remain absent. Typed group cleanup and ordinary final
shutdown follow independently of the interrupt.

M2 uses a named 300-second cold-debug settlement budget for its real setup,
parent cells, post-failure reader cells and checkpoint reuse. This semantic
budget includes whole-cell compilation, validation and native attachment;
it makes no speed claim. Typed child reply settlement keeps its 90-second
budget, provider branch/read observations keep 120 seconds, and host readiness
and termination keep 30 seconds. The descriptor applies a 600-second default
outer watchdog and 900 seconds to unfinished-parent survival, nominal
publication join, checkpoint release and the selected coding child. Optimized
performance acceptance remains separate, including its one-second p95 and
ten-second cold limits.

The preflight gate installs a real `AgentSpec`, then dispatches an authored cell
through its admitted root policy. The first bind would send an observable
notification and return `41`; its final statement is the real type error
`pure (True :: Int)`. Rejection must retain receipts with no committed item,
installed binding or completed operation. The notification inbox stays empty,
and a fresh Haskell lookup reports that `neverPublished` is not in scope.
Retrying the identical original operation returns the same retained rejection
without another compiler submission. A valid control changes only the final
statement, executes exactly one notification through the production durable
inbox/Store owner, and publishes the binding. This is admitted-actor evidence;
it does not claim a scripted provider drove that call.

## Matched inputs and execution

Build the current `//bridge/facade:tidepool_unit_tests` and its declared compiler,
stdlib and browser resources in the admitted root Buck lane. Use the native
no-Codex profile. Freeze the executable and all inputs before execution, retain
source and harness revisions plus hashes, and preserve shared daemons.

Set absolute paths for the frozen `TIDEPOOL_EXTRACT`,
`TIDEPOOL_EXTRACT_WORKER`, `TIDEPOOL_PRELUDE_DIR` and
`TIDEPOOL_COMPILER_DEPLOYMENT`. Generate the deployment manifest with the
selected frontend and worker; do not copy authority from a different pair:

```sh
"$TIDEPOOL_EXTRACT" --compiler-deployment-manifest "$TIDEPOOL_COMPILER_DEPLOYMENT"
```

Retain the manifest and set `TIDEPOOL_KEEP_TEST_LOGS=1`. M1 additionally needs
`EXOMONAD_EMBEDDED_ASSET_ROOT`, `TIDEPOOL_BROWSER_NODE`,
`TIDEPOOL_BROWSER_DRIVER`, and `PLAYWRIGHT_BROWSERS_PATH` from the same declared
browser closure. M2 supplies its own minimal local HTTP assets.

Run the frozen cohorts through the descriptor owner, which supplies the exact
roster, expected count, ignored status and per-case watchdogs to the bounded
runner. The descriptor and command forms are documented in the
[package guide](../../../build/package/README.md):

```sh
python3 "$(dirname -- "$DESCRIPTOR")/qualification.py" run "$DESCRIPTOR" \
  --cohort m1 --output "$M1_EVIDENCE"

python3 "$(dirname -- "$DESCRIPTOR")/qualification.py" run "$DESCRIPTOR" \
  --cohort m2 --output "$M2_EVIDENCE" --jobs 3 \
  --delegated-service --service-slice "$ADMITTED_USER_SLICE"
```

Use an admitted user slice whose checked resource bounds fit the selected
concurrency. A fresh live recursive delegation smoke is separate from this
finite deterministic cohort. Retain its actual descendant relationships,
replies and cleanup evidence independently.

Keep each test's actual outcome, nonzero executed count, elapsed boundaries and
cleanup separate from compilation and discovery. Replay the selected tests on
the joined compiler/actor revision and retain their evidence before reporting
acceptance. This guide specifies checks and commands; it records no execution
outcome.

## Warm production measurement

The ignored warm fixture reuses the M1 real host launcher and cleanup. It submits
one authenticated HTTP command; the request-aware deterministic provider emits
ten distinct real warm-up cells followed by fifty unique authored cells across
ten workload kinds. Every returned display must match its source fixture, and
its exact original operation, authored source and settled output must exist in
Store. No provider response supplies a tool result.

Set `TIDEPOOL_PERFORMANCE_COMPILER_TRACE` to the absolute retained JSONL path of
the exclusively assigned owned daemon, alongside its bound
`TIDEPOOL_EXTRACT_DAEMON_SOCKET` and the frozen compiler deployment inputs above.
The trace must contain one actual matching boot. A test-only observer reads the
existing client `compile_request` spans; an incremental byte cursor joins those
exact digests to actual daemon completions. Foreign requests, missing or repeated
completions, cold/rotated measured workers, or workers without repeated measured
use invalidate the campaign. Trace waits and Store checks occur after the latency
boundary, before admitting the next cell.

Timing runs from actual raw tool submission to its display in the Engine's
successor request. It includes production checking, compilation, execution,
publication and result return; it does not measure DOM painting. Warm-up lines
use `resident-performance-warmup`; fifty measured lines use the existing
`resident-performance` schema with `composition: engine-store`, actual source,
source SHA-256, original operation and every consumed compiler correlation.
Retain both streams and the complete trace for the owning reporter. Record
compile checks separately until a matching real-worker execution is retained.

```sh
python3 build/rust/isolated-libtest.py "$FACADE_TEST_BINARY" \
  --exact actor_host::m1_host_tests::warm_cell_performance::production_engine_store_warm_display_cells_50 \
  --expected-count 1 --ignored --jobs 1 --timeout 1800
```

Execute after the actual cell correctness gate passes. The warm fixture starts
no compiler daemon and never stops a shared daemon. The assigned runner retains
and stops its owned daemon using the existing lifecycle recipe, then runs
`scripts/resident-performance-report.py`. Cold-start and active cancellation
measurements remain separate fixtures; this warm run alone does not establish
all performance targets.

## Reporting retained measurements

After the owning runner and compiler have exited and flushed their logs, report
samples against the same frozen frontend, worker, host, and source revision:

```sh
python3 scripts/resident-performance-report.py \
  --samples /path/to/runner.log \
  --compiler-trace /path/to/compiler.jsonl \
  --manifest /path/to/manifest.json \
  --output /path/to/report.json
```

The runner log supplies `resident-performance` JSON records; the manifest binds
exact runner commands, executed counts, binary hashes, compiler producer, and
retained Rust/GHC build packets. Repeat `--compiler-trace` for separate compiler
epochs. The reporter validates those inputs and source provenance before counting
samples, reports missing evidence separately, and exits nonzero for incomplete
or unmet product gates. `--actor-samples`, `--durable-samples`, and
`--package-cold-report` supply independent retained evidence; private-session
measurements do not establish the Engine/Store product gate. Consult `--help`
and the owning reporter validators for the accepted schemas.

## Active cancellation measurement

The ignored cancellation fixture reuses the same production host and owned
compiler trace inputs as the warm fixture. A real displayed `42` warms the host.
Fifty authenticated HTTP inputs then cause the deterministic provider to issue
fifty distinct original Haskell calls, each containing the adjacent single-Sleep
fixture. Their source and qualified original request must be retained in Store.
No transport or endpoint fabricates a cancellation result.

Before each timed interval, the exact claim must be pending with no scheduler
output. `LocalActorRef::hosted_workbench_waiting` must observe that exact original
call's armed owner, and the production projection supplies its actual active
round. This observation is momentary; the fixture rechecks immediately before
sending `HostCommand::Interrupt` for that round. Compilation and readiness polls
finish before the timer starts.

The `cancel_ack` interval runs from HTTP interrupt submission to receipt of the
exact scheduler `CancelledWithReceipt` result and its retained
`CancellationAcknowledgment::StoppedWithReceipt`. The production dispatcher can
return that acknowledgment only after the original native cancellation owner
confirms abort.
The fixture awaits that real acknowledgment concurrently with the HTTP request;
HTTP `202` or a control-request receipt alone never qualifies. Every row retains
actual monotonic `started_ns`, `settled_ns`, `effect_active_ns`, the exact original
operation, execution identity and targeted round. It also asserts no compiler
submissions occurred in the measured interval.

A separate `cancel_cleanup` interval starts at the observed owner acknowledgment
and ends after the original cancelled output/claim is durable, the production
projection is waiting with no active round, the interrupt has its actual durable
control receipt, and the issuer actor remains live. Native abort confirmation is
already required by the acknowledgment boundary. This cleanup sample measures
subsequent durable/host work; it makes no independent claim about heap reclamation.
The fixture emits fifty rows of each kind and requires acknowledged final host
and resident-forest cleanup on success.

```sh
python3 build/rust/isolated-libtest.py "$FACADE_TEST_BINARY" \
  --exact actor_host::m1_host_tests::cancel_performance::production_engine_store_active_cancellation_50 \
  --expected-count 1 --ignored --jobs 1 --timeout 1800
```

Use this owning isolated process runner: it bounds the test and kills residual
process-group children on failure, timeout and completion. A failed assertion
cannot establish acknowledged in-process forest cleanup. Retain its nonzero
status and diagnostics, then stop only the assigned owned daemon through the
existing lifecycle recipe. Compile/list evidence is separate from an executed
fifty-sample campaign; actual execution waits for the real-cell correctness gate.
