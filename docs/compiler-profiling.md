# Resident compiler profiling

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
buck2 build --local-only -c remote.enabled=false \
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
Pass no command to capture externally submitted work for a bounded duration.
The script owns and terminates its request command on timeout or capture failure;
it never terminates the selected worker or changes OS settings.

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

`phase_leaf_groups` reports up to eight leaf symbols and DSOs for each qualified
invocation and phase, plus unknown, unparsed and remaining sample counts.
Repeated spans of the same group use the union of their intervals so a CPU
sample is counted once within that group. Nested groups still overlap and are
not additive. Request digests identify inputs; admission IDs and ordinals
distinguish separate executions of the same inputs.

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

Existing `tidepool-timing-detail` diagnostics now include monotonic start/end,
process CPU, allocation and GC deltas. Nested phases overlap; never add children
to their parent. Flat phases measured through `timePhase` have a corresponding
resource detail. Manually accumulated flat phases have no inferred resource
span. An action that throws before completion has no completed span; missing
phases are unmeasured, not zero. `allocated_bytes` follows RTS accounting and
can lag until GC; these counters do not force collection or measure live heap.
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
