# S1 parked-registry property findings (bug map)

Property suite: `tidepool-codegen/tests/proptest_parked_registry.rs`.
Date: 2026-06-10. Workstream S1 (ParkedStream registry unit angle —
complements W4's end-to-end campaign and W5's dispatch-loop differential).

## Method

Two tracks, both driving the REAL registry code in
`tidepool-codegen/src/host_fns.rs:1989-2499`:

- **Track A (no Cranelift at all):** `heap_force` is `pub`; thunks are
  hand-built in a raw buffer with **our own** `extern "C"` host functions as
  code pointers (the same calling contract `stream_chunk`/`stream_element`
  use). This unit-drives the memoization mechanism element thunks rely on.
- **Track B (thinnest compiled shim):** hand-built `CoreExpr` trees (NO
  Haskell) compiled with `JitEffectMachine`, with handlers delivering
  **custom adversarial `ValueSource` implementations** through the pub
  `ValueStream::from_source` escape hatch. Every park / chunk pull / element
  force / teardown executes the real `park_stream` → `stream_chunk` →
  `stream_element` → `RegistryGuard` code.

Observability without touching internals:

- **Producer call-counters** (`next_calls` / `get_calls` / `len_calls` via an
  `InstrumentedSource` wrapper) quantify laziness EXACTLY.
- **`Drop` instrumentation** on sources observes registry teardown: when
  `RegistryGuard` clears the registry, the parked `Box<dyn ValueSource>`
  drops, setting a flag + global drop-order stamp.

### Why not the original "no JIT, direct calls" plan

The entire registry surface is `pub(crate)`/private (see Route inventory),
and `host_fn_symbols()` does not export the stream entry points, so an
integration test cannot call `park_stream`/`stream_chunk`/`stream_element`
directly — and may not make them pub. The documented fallback applies: test
through the thinnest reachable drivers. Track B's hand-built-Core shim is
that route; it is NOT the W4 end-to-end path (no Haskell, no GHC pipeline,
and sources that Haskell cannot produce).

## Route inventory

**Unreachable for direct integration-test calls** (`pub(crate)` or private
in `host_fns.rs`; `host_fn_symbols()` exports none of them):
`ParkedStream`, `park_stream`, `clear_parked_streams`,
`alloc_stream_tail_thunk`, `stream_chunk`, `stream_element`,
`PARKED_STREAMS`, `STREAM_NEXT_ID`, `ReadySource`, `materialize_cons_list`,
`build_cons_cells`, `build_cons_cells_thunked`, `host_alloc_gc`,
`alloc_host_thunk2`, `alloc_element_thunk`.

**Exercised by this suite (via the Track B shim unless noted):**

| Code | How reached |
|---|---|
| `park_stream` + `alloc_stream_tail_thunk` + `RegistryGuard` teardown | every Track B run (dispatch-site `Plan::Park`) |
| `stream_chunk`, sequential path (incl. `catch_unwind`, `ChunkPull::Failed`, exhaustion-removal) | `SeqSource` / `PanicSeqSource` / `FailingConvSource(seq)` / `InfiniteGuardedSource` |
| `stream_chunk`, indexed path + `build_cons_cells_thunked` | `IdxSource` / `LyingLenSource` / `PanicIdxSource` |
| `stream_element` — convert, memoize, panic, `Err(BridgeError)`, out-of-bounds branches | `IdxSource` (force-twice), `PanicIdxSource`, `FailingConvSource(idx)`, `LyingLenSource` |
| `ReadySource` (dismantled `Response::Complete` spine re-park) | `t_g4` fenceposts {1999, 2000, 2001, 2256, 2257, 2500} |
| `build_cons_cells` + GC mid-materialization | p7 (512 KiB nursery, fork-contained) |
| `heap_force` memoization / poison memoization / blackhole / indirection chains | Track A (`t_a1`–`t_a4`), no Cranelift |

