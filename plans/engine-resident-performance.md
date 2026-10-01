# Resident production performance evidence

The delivery targets in `engine-compiled-cell-delivery.md` require the packaged
host/Engine/Store route. A direct worker, the historical NO_DAEMON scaling
fixture, and a private-session benchmark do not establish these targets.

Prioritize removing unnecessary work before tuning or caching it. A candidate
must identify the production consumer, the work that disappears, and the
authority or ownership checks that remain. Report structural reductions
separately from measured latency improvements.

## Latest executed baseline

The resident two-test baseline passed on
`cf5d7244d24c896d867c389d41836fd2ef7bda63` (joined root `611b32438a` plus the
explicit daemon-argument override): two tests executed, 403 skipped, exit zero.
This used the Cargo debug profile and the frozen `1b874b9896` compiler pair after
checking that its producer inputs match this consumer revision. The compiler
manifest was regenerated for the selected paths. Evidence, exact commands and
binary hashes are retained under
`target/completion-evidence/compiled-cell/resident-611-two/`; the original
workspaces remain in the scheduling owner's checkout.

| Completed phase | Warm cell 0 | Warm cell 1 |
|---|---:|---:|
| Complete durable cell | 16.976 s | 18.944 s |
| Complete-cell compilation | 15.407 s | 17.484 s |
| Native observation | 178.551 ms | 184.436 ms |
| Native display | 220.842 ms | 187.479 ms |
| Metadata staging and file sync | 949.622 ms | 847.267 ms |
| Publication rename and directory sync | 0.920 ms | 0.454 ms |

Foundation publication took 121.862 seconds; the separate warm-up took
17.175 seconds. Both measured cells displayed 42 and committed durably. Their
v5 manifests are 137,212 and 137,377 bytes; the foundation manifest is 132,332
bytes. All 18 native item/display phase observations preserved the compiler
submission count. One owned worker completed all 11 compiler requests without
rotation, and the daemon and worker exited. The scope peaked at 8.911 GiB.

These are passing resident semantics and diagnostic timings, not acceptance of
the packaged Engine/Store route or its latency targets. Complete-cell compilation
dominates these two debug samples. The optimized, precompiled packaged candidate
must be measured before choosing further performance repairs.

Cold declaration recovery is still blocked. The frozen `1b874b9896` recovery run
completed original publication, then refused successor startup because identical
checked inputs received different bootstrap input identities. Native product
availability incorrectly influenced the package-interface identity. The compiler
owner is separating complete input proof from optional native output inventory;
the failed run and causal packet are retained in
`/tmp/tidepool-retained-package-witness/target/completion-evidence/cold-original-b53-exportfix/`.

## Current structural reductions

- Recovery publication: the retained v4 foundation snapshot at
  `baseline-488cf013c1-run2/workspaces/durable-workspace-9HdfEn/`
  is 12,663,202 bytes. Most graph rows repeat native dependency facts already
  present in sealed artifacts. Joined v5 persists interface selection
  and derives native requirements from verified artifacts, preserving exact
  lost-binding refusal. Focused validation includes authentic native markers and
  tampering after hydration. An earlier real foundation manifest was 388,062 bytes,
  with 157 interface rows; the following warm-up also published and displayed 42.
  That historical run failed later admission checks; the latest passing baseline
  and its compact manifest sizes are recorded above. A
  structural projection of that v4 snapshot is 132,000 compact bytes, compared
  with 4,220,736 compact v4 bytes. This is not a valid emitted v5 manifest or a
  measured latency result; see the retained size diagnostic under
  `target/completion-evidence/compiled-cell/recovery-v5-size-diagnostic.json`.
  Compact JSON is already joined; it removes formatting bytes, not graph work.
