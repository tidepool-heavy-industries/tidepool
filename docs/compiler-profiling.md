# Resident compiler profiling

## Choose the question and measurement

Start with an observed workload and a decision the profile could change. Retain
the request shape, relevant source, expected outcome, producer and workload
history. Trace the request through its production consumer before choosing a
probe; the slowest visible step may be waiting for work owned elsewhere.

| Observation or hypothesis | Discriminating evidence |
|---|---|
| Wall time grows without similar worker CPU growth | Admission and phase timing, IO or wait evidence, and cgroup pressure; include work outside the sampled worker before blaming its code |
| A phase repeatedly consumes CPU | Leaf samples qualified by invocation and phase, plus exact symbols; use a controlled change to test the suspected algorithm or repeated work |
| Allocation grows while apparent retained size stays stable | Allocation and GC counters with their collection epochs; distinguish transient churn, retained live data and process RSS |
| Warm requests improve but cold requests do not | Matched producer-specific cohorts and explicit warmup histories; separate setup, compilation and reuse |
| Identical content is repeatedly read or hashed | Total versus unique bytes and invocation identity; check required validation and invalidation before proposing memoization |

Use cost decomposition and Amdahl's law to prioritize: reducing one phase has
limited end-to-end value when other work dominates. Investigate retention and
throughput as well as latency; a faster request can retain more memory or shift
cost to the next one. Start with existing phase evidence, then collect the
smallest additional measurement that distinguishes competing explanations.

## Measure host preparation and compiler residence

The `exomonad_harness::timing` debug target includes nested spans for
`cell_program.prepare`, `products.cell_program`, `products.seal`,
`exact.program_context`, and `exact.compiler_inputs`. Retain span NEW and CLOSE
events along with their parent context. These spans cover early errors as well
as successful returns. Their durations are inclusive: take interval unions or
subtract child intervals; do not add them to the existing `turn stage` rows.
In particular, context preparation before materialization and sealing after
`products.target_admission` have their own spans.

`compiler_transaction.scope` measures the host owner, including work before
worker admission. Use `compiler_endpoint.bind`, `compiler_transaction.begin`,
the existing admitted event, ordered `compile_request` spans, and
`compiler_transaction.close` to separate acquisition, submitted requests, host
work between requests, and END/retirement. A scope by itself does not prove a
worker was acquired. Endpoint submissions to a daemon are not physical worker
execution; retain the daemon's correlated events too. Bounded stderr excerpts
can omit compiler phases, so report that coverage separately from complete host
span capture.

Startup work uses the production `compile_root`, `workspace_toolsets_prepare`,
and `actor_application_prepare` spans. The last carries the exact actor and
actor path; the canonical path `root` distinguishes root activation from child installation.
`child_launch` carries its parent actor and includes workspace admission,
custody transfer and waiting for the child's startup. These durations overlap
the child's own preparation. Shared toolset preparation retains the first
issuer's span and tracing dispatcher across its async and blocking tasks;
another waiter does not issue another physical compilation. Attribute startup
requests using that ancestry and the exact daemon request identity, never by
matching counts or assuming everything within a host's lifetime is startup.

## Prepare matched producers

Use a separate compiler producer and fresh request cohort. Debug information
changes worker bytes and therefore its producer identity: neither old exact
scopes nor a private compilation cache from another producer can be reused.
Keep the live compiler and its caches running unchanged.

The native `//bridge/haskell:tidepool_extract_bin_profile` target uses the same
selected native optimization profile as `tidepool_extract_bin`. It adds `-g3`,
`-fexpose-internal-symbols` and `-finfo-table-map` to both the internal library
and executable. Installed Nix libraries retain their own existing symbols;
this target does not rebuild those dependencies. Default `fast-dev` currently
has no Haskell optimization flag. Select the same profile explicitly for both
sides of a comparison; do not compare a debug `fast-dev` worker to an `-O2`
production worker and attribute the difference to a source repair.

Build inside the admitted build environment, after checking the checkout's
`buck-out` mount and materializing its declared closure:

```sh
swarm-build bash scripts/buck2-run.sh build --local-only -c remote.enabled=false \
  //bridge/haskell:tidepool_extract_bin_profile \
  --build-report target/compiler-profile-build.json
```

Freeze the worker output and its matched frontend together in a private run
directory. With `TIDEPOOL_EXTRACT_WORKER` selecting that worker and the matched
GHC libdir environment, generate a deployment manifest through the frontend's
existing `--compiler-deployment-manifest PATH` command. Record source revision,
any dirty source hashes, build report, compiler flags and binary hashes. Start
a privately owned daemon with one worker, a fresh socket and cache directory,
and `TIDEPOOL_TIMING=1` set **before startup**. Warm it only with requests created
by that producer. Record the exact commands and warmup requests. Changing an
environment variable in a client does not change a resident worker environment.