**NOT exercised, and why:**

- `materialize_cons_list` — kill-switch path (`TIDEPOOL_LAZY_RESULTS=0`);
  the env var is process-global and read at dispatch time, so it cannot be
  safely toggled in-process. W4 covered it via per-case subprocesses.
- `ChunkPull::Cancelled` — needs a watchdog thread flipping the cancel flag
  mid-pull; racy to pin deterministically. Future work.
- GC capture-aliasing hazard (a raw `(id, offset)` capture whose VALUE falls
  inside the nursery address range would be rewritten by evacuation's
  range-check) — not constructible through the real API: ids are small
  monotonic integers and offsets are list indices, far below address range.
  Noted as a design assumption, not tested.

## Coverage vs W4 / W5

- **W4** (`tidepool-runtime/tests/proptest_lazy_consumption.rs`): Haskell
  end-to-end, lazy-ON vs lazy-OFF vs reference, 129 cases — exonerated the
  machinery for everything Haskell + the MCP handlers can produce.
- **W5** (`tidepool-codegen/tests/proptest_jit_dispatch.rs`): JIT-vs-eval
  differential over the dispatch loop, with well-behaved
  `respond_stream(0..n)` sources.
- **S1 (this suite) adds:** adversarial sources impossible from Haskell
  (panicking-at-k producer, panicking element conversion, `BridgeError`
  returns, lying `len()`, guarded-infinite), EXACT pull/conversion counts,
  `Drop`-observed teardown, fired-panic sibling isolation, poison
  memoization semantics, and `ReadySource` re-park fenceposts.

## Bug table

| Bug | Class | Property | Repro | Status |
|:---|:---|:---|:---|:---|
| — | B1 model mismatch | P1, census, t_g4 | — | **NEGATIVE** |
| — | B2 unexpected/missing error | P6, p8, p9, lying-len | — | **NEGATIVE** |
| — | B3 fatal signal | P7 (forked GC), p8/p9 panics | — | **NEGATIVE** |
| — | B5 memoization/isolation violation | P2, P3, P4, t_g3 | — | **NEGATIVE** |

**Zero confirmed bugs.** No `.proptest-regressions` entries were recorded
(nothing to commit). The registry machinery held up against every
adversarial source, with laziness quantified to the exact call. Combined
with W4 (e2e GREEN) and W5 (dispatch differential GREEN), the parked-stream
channel is now verified-negative from three independent angles.

## Property inventory (all GREEN)

| ID | What it pins |
|:---|:---|
| t_a1 | force-twice runs the entry ONCE; state → EVALUATED |
| t_a2 | poison is MEMOIZED; second force returns poison with NO pending RuntimeError |
| t_a3 | re-entrant self-force → clean blackhole error, no hang/signal |
| t_a4 | evaluated-thunk indirection chains resolve without re-entry |
| P1 (proptest) | model equivalence, Seq+Idx, len ∈ fenceposts ∪ 2..600 |
| P2 | EXACT laziness: take-3 seq = **256** `next_value`s (one chunk); take-3 idx = **3** `get`s; spine-walk-280 idx = **0** `get`s; double-force = **1** `get`; guarded-infinite take-257 ≤ 512 pulls |
| P3 | two simultaneously-parked streams: correct values, both dropped |
| P4 | machine re-run: abandoned entry dropped BEFORE run 2 parks; run 2 clean |
| P5 | teardown universality — every run epilogue asserts sources dropped (folded into all properties) |
| P6 (proptest) | producer panic at fencepost ∪ 2..600 → clean `UserErrorMsg("…panicked…")`; unfired-panic sibling case; lying-len full force → clean "out of bounds" |
| P7 | fork-contained GC: 512 KiB nursery, GC mid-chunk-materialization → model match or clean HeapOverflow, never a dead child |
| p8 | element-thunk panic (idx ∈ {0,1,255,256} — incl. second chunk) → clean error; spine walk immune (`get_calls == 0`) |
| p9 | `BridgeError` from producer: seq `ChunkPull::Failed` + idx `stream_element` Err branches → clean "conversion failed" |
| t_g3 | FIRED-panic sibling isolation: A panics on chunk 2; B's `next_calls == 4` (fully, cleanly drained); both dropped |
| t_g4 | `Response::Complete` spine re-park fenceposts {1999, 2000, 2001, 2256, 2257, 2500}: eager path and `ReadySource` path agree with the model |
| t_g5 | lying-len spine-walk immunity (`get_calls == 0`, Ok) |
| t_panic_payload_nonstring | `panic_any(42)` → "<non-string panic>" fallback |
| t_seq_len256_pull_count | seq len=256 full drain = **257** `next_value`s (two pulls) |
| fencepost_census | len ∈ {0,1,255,256,257} × {Seq,Idx}, deterministic, counter-asserted |