- Startup compilation: the admitted execution seals its output privately,
  then the previous runtime read and certified the same bundle again. Joined
  commits `d50d119202` and `2ce16592b4` hand off the original immutable decoded
  bundle, eliminating the second
  certification and cache publication. Mutating a public runtime bundle must
  still fail its compiler-issued proof check. Independent source review and ten
  focused checks passed. The real startup activation smoke also passed at
  `97e1bef3b0`: one executed test, 18.562 seconds, with compiler teardown confirmed.
  This does not establish cold declaration recovery or the full startup gate.
  `turn.cbor` still undergoes semantic decoding in both layers.
- Recovery hydration: joined v5 shares a verified artifact inventory
  between manifest admission and hydration instead of discarding and decoding
  it again. Separate later filesystem admissions must still detect tampering.
- Scheduler tests: joined retirement checks use an existing empty binding
  lease instead of compiling Haskell to exercise ownership alone. Native root
  retention remains covered by separate native tests.
- AgentSpec setup: joined code moves the resolved source-root inventory
  into its owner rather than cloning it through resolution and publication.
  Four focused resolver checks passed. Tool dispatch also borrows declaration
  shape instead of cloning its schema for every invocation; registration owns
  schema validation and retains typed errors.
  Joined reload validation now checks the current published source graph without
  producing a discarded driver. An unchanged run module alone still does not
  establish unchanged helper sources or resident compilation inputs.
- Immutable library startup: the package currently ships source text. The
  delivery plan now specifies a closed precompiled support cohort admitted by
  the existing GHC candidate mechanism, plus direct use of its pinned immutable
  source root. This removes repeated lowering/product emission and source copies
  when accepted. Implementation and real cold-start evidence remain pending.

These observations do not establish the production latency targets. The latest
resident baseline passes; production acceptance still requires the packaged route
and the specified sample counts.

## Production measurement

Use the existing Exomonad production launcher with this workspace configuration:

```toml
[compiler]
workers = 2
rss_ceiling_mb = 10240
```

Freeze the matched frontend, worker and host binary before the run; record their
SHA-256 values and source OID. Execute the exact production fixture command in
the approved build slice. Retain the command, exit status, actual execution
count, stdout/stderr, compiler `.log` and `.jsonl`, startup timings, process and
aggregate cgroup memory observations, and cleanup result. Do not clear caches.
A cold packaged start means a new owned host and compiler epoch, not a cleared
machine cache. A warm cell must use a worker that already served a real request;
first requests and requests after rotation are recorded separately.

The compiler's existing trace records boot producer, epoch and daemon PID. Each
completed request retains its correlation digest, worker PID/slot, prior served
count, rotation state, service time, output bytes and observed RSS. Queue timing
measures enqueue to worker service, including the acceptance handshake. Client
transaction admission timing also includes connection and any busy retries;
these boundaries overlap and must not be summed. Per-request observed RSS is
not a process high-water mark or aggregate peak. Retain cgroup/process sampling
for worker replacement overlap and aggregate peak evidence.

The production fixture emits one `resident-performance ` JSON line per sample:

```json
{"schema":1,"composition":"engine-store","kind":"warm_cell","index":0,"elapsed_ns":123000000,"completed":true,"displayed":true,"workload":"integer-addition","source_digest":"<SHA-256 of submitted cell>","daemon_epoch":"<boot epoch>","compiler_requests":["<request digest>"]}
```

`warm_cell` spans submission through returned display, including checking,
compilation, native execution and publication. Emit at least 50 samples with at
least ten varied workload labels. Every consumed compiler request must appear
in `compiler_requests`. `cold_start` spans the packaged process launch through
workspace readiness; five samples must have five distinct compiler epochs.
`cancel_ack` measures an interrupt of an effect proved active to its actual
acknowledgment; emit at least 50 samples. `cancel_cleanup` measures cleanup
separately. All samples carry schema, composition, kind, index, elapsed_ns,
completed and daemon_epoch. Cold and cancellation samples also carry actual
monotonic `started_ns` and `settled_ns`, whose difference must equal elapsed_ns.
Cold samples carry `packaged: true`, actual `host_pid`, `readiness:
"workspace-ready"`, `host_executable` and `host_sha256`. Cancellation samples carry the exact
unique `operation_id`, confirmed `effect_active_ns` at or before interrupt,
and `acknowledged: true` only when the actual acknowledgment arrives. Record
these in the owning fixture at the observed process/effect boundaries.
A sample may retain its submitted `source`; the reporter then recomputes its
UTF-8 SHA256 and refuses a mismatched `source_digest`.
Warm cells require at least ten distinct actual source digests and repeated
measured use of every participating worker PID. Private-session attribution uses a different
composition and cannot satisfy the product gate.