For before/after comparisons, construct equivalent requests separately under
each matched producer. Keep optimization profile, workload semantics, warmup,
resource admission and observation settings comparable; record host contention
and repeat observations when variation could change the conclusion. A source
change that removes a dependency is a structural saving; quantify its runtime
benefit separately. Report instrumentation overhead or altered forcing when it
could affect the behavior under investigation.

## Capture the request

Find the private worker PID from the daemon's process tree, verify ownership,
and capture one explicit request command:

```sh
python3 scripts/profile-compiler.py \
  --pid WORKER_PID --output target/compiler-profile-capture \
  --perf /path/to/pinned/perf --duration 120 \
  --timing-log /path/to/private/compiler.jsonl \
  --worker-build-identity /path/to/frozen/build-identity.json \
  --request-id REQUEST_ID \
  --retain-file /path/to/immutable/request-manifest \
  --retain-file /path/to/immutable/execution-graph \
  -- /path/to/request-command ARGUMENTS
```

The request command must target that same private daemon. The script accepts a
same-user PID and fences PID reuse by process start ticks. It waits for perf's
explicit enable acknowledgement before launching the command, records UTC and
`CLOCK_MONOTONIC` anchors, and samples RSS from `/proc` every 50 ms. Recording
uses userspace `cpu-clock` at 199 Hz, no inherited tasks and no call graph.
Perf uses eight ring-buffer pages per CPU by default. `--mmap-pages` selects
a power of two between one and 1024. Smaller buffers reduce locked-memory
demand for parallel captures; report lost samples rather than assuming that
the smaller buffer is sufficient. An mmap allocation failure is a capture
failure, not evidence that the compiler request failed.
Pass no command to capture externally submitted work for a bounded duration.
The script owns and terminates its request command on timeout or output overflow.
If sampling ends early, including when a worker rotates, the command can finish
within its original deadline. The capture remains incomplete and no replacement
worker is sampled. Startup failures prevent command launch. The script never
terminates the selected worker or changes OS settings.

Evidence is private (directory mode 0700 and files 0600). Perf data is bounded
to 64 MiB and the capture to five minutes. Derived perf text is bounded to
16 MiB per file; overflow leaves the raw recording, reports incomplete analysis,
and exits unsuccessfully. Timing-log tails retain at most 16 MiB, including a
bounded prefix with explicit omitted bytes on overflow. The summary retains at
most 8192 completed spans and 256 invocation/phase groups, and is itself bounded
to 16 MiB. Any omitted spans, groups, or summary details make phase analysis
incomplete. RSS sampling intervals cannot be shorter than 10 ms. Command output and each opt-in input
are retained up to four MiB; oversize output ends capture and retains a bounded
prefix. The input set is limited to eight MiB. Immutable files must still exist
when capture begins. Successful browser requests currently clean up their
request-local scopes; `--retain-file` alone cannot recover those files afterward.
State unavailable exact input evidence explicitly. Do not substitute an unrelated
scope or snapshot for the measured request.

## Read the evidence

For stage-specific reuse decisions and cache-disable controls, use the
[reuse evidence report](compiler-reuse-evidence.md). Latency alone does not
establish compile reuse; absent stage completion remains unknown.

The profiling capture also consumes that same schema-1 reuse validator in
`summary.json` under `compile_reuse`, with a readable `request-report.txt`.
It preserves physical requests (epoch, PID, admission and ordinal), including
repeated executions of the same input digest. Worker cycles retain separate
stage decisions and rebuild reasons; request stage aggregates remain separate
from those cycle views. Missing, duplicate or nonfinal stage completion is
`UNKNOWN`, with null counts. A cancelled, failed or unterminated request cannot
qualify a frontend work total, even if an earlier cycle completed. Successful
output alone supplies no reuse evidence. Capture truncation and recovery errors
remain report errors alongside any individually completed requests.
Legacy count and flat-timer aggregates require one successful request terminal
and diagnostic rows strictly between the unique start and that terminal. Before
start, after terminal, missing-terminal and duplicate-terminal histories leave
these quantities unknown. The shared reuse report retains indexed raw legacy
observations and their boundary status; it does not present an incomplete
subtotal as a full request count.