## Fencepost coverage

| Dimension | Values | Where |
|:---|:---|:---|
| stream length | 0, 1, 255, 256, 257 (× Seq, Idx) | fencepost_census, P1 strategy |
| producer panic position | 0, 1, 255, 256, 257 (+ random) | P6 strategy |
| element panic index | 0, 1, 255, **256** (second chunk) | p8 |
| conversion-failure position | 0, 1, 255, 256 (× seq, idx) | p9 |
| Complete-spine length | 1999, 2000 (≤ threshold, eager), 2001, 2256, 2257, 2500 (re-parked) | t_g4 |

## Notable semantics (documented, not bugs)

1. **Poison memoization is single-witness** (t_a2): a thunk whose entry
   errors memoizes the poison; the `RuntimeError` thread-local is consumed
   by the first observer. A second force returns the poison with NO pending
   error. Today's consumers check `take_runtime_error()` immediately after
   each force, but any future re-entrant consumer would see an
   inexplicable poison — watch-item.
2. **Lying-len asymmetry** (P6/t_g5): full materialization → clean
   "element index out of bounds" error; a spine-only consumer walks all
   `claimed` cells and returns Ok without ever observing the lie.
3. **Seq chunk boundary takes two pulls** (t_seq_len256_pull_count): a
   length-256 sequential source costs 257 `next_value` calls — the loop
   fills the chunk without seeing `None`, so exhaustion is only learned on
   the next (empty) pull.
4. **A fired producer panic poisons only its own tail** (t_g3): the
   registry entry is NOT removed (only exhaustion removes it); the
   panicking pull's partial items are discarded and the tail thunk memoizes
   the poison. Sibling streams pull cleanly before and after; teardown
   still drops both sources.
5. **Pull granularity differs by source strength** (P2): `take 3` of a
   sequential source converts 256 elements (chunk granularity); of an
   indexed source converts exactly 3 (element granularity); a spine-only
   fold converts 0; a twice-forced head converts 1.

## Hunt record

- `PROPTEST_CASES=1000` run on the two randomized properties (`p1`, `p6`):
  GREEN, zero divergences, no regression seeds recorded.
- p8/p9 are deliberate **deterministic fencepost sweeps**, not random: each
  case JIT-compiles an unrolled consumer proportional to the panic/fail
  index (~3 s/case at index ≈ 600), and the meaningful inputs are the
  discrete chunk fenceposts. Random sampling at 1000 cases would cost hours
  for no added coverage. (Earlier randomized versions ran 100+ cases GREEN
  before the conversion.)
- Full suite: 18 tests, ~11 s.

## Running

```bash
cargo test -p tidepool-codegen --test proptest_parked_registry
# the randomized hunt:
PROPTEST_CASES=1000 cargo test -p tidepool-codegen \
  --test proptest_parked_registry -- p1_model_equivalence p6_panic_containment
```

Parallel test threads are safe: the registry is thread-local and every
park/run/teardown sequence is confined to one 8 MiB harness thread.
