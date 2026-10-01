# Real embedded Engine and Store gates

These fixtures inject deterministic provider responses into the production
embedded host. Haskell runs through the resident compiler and native machine;
the fixture never supplies Haskell results or replaces the actor supervisor.
No live provider credentials are used.

| Gate | Exact libtest name | What it proves |
|---|---|---|
| M1 browser | `actor_host::m1_host_tests::production_browser_executes_resident_haskell_retries_and_controls_root` | Playwright input and real raw Haskell output, request-pinned reload/retry, interruption of the active operation, continue and root retirement. |
| Complete-cell preflight | `actor_host::embedded_captured_unfold_tests::admitted_cell_late_type_error_has_no_effect_or_publication_on_retry` | An installed AgentSpec and admitted root reject a final type error before the first notification effect or binding publication; exact operation retry retains rejection without compiler work. |
| Warm production cells | `actor_host::m1_host_tests::warm_cell_performance::production_engine_store_warm_display_cells_50` | Fifty source-backed displays across ten workloads through the production HTTP/Engine/Store host, exact original operations and exclusive daemon request attribution. |
| M2 captured replies | `actor_host::embedded_captured_unfold_tests::embedded_captured_unfold_awaits_two_child_replies_before_parent_call_returns` | Two independent captured children reply while the parent call is unfinished; release refuses new use while admitted children retain their scope. |
| M2 failure and reuse | `actor_host::embedded_captured_unfold_tests::embedded_captured_children_and_capture_survive_failure_of_the_unfinished_parent_cell` | Both children reply, the same parent cell fails, both children still read its completed private prefix, and a third child uses the retained checkpoint independently. |

The M2 parent call first completes `capturedValue <- pure (x :: Int)` and a
`capturedGetter` declaration before creating its checkpoint. The children read
these new private names, not just the earlier published `x` and `getX`.
The scripted child turns remain held until both branches are observed with the
original exact Store claim pending. Real Haskell `respond capturedGetter`
settles each typed reply; the parent awaits the pair `Right (42, 42)`.
Every observed operation retains the Engine's request ID, conversation identity
and provider call ID, and is checked against its recorded provider response.
Child history contains earlier provider provenance and excludes the current
unfinished parent call.

The failure gate raises an authored Haskell error after those replies. It fails
the parent **cell**, leaving its actor alive, and therefore retains the normal
`ParentOwned` child lifetime. Changing the children to `SwarmOwned` would test a
different contract. Their subsequent real reads must return `(41, 42)`, then the
checkpoint retained in the seed-store actor admits a third reader after the
failed original operation has durably settled. Releasing that checkpoint twice
must succeed. Issuer actor retirement and final native reader reclamation have
separate native-owner gates; these tests do not substitute for those proofs.

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

Run the frozen libtest with the existing bounded runner, in an admitted scope:

```sh
python3 build/rust/isolated-libtest.py "$FACADE_TEST_BINARY" \
  --exact actor_host::m1_host_tests::production_browser_executes_resident_haskell_retries_and_controls_root \
  --expected-count 1 --ignored --jobs 1 --timeout 900

python3 build/rust/isolated-libtest.py "$FACADE_TEST_BINARY" \
  --exact actor_host::embedded_captured_unfold_tests::admitted_cell_late_type_error_has_no_effect_or_publication_on_retry \
  --expected-count 1 --jobs 1 --timeout 600

python3 build/rust/isolated-libtest.py "$FACADE_TEST_BINARY" \
  --exact actor_host::embedded_captured_unfold_tests::embedded_captured_unfold_awaits_two_child_replies_before_parent_call_returns \
  --exact actor_host::embedded_captured_unfold_tests::embedded_captured_children_and_capture_survive_failure_of_the_unfinished_parent_cell \
  --expected-count 2 --jobs 1 --timeout 600
```

Keep each test's actual outcome, executed count, elapsed boundaries and cleanup
separate from compilation and discovery. The amended private-prefix fixtures
still require execution on the joined compiler/actor candidate; this guide is
not an acceptance report. The retained browser native failure on `32bd98b7b`
preceded Sleep and supplies no passing cancellation evidence. Its older compiler
pair cannot validate the current whole-cell protocol or deployment authority.

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
Retain both streams and the complete trace for the owning reporter. The fixture
has only been compile checked until a matching real-worker execution is retained.

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