`source_frontend_work_items` counts actual completed stage work items, not a
whole-request cache status. `activation_preview_frontends` is a separate
observed counter when its producer emits it. Executable selection counters
(`candidate_executable_required` and `exact_execution_original_load_owners`)
describe selected requirements/owners. Completed `retained_source_bytecode`
and `retained_finalized_bytecode` timers describe reconstruction operations;
they neither count all bytecode modules nor prove linkage. `actually_linked`
is null/`UNKNOWN`: the current diagnostic grammar has no owning linkage event.
Missing reconstruction observations are likewise unknown, not zero. Reports
never equate selected executable owners with linked or demanded bytecode.

Each request lists completed resource spans with their parent, monotonic
boundaries and available process CPU, RTS allocation and GC counters. It does
not add child counters to parent counters or manufacture request resource
totals. Explicit `unavailable` RTS counters and GC gauges become null in JSON
and `UNKNOWN` in the readable counter view while available wall/CPU measurements
are retained. Required timing fields still reject nonnumeric values.
Missing RTS fields are unknown; allocation counters can lag until a GC
accounting boundary. Request RSS observations select worker samples inside the
union of its completed resource spans, count each sample once and explicitly
describe that scope. Their sampled peak is not the full request peak, retained
heap size, or memory allocated by that request. Missing samples leave the peak
unknown. Capture-wide RSS and shared ancestor-cgroup context stay separate.
Raw log and RSS artifacts retain the underlying evidence. The JSON report binds
its retained timing input by path and SHA-256. Detail overflow reports omissions
instead of returning a complete accounting claim.

`capture.json` records the exact command, process identity, worker hash, request
linkage, clock anchors, exit status and errors. Optional `--worker-build-identity` binds
source provenance to the sampled worker hash; the current checkout OID is
recorded separately and does not identify an already running worker. `summary.json` records samples,
lost samples, unknown leaf symbols, RSS samples, and CPU samples overlapping
completed instrumented spans. JSON and text daemon trace envelopes preserve
request digest, worker PID, daemon epoch, admission ID, request ordinal and
transaction identity; rows with another worker PID are excluded. The quoted
text diagnostic is decoded separately from its envelope, and malformed numeric
fields are rejected and counted. Bare stderr without a PID
is explicitly marked as assuming the selected worker. Operator `--request-id`
labels remain distinct from observed compiler request IDs. `samples.txt`, `report.txt`, `cpu.perf.data`,
`rss.jsonl` and the selected log suffix retain the underlying evidence. Timing
log replacement or truncation is reported, rather than silently accepted.

`capture.json` also records the selected worker's unified cgroup path and
bounded start/end snapshots for up to 16 ancestors. The snapshots read
`cpu.stat`, `memory.events`, current/max/high memory values, and CPU, memory,
and IO pressure when those files are available. Cumulative CPU, event and
pressure `total` counters include deltas; memory values and PSI averages remain
start/end gauges. A counter decrease is reported as a reset. Missing,
unreadable, oversized or disappeared cgroup data is retained as an explicit
status. If the worker's unified path changes, deltas are omitted. Parent
cgroups are shared context and their counters cannot be attributed to this
worker or request; start/end snapshots can also miss brief pressure spikes.
Offline reanalysis retains these original snapshots and does not resample the
cgroup.

`phase_leaf_groups` reports up to eight leaf symbols and DSOs for each qualified
invocation and phase, plus unknown, unparsed and remaining sample counts.
Repeated spans of the same group use the union of their intervals so a CPU
sample is counted once within that group. Nested groups still overlap and are
not additive. Request digests identify inputs; admission IDs and ordinals
distinguish separate executions of the same inputs.

## Recover phase analysis

Recover phase analysis from an explicitly supplied retained daemon log into a
new directory, without changing the original capture or rerunning its workload:

```sh
python3 scripts/profile-compiler.py \
  --reanalyze target/original-capture --output target/recovered-analysis \
  --timing-log /path/to/retained/compiler.jsonl --perf /path/to/pinned/perf
```

Recovery scans at most 64 MiB of that log and selects worker invocations with
completed spans intersecting the original monotonic sampling window. It retains
at most 16 MiB of their diagnostics, records the scanned source hash and bounds,
and reports source changes, invalid records or omissions as incomplete. Daemon
diagnostics can be flushed after their spans end; log timestamps alone do not
establish CPU overlap. Recovery retains the original workload outcome and
measurement anchors. It cannot recover spans that the worker never completed.
`recovery_complete` and `recovery_errors` describe this recovery separately from
the original capture's timing-log error, retained in `original-capture.json`.
`phase_analysis_complete` also checks parsed records and report bounds. A span
extending outside the recorded window is marked as having partial CPU sampling;
its resource deltas still cover its complete span.

## Interpret phases, allocation and samples

