# Resident production performance evidence

The delivery targets in `engine-compiled-cell-delivery.md` require the packaged
host/Engine/Store route. A direct worker, the historical NO_DAEMON scaling
fixture, and a private-session benchmark do not establish these targets.

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