After the owning fixture and its compiler have stopped and flushed their logs,
write `manifest.json` containing `source_oid`, exact `command` (argv array),
`exit_code`, retained absolute `binary_path`, `binary_sha256`, `frontend_sha256`, `worker_sha256`, and the
configured/consumed `compiler_producer`. Then run:

```sh
python3 scripts/resident-performance-report.py \
  --samples /path/to/fixture.log \
  --compiler-trace /path/to/run-compiler.jsonl \
  --manifest /path/to/manifest.json \
  --output /path/to/report.json
```

Repeat `--compiler-trace` for cold-start traces. The report uses nearest-rank
p95, reports missing evidence and unmet targets independently, and exits nonzero
unless all three product targets have sufficient valid evidence. Warm cells must
match successful daemon completions with the configured producer, exact epoch
and actual worker PID, and show worker reuse. The report hashes the retained
frontend and worker paths named by actual daemon startup and compares them to
the frozen manifest; keep those selected files intact through reporting. Five cold starts each must be at
most 10 seconds; warm p95 must be at most 1 second; active cancellation ack p95
must be at most 250 milliseconds. Missing data is not a passing measurement.

## Compiler/native attribution fixture

After joining the complete worker producer and runtime consumer, compile the
runtime unit target before execution. The two-cell baseline and the 50-cell
measurement use the same helper. It prepares the complete cell before effects,
consumes sealed native/display products, and checks that native execution does
not submit any compiler requests. Every sample verifies actual display `42`.
The helper records private-session composition and cannot satisfy product gates.

With the matched frozen pair exported as `TIDEPOOL_EXTRACT` and
`TIDEPOOL_EXTRACT_WORKER`, and its configured deployment authority selected,
execute one exact ignored test through the existing battery owner:

```sh
TIDEPOOL_DAEMON_ARGS='--workers 2 --rss-ceiling-mb 10240' \
TIDEPOOL_KEEP_TEST_LOGS=1 NEXTEST_SUCCESS_OUTPUT=immediate \
  bash scripts/dev-shell.sh bash scripts/battery.sh \
    -p tidepool-runtime --lib --run-ignored all \
    -E 'test(=session::turn::scaling_tests::resident_display_cells_2_baseline)'
```

The 50-cell case is
`session::turn::scaling_tests::resident_warm_display_cells_50`. Confirm capacity
before expanding from the baseline. Retain warm-up separately and use the actual
daemon JSONL worker PID/served/rotation fields to decide which cells were warm;
a label alone is insufficient. Historical growing-prefix tests retain their
explicit direct-worker transport for a controlled comparison, while their
helper now prepares complete cells before effects. Any compiler submission in
native consumption fails the fixture before publication; separate publication
join compilation remains visible in its own measured phase.

The durable attribution baseline is the exact ignored runtime test
`session::turn::scaling_tests::resident_durable_display_cells_2_baseline`.
Use the same admitted battery command and frozen compiler pair above, changing
only the exact test filter. It attaches the existing recovery graph with a held exclusive reference-workspace run lock,
initializes the durable public scope, publishes one real foundation declaration,
warms the display path, and publishes two displayed cells. It retains the
workspace and each actual `declarations-<cell>.json` snapshot. A successful
sample requires `PublicManifestCommit::Durable`; partial directory sync is a
failure, not a completed timing row.