Existing `tidepool-timing-detail` diagnostics now include monotonic start/end,
process CPU, allocation and GC deltas. Nested phases overlap; never add children
to their parent. Flat phases measured through `timePhase` have a corresponding
resource detail. GHC setup and dependency loading have explicit resource spans;
their existing flat timings differ slightly because sampling adds overhead.
Per-module `typecheck` spans include parsing, classification, transformation,
typechecking and family validation. `checked_typecheck` spans cover that work
only when a checked candidate needs fresh typechecking; reused candidates do
not emit a synthetic span. Planning between setup and loading remains outside
those spans. Product construction also records per-module interface serialization,
product and package-bundle encoding, dependency-evidence construction, certificate
input reads, and certification. Certification includes encoding and the output
write so lazy serialization is charged to that phase. These spans nest inside
`module_products`; their times must not be added to the parent. Interface
sidecars and remaining digest work belong to the surrounding product-writing
span, rather than the individual interface span. Other manually accumulated flat phases have no inferred resource
span. An action that throws before completion has no completed span; missing
phases are unmeasured, not zero. `allocated_bytes` follows RTS accounting and
can lag until GC; these counters do not force collection. `major_gcs` and
`minor_gcs` distinguish collection counts, but do not split GC CPU by generation.
`last_gc_*_before` and `last_gc_*_after` are snapshots of the last completed
collection, not instantaneous heap measurements. Equal epochs mean no newer
collection; epoch zero has no heap details. A minor collection's live estimate
includes uncollected generations. `process_highwater_*` describes process-lifetime
RTS maxima, not phase peaks or native RSS. The live maximum updates at major
collections. Keep these gauges separate from the sampled RSS and cgroup limits.
Enabled package measurements force the validation verdict and digests before
closing the span, so lazy hashing is charged to its owner. This diagnostic
forcing can change evaluation order on rejection paths. Hash byte counters
identify contents and purpose; their total and unique byte counts distinguish
repeated verification. Exact-scope counters count successful hash checks.
When a counter includes `count_ns`, `phase_hash_groups` attributes its emission
to containing intervals with the same invocation identity and reports total
versus unique content bytes. Nested groups overlap. The timestamp locates the
count event, not the duration of hashing. Older untimestamped counters remain
request-only, with their count reported explicitly; log order is not used to
guess a phase.
Interface hydration retains GHC's lazy knot: its timer covers immediate
construction, not all later evaluation of interface details.
Source-free declaration operations additionally emit `declaration_join` spans
for export and inventory observations, package visibility, selected
instance and retained-family consistency, fingerprints, interface writing,
package sealing and artifact revalidation. Enabled measurements force their
scalar observations and rejection decisions within the owning phase; they do
not deep-force `HscEnv` or the hydrated interface knot. This can move lazy work
earlier on instrumented rejection paths. The `build_interface` envelope includes
its nested phases, whose times must not be added to it. Source-free helpers
borrow the resident compiler's fixed package facts through its guarded scope;
they clear home visibility without creating a second EPS or unloading code.
GC CPU is aggregate CPU and GC elapsed is wall time. Neither can be subtracted
from another differently scoped measurement to manufacture mutator time.
Sampling RSS can miss a brief peak; `/proc`'s VmHWM is a process lifetime high
water mark, not necessarily the measured request's peak.

Use the recorded leaf instruction address with the frozen worker's `nm`,
`objdump` and `addr2line`/DWARF records to establish attribution. Large offsets
from a nearest exported symbol are not caller evidence. Haskell tail calls do
not preserve ordinary frame-pointer call chains. This recipe deliberately
reports leaf CPU sample share, not caller time or wall time. DWARF sampling can
be a separate experiment, with its unwinding failures reported explicitly.
Installed libraries without useful debug information remain unattributed at
source level. A zero-event capture, unrecognized loss record, command failure,
missing log or incomplete phase evidence must remain visible in the report.

## Test sensitivity and deliver the comparison

Before using absent samples or spans to rule out a cost, establish that the
capture covered the intended worker and request, that the operation ran, and
that the observation can resolve it. A known CPU-active request on the private
worker can check sample collection; it cannot prove that every short phase is
measurable. Missing symbols, worker rotation, lost samples, incomplete spans and
short observation intervals limit negative conclusions. Improve the capture or
report the blind spot rather than attributing it to a fast or idle compiler.

Deliver the decision, matched input and producer identities, workload outcomes,
measurement settings, absolute measurements and variability, and links to the
retained capture. State which explanation the comparison supports, plausible
alternatives it leaves open, and the next discriminator if needed. Reuse the
retained recording for new analysis; collect another workload only when the
existing evidence cannot answer the question.
