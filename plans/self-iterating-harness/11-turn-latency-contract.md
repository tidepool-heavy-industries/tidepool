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

## The bench artifact and how to run it

`tidepool-harness/examples/turn_latency_bench.rs`. It builds a real `Harness`
over `answerer_decls()` (mirroring `tests/acceptance_selfharness.rs`'s
construction) with a `replay::ReplayProvider` substituting the model, and
drives real turns through `Harness::drive_turn` (cold/warm, small/large) and
`Harness::run_to_hole_or_done` (the compile-error retry scenario, whose
corrective loop is the driver-level mechanism from the pipeline section
above). Every turn finalizes a plain `Int`, so no `project_lib`/author-defined
ADT is needed.

Build once, then run under the shared GHC lock (the run itself spawns real
`tidepool-extract` processes — respect the same serialization discipline as
any other GHC-heavy invocation in this repo):

```bash
export PATH=/nix/store/i7xkw0wd599j23fbsz8ydmsfj4dp9831-ghc-native-bignum-9.12.2-with-packages/bin:$PATH
export TIDEPOOL_EXTRACT=/home/inanna/dev/tidepool/haskell/dist-newstyle/build/x86_64-linux/ghc-9.12.2/tidepool-extract-0.1.0.0/x/tidepool-extract-bin/build/tidepool-extract-bin/tidepool-extract-bin
export TIDEPOOL_TIMING=1

cargo build --release --example turn_latency_bench -p tidepool-harness
flock /tmp/tidepool-ghc.lock ./target/release/examples/turn_latency_bench
```

The `cargo build` step is pure Rust and does not touch GHC/`tidepool-extract`,
so it does not need `flock` — only the second line (which spawns the real
extract subprocesses) does. Splitting it this way keeps the lock held for the
run only, not for however long the Rust release compile takes.

`RUST_LOG` is **not** required: the bench installs its own `tracing`
subscriber that matches `tidepool_harness::timing` events at `DEBUG`
unconditionally (step 8 of this bench's build spec — a human who forgets to
set `RUST_LOG` still gets data). `TIDEPOOL_TIMING=1` is optional today — it
only matters once the extract-side phase-forwarding sibling branch lands — but
harmless to set now and forward-compatible once it does.

Env knobs (all optional):

| var | default | meaning |
|---|---|---|
| `TURN_LATENCY_BENCH_N` | `5` | turns in the cold-vs-warm scenario |
| `TURN_LATENCY_BENCH_SIZE_N` | `N` clamped to `[1,3]` | turns per condition (small/large) in the block-size scenario |
| `TURN_LATENCY_BENCH_RETRY_N` | `N` clamped to `[1,3]` | round-trips (bad block + corrected block) in the retry scenario |
| `TURN_LATENCY_BENCH_OUTPUT` | `target/turn-latency-bench.json` | where the JSON summary is also written (stdout always gets it too) |

## Output shape

One JSON object on stdout (and written to `TURN_LATENCY_BENCH_OUTPUT`):
`{pid, n, size_n, retry_repeats, overall_wall_ms, output_path, scenarios: \
[{scenario, n_turns, turns: [{index, label, node, outcome, wall_ms, \
attributed_ms, unattributed_ms}], stages: [{stage, n, median_ms, p90_ms, \
total_ms}], wall_ms_total, wall_ms_median}], meta}`.

`attributed_ms` is the sum of every `record_stage` sample seen for that turn's
`node` id while its scenario's collection window was open; `unattributed_ms`
is `wall_ms - attributed_ms` — the residual is itself a finding (see below).
`stages` is `RUST_STAGES` (pipeline order) filtered to what actually fired,
plus any `extract.*` phase stages, sorted. Percentiles are documented in
`meta.percentile_definition`: a plain sorted-index nearest-rank
(`round((n-1)*p)`) — with the single-digit `n` this bench uses, "p90" mostly
coincides with the max, a worst-observed marker rather than a true quantile.

## Measured run (2026-08-08)

A full invocation (`N=5`, `size_n=3`, `retry_repeats=3`, the defaults above)
completed in **113.7s wall clock** (`overall_wall_ms: 113666`), comfortably
under the ~380s kill. Per-scenario wall totals: `cold_vs_warm` 28.9s (5
turns), `small_vs_large_block` 35.0s (6 turns), `compile_error_retry` 22.6s (3
round-trips). Every turn reached `Suspended` (the `Finalize` hole) — including
every retry round-trip, which proves the corrective-retry loop actually
recovered from the injected error, not just that it ran.

**Confirmed this drives real `tidepool-extract`, not a stub**: turns cost
5.7–7.9s wall clock each (two real GHC-session spawns per turn: the
`--emit-stmt-binders` classify lane, then the full compile), and the retry
scenario's pushed-back diagnostic is a byte-for-byte real GHC error, e.g.
(from `run_to_hole_or_done`'s corrective user turn, read off the scenario's
`log.jsonl`):

```
GHC error:
tidepool-extract failed:
stdout:
{"version":1,"diagnostics":[{"span":{...,"startLine":27,"startCol":16,...},
"severity":"error","message":"No instance for `GHC.Internal.Data.String.IsString Int'
  arising from the literal `\"oops-not-an-int\"'"}]}
stderr:
/tmp/.tmp43VLb8/Expr.hs:27:16: error: [GHC-39999]
    No instance for ‘GHC.Internal.Data.String.IsString Int’
      arising from the literal ‘"oops-not-an-int"’
   |
27 | (finalize @Int "oops-not-an-int" :: M ())
   |                ^^^^^^^^^^^^^^^^^
```

**All `stages` tables were empty** in this run (`attributed_ms: 0` on every
turn, `unattributed_ms == wall_ms`) — expected per this bench's build spec: as
of this measurement, ZERO `timing::record_stage` call sites exist on the
answerer turn path yet, and ZERO extract-side `TIDEPOOL_TIMING` forwarding
exists (both are sibling branches that merge after this one). The bench is
correct-but-empty today; it fills in once those land, with no changes to this
bench needed.

**One directional observation** (not a finding — measurement only, no fix
proposed here): `small_vs_large_block`'s per-turn wall clock did not move
appreciably between the one-line block and the ~60-line block (both
~5.7–5.9s), suggesting turn cost in this run was dominated by a flat
per-spawn floor rather than scaling with source size — but with zero stage
attribution this run cannot say WHERE that floor is (GHC session startup vs.
typecheck vs. something else). That question is exactly what the per-stage
table answers once the sibling branches land; re-run this bench after they
merge to get the real breakdown.