Pass this retained log as `--durable-samples` to the reporter. This section is
`unmeasured` when omitted and never contributes to the Engine/Store gate.
Retained v4 samples remain historical timing evidence; the current runtime
requires v5 for recovery. The reporter accepts those two known recorded formats
only when the sample agrees with the retained document, and refuses unknown
formats. Reporter validation does not admit a graph to the runtime.
`metadata_stage_file_sync_ns` includes the owning stage operation (metadata
encoding, integrity checks, staged file write and sync); the separate publication
phase includes rename and directory sync. Certification is separate and absent
for binding-only cells. Final manifest file size is measured, but checksum
encoding bytes, artifact hash bytes, write bytes and whole graph copies are
unknown until the owning code exposes those measurements. Diagnostic snapshot
copies and reads happen after the measured cell commit. This small fixture does
not establish B0/B100 scaling or restart recovery latency.

`scripts/resident-baseline.sh ABSOLUTE_NEW_EVIDENCE_DIRECTORY` runs exactly the
small multi-item consumption test and durable two-display baseline through the
existing battery owner. Enter the pinned dev shell and admitted resource scope,
set `CARGO_TARGET_DIR`, `TIDEPOOL_EXTRACT`, `TIDEPOOL_EXTRACT_WORKER`, and
`TIDEPOOL_COMPILER_DEPLOYMENT` to the frozen matching pair and manifest. The
runner records exact source OID, dirty paths, command argv, file SHA256 values,
exit code, exactly two executed tests, and scope memory peak. A missing summary
or a different execution count refuses the baseline. It uses two workers with 10240 MiB ceilings,
retains compiler/test logs, and verifies the selected files did not change.
It makes no packaged-product latency claim. The later durable scaling tests
`resident_durable_growing_prefix_10_baseline_0` and
`resident_durable_growing_prefix_10_baseline_100` reuse the same actual-cell and
publication helper; they are opt-in and are not part of this baseline command.

The small complete-cell vertical also refuses a missing configured deployment
and a deliberately wrong producer before extraction count changes. It restores
the exact deployment with a scoped guard before the valid compile. These
refusal probes run only in that vertical correctness test; warm and durable
measurement cells do not change authority configuration.

Durable workspaces are created under the explicit retained
`TIDEPOOL_PERFORMANCE_WORKSPACE_ROOT`, supplied by the bounded runner outside
the dev shell's temporary directory. Keeping a child of the dev shell's scratch
directory alone does not survive that shell's cleanup. The battery owner copies
the compiler JSONL sidecar before daemon teardown as well as the readable log.
The initial baseline on `e9be683815` selected exactly two tests and failed both:
the valid multi-item program reached a worker error after 64 seconds
(`ordinary source candidates cannot accompany exact declaration owners`), and
the path-only durable graph lacked configured run authority. Its retained traces
are failure evidence, with no completed durable or warm measurements.

Additional complete-cell verticals share the same upfront compiler/consumer
helper. `late_record_selector_replaces_earlier_cell_value` checks that a later
record selector replaces a prior heap binding and displays 2.
`complete_cell_preserves_exact_local_fixity` is ignored while the local fixity
production guard remains; after its reviewed removal it must display 8.
`durable_mixed_originals_recover_independent_native_entry` is an opt-in durable
fixture: it publishes live x, independent y=42, then one original containing
independent=42 and dependent=x; asserts the persisted root dependency
classifications; and launches the already-built native test binary for one
exact fresh-process test. That process opens the owned retained graph, verifies
x has no live heap binding, and demands the mixed original's independent entry,
which must display 42. Demanding dependent must return the typed missing exact
retained owner. Rebinding the same-spelled x then demanding dependent again
must refuse the same identity and generation. Unexpected compiler or native
errors fail the test. The battery continues to own the one compiler daemon.
This fixture does not establish packaged host cold-start timing or instance
fallback behavior.

## Static work on cold compilation

