# Turn-latency measurement contract

Measurement only. Nothing in this document proposes a fix; the attribution
report (`11-turn-latency-report.md`) ranks candidates, and a later wave picks
one.

## The pipeline one answerer round walks

`SelfHarnessDriver::drive_answerer_to_finalize` loops over
`Harness::drive_turn`. One iteration:

1. **provider call** — `Harness::stream_turn`: the model writes a reply,
   possibly containing a fenced Haskell block. Network + inference.
2. **classify** — `run_block` calls `tidepool_runtime::session::classify_turn`,
   which spawns `tidepool-extract --emit-stmt-binders` (parse-only lane, no
   Core pipeline — but still a full process start).
3. **template** — `engine::template_turn` wraps the block in a module with the
   effect row, pragmas, imports, and the node's `Lib.G<gen>` decl module.
4. **extract** — `compile::compile_turn` spawns `tidepool-extract` again, this
   time through the whole GHC pipeline, and it writes `<target>.cbor`,
   `meta.cbor`, `asks.json` into a tempdir.
5. **read + deserialize** — the three files are read and decoded into
   `CoreExpr`, `DataConTable`, `AsksSidecar`.
6. **jit codegen** — `ResidentSession::run` → `add_fragment_session`: Cranelift
   mints the fragment against the merged table.
7. **run** — `Threadless::run_fragment` on the eval thread, to completion or to
   a suspension (`finalize`, `askUser`, `fork`).

Steps 2 and 4 are two separate `tidepool-extract` process spawns per round.
A compile error at step 4 costs a full round: the driver pushes the diagnostic
back as a user turn and the loop restarts at step 1.

## Stage vocabulary and event shape

`tidepool-harness/src/timing.rs` is the single source of truth: the `STAGE_*`
constants, the `PHASE_*` constants, the [`record_stage`] emitter, and
`ExtractTiming::parse`. Read the module doc — it specifies the event fields and
the extract-side stderr grammar.

Two rules for anyone adding a call site:

- Emit through `timing::record_stage`, never a hand-written `debug!` — the
  bench collector matches on the exact event shape.
- Stages are flat. `extract.*` stages are the inside of `extract_spawn`; a
  collector picks one granularity, and summing across both double-counts.

## Extract-side timing is env-gated and diagnostic-only

`TIDEPOOL_TIMING=1` makes `tidepool-extract` write
`tidepool-timing phase=<name> ms=<int>` lines to STDERR. Unset, it writes
nothing. stdout (the JSON diagnostics report) and every emitted file stay
byte-identical either way — the wire format does not move.

## Measuring without burning model tokens

The bench drives the production path (`Harness::run_block` /
`SelfHarnessDriver::run_one_cycle`) with a `replay::ReplayProvider` supplying
the assistant replies, so the compile/run stages are real and only the provider
call is substituted. `provider_call` is therefore ~0 under the bench and its
real cost has to be read off a live run's logs, not the bench summary.
