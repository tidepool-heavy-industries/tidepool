# Compiler reuse evidence

Reuse is a decision at an owning stage, not a synonym for a fast request.
Enable `TIDEPOOL_TIMING=1` before starting a private matched daemon and worker.
Retain the JSON trace with span context, request start/terminal rows, and raw
`tidepool-reuse` lines. Existing latency qualification and source/binary provenance
remain separate requirements; this reporter starts no services and changes no caches.

`Tidepool.Timing.emitReuse` owns the worker grammar. Rust emitters use the same
JSON shape through their existing tracing owner. Each diagnostic line is
`tidepool-reuse ` followed by one JSON object:

```json
{"schema":1,"cycle":7,"purpose":"cell_program","stage":"source_frontend","decision":"hit","reason":"matched","unit":"main","module":"Support","version_kind":"source_fingerprint","version":"actual-owner-fingerprint","items":1,"bytes":null,"observed_ns":12345}
```

Stage tags are `source_frontend`, `interface`, `finalized_core`, `prepared_body`,
`site_witness`, `original_recovery`, `raw_projection`, `artifact_reference`,
`artifact_transfer`, and `native_image`. Decisions are `hit`, `miss`, `work`,
`disabled`, `evicted`, `epoch_rotated`, and `complete`. Reasons are `matched`,
`absent`, `changed_source`, `changed_dependency`, `changed_authority`, `th_fresh`,
`epoch`, `recovery`, `cache_disabled`, `evicted`, and `stage_complete`.
Emit reasons where the owner makes the decision. A miss is not work: emit actual
frontend/lowering/preparation work separately when it occurs.

`version` is an opaque owning seal or fingerprint, never a rendered type or graph.
`version_kind` names `source_fingerprint`, `canonical_seal`, `interface_fingerprint`,
`prepared_identity`, or `image_identity`. An interface fingerprint does not prove
body equality; a process-local identity does not establish portable authority.
Do not describe a collision-prone stable-name hash as a unique body seal. Use
null for all four module fields on request-wide events.
Image-wide decisions may instead use null unit/module with `image_identity`
and the actual image key; `ReuseImage` renders that shape without fabricated owners.
Counts are nonnegative actual items; bytes are actual accounted reads/transfers,
or null when unaccounted.
Reference reuse and retransferred bytes remain separate stages; required validation
reads must not be labeled transfer avoidance.

Call `emitReuseComplete` after each instrumented stage for every worker cycle,
including zero work. It emits `complete`/`stage_complete`, zero items, null bytes
and null module fields. A stage without that completion is **UNKNOWN**, not zero.
Completion does not manufacture hit/work events or qualify a whole compile.
Counters should be cheap; per-owner detail remains opt-in through the existing
timing switch. There is no telemetry registry or independent validity identifier.

The physical trace envelope supplies daemon epoch, worker PID, admission ID,
request ordinal and compile-request digest. The digest identifies payload bytes,
not a unique invocation. The existing admission identifies the owning transport
transaction; its `transaction` field records whether it spans requests. Worker
`cycle` and `purpose` come from the actual compile owner and pass through completion
inputs. They are observations and never cache validity. Missing linkage remains
incomplete; the reporter does not guess a worker or join by digest alone.

```sh
python3 scripts/compiler-reuse-report.py --trace /absolute/compiler.jsonl \
  --output /absolute/reuse.json --require-stage source_frontend \
  --require-stage prepared_body --workload /absolute/workload.json
```

The report retains each raw parsed event and source row, per-request stage counts,
accounted bytes, reasons, purpose/cycle, service time, admission queue time and
flat phase totals. Input trace path and SHA bind retained raw details. Legacy
counts and compile summaries remain diagnostic data, never substitutes for a
completed stage. Timers lacking interval boundaries (including legacy lowering)
are preserved as nonexclusive totals; do not sum overlapping phases. Queue time
belongs to admission and is not additive across requests sharing a transaction.

`resident-performance-report.py` also includes `compile_reuse`; repeat
`--require-reuse-stage STAGE` to make missing stage evidence fail its gate. No
reuse claim can be recovered from a historical elapsed-only sample.

The owning next-run fixture must execute distinct cells, repeated identical cells,
binding growth, and A–B–A on one private worker/epoch, asserting actual semantic
results. Preserve the actual authored inputs. Its workload map uses schema 1:

```json
{"schema":1,"cases":[{"name":"repeat0","scenario":"repeat","step":0,"identity":{"daemon_epoch":"actual-epoch","worker_pid":123,"admission_id":4,"request_ordinal":1},"source":{"path":"/absolute/retained-cell.hs","sha256":"actual-source-sha256"}}],"controls":[{"stage":"source_frontend","normal":"normal-source","disabled":"disabled-source"},{"stage":"prepared_body","normal":"normal-body","disabled":"disabled-body"}]}
```

The example shows shape, not passing evidence. At least two distinct/repeat/growth
cases and exactly A, B, A are required. Each case links a different actual physical
request; source references are checked. This validates input relationships, not
binding semantics or proof that a retained file was submitted: the owning runner
must capture inputs at submission and retain its semantic assertions.

Use actual owner-provided source/body disable controls, matched producer and
workload, with retained startup configuration. Each control must show a normal
stage hit, an actual disable decision, matching module/version roster and authored
input, and **nonzero increased actual work**. Lower latency, a changed miss count,
or an absent work event cannot pass. This reporter checks observation controls,
not native execution qualification; Python regression fixtures are synthetic
parser controls and are never reported as successful live compiles.