The debug attribution run at `11beceefdd` selected frontend SHA256 `cee8dae…`
and worker `8e6bc527…`, exactly two tests, and failed both at Rust's temporary
support-graph admission. Its successful foundation publication took 114.034 s:
106.511 s complete-cell admission, 2.075 s adoption, 3.080 s certification,
1.993 s stage/file sync, and 5.068 ms rename/directory sync. These are distinct
boundaries; the compiler phases below nest inside admission and overlap.
This is not packaged cold-start evidence. The optimized packaged pair remains
unmeasured.

The foundation's worker request `b28c689d10d5204d` took 88.742 s. Its declaration
phase took 88.737 s, GHC load recorded two phases totalling 10.364 s, lowering
12.168 s, and 44 prepared-STG phases totalling 1.122 s. The complete-cell request
`4a897ffa413880ec` took 90.814 s and recorded three module-product phases totalling
56.369 s; native item phases totalled 61.408 s and display 21.139 s. Do not sum
these nested values. Its 60 exact interface reads/decode calls captured
3,564,668 bytes, with 243 recovery module preparations across 56 rounds. The
later warm-up recorded 138 interface reads/decode calls and captured 7,140,463
bytes; it still failed admission before execution, so it is not a successful
warm-cell sample.

Source inspection at package revision `2de99bbd36` finds source embedding,
not prepared support selection: facade `build.rs` embeds `.hs` text;
`haskell_sources::ensure_embedded_stdlib` materializes those sources; the actor
host supplies that directory as an include path. `build/package:compiler_deployment`
and `matched_runtime_bundle` package the worker, frontend, authority manifest,
shared native libraries, host and browser assets, but no certified prepared
stdlib product resource. The host's compiled driver is already shared with
children; there is no per-child driver compilation to remove.

The first candidate for removing work is a build-time certified stdlib/support
resource admitted through the existing artifact inventory and exact-scope owner.
On a fresh host, select those immutable products so GHC compiles the authored
wrapper and workspace source while omitting unchanged library lowering and
module-product emission. `Main.writeModuleProducts` currently visits every fresh
prepared module, projects its groups, writes and reads a temporary interface,
then encodes the module products and certification. A selected immutable base
would make those steps unnecessary for fixed modules; it must retain complete
interfaces, instances, source identity and exact native ownership, including
transitive home import edges.

Within a cell, avoid re-emitting the same certified support products for each
item and display, and construct each fresh GHC environment from admitted
immutable support. `GhcPipeline.runCompileCycle` explicitly disables mutable
memo/cache reuse and creates fresh exact state. Keep that isolation: sharing
certified bytes or immutable decoded products must not retain mutable EPS/HPT
state or broaden the selected graph to unrelated inventory. The ordinary startup
bundle's duplicated parse/certify/publish is a separate compiler-owner repair.
The counts above identify repeated work; none establishes a measured speedup.

The later `0092148c64` run still failed both selected tests: standalone admission
refused the selected graph, while the durable case displayed 42 and committed
its warm-up before the next planned admission hit a missing file. It selected
exactly two tests, exited 100, retained unchanged frozen binaries and recorded
an aggregate scope peak of 8,567,394,304 bytes; its daemon and workers exited.

The actual retained foundation snapshots provide storage evidence for v5's
native-edge removal. With the same frozen worker and authored foundation,
both snapshots contain 45 artifact descriptors and two nodes. V4 contains
12,303 dependency rows (12,146 native-group rows and 157 interface rows) and is
12,663,202 bytes. V5 persists only the 157 interface rows and is 388,062 bytes:
12,275,140 fewer bytes. Native relations are reconstructed by the owning v5
validator from certification. This is a measured file-size difference, not a
latency claim. The v5 warm-up manifest is 397,505 bytes; its successful private
cell took 18.773 s, including 734.924 ms stage/file sync and 617 microseconds
rename/directory sync. It does not satisfy a product performance target or make
the failed two-test baseline a pass.
