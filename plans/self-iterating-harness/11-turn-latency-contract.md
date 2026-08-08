# Turn-latency measurement contract

Measurement only. Nothing in this document proposes a fix; the attribution
report (`11-turn-latency-report.md`) ranks candidates, and a later wave picks
one.

## The pipeline one answerer round walks

`SelfHarnessDriver::drive_answerer_to_finalize` loops over
`Harness::drive_turn`. One iteration:

1. **provider call** — `Harness::stream_turn`: the model writes a reply,
   possibly containing a fenced Haskell block. Network + inference.
2. **template** — `run_block` builds every wrapper template the turn's
   verdict might select (`engine::expr_turn_template`/`template_session_bind`,
   the decl parse wrapper) — the effect row, pragmas, imports, the node's
   `Lib.G<gen>` decl module, and its current `Val.G<gen>` modules — BEFORE the
   verdict is known, since the one spawn below classifies and compiles
   together.
3. **turn compile** — `tidepool_runtime::session::run_turn` spawns
   `tidepool-extract --turn` ONCE: the extract classifies the block itself
   (bind/expr/decl — GHC-sourced, never a Rust-side guess), splices the
   matching template, and — for a BIND or EXPR verdict — compiles it through
   the whole GHC pipeline in the SAME process, writing `result.cbor`,
   `meta.cbor`, `asks.json`, and the `TurnOut` sidecar into a tempdir. A DECL
   verdict does not compile here: `run_block` separately calls
   `session.define_scoped`, which performs its OWN decl-plane `run_turn`
   spawn (see `plans/one-spawn-turn-protocol-phase-b.md`'s Decision 1 — this
   is the one case Phase B's "one spawn per turn" claim does not cover, and
   says so).
4. **read + deserialize** — `result.cbor`/`meta.cbor` are read and decoded
   into `CoreExpr`, `DataConTable`; `asks` arrives already decoded, off the
   `TurnOut` wire payload.
5. **jit codegen** — `ResidentSession::run` → `add_fragment_session`: Cranelift
   mints the fragment against the merged table.
6. **run** — `Threadless::run_fragment` on the eval thread, to completion or to
   a suspension (`finalize`, `askUser`, `fork`).

Step 3 is the round's one `tidepool-extract` process spawn (down from the two
separate spawns — a parse-only classify, then a full compile — a harness turn
made before the one-spawn-per-turn migration). A compile error at step 3
costs a full round: the driver pushes the diagnostic back as a user turn and
the loop restarts at step 1.

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

Build once per profile, then run under the shared GHC lock (the run itself
spawns real `tidepool-extract` processes — respect the same serialization
discipline as any other GHC-heavy invocation in this repo). **The `debug`
build is the primary documented invocation** — production
(`harness-dogfooding/run.sh`) launches `./target/debug/tidepool-selfharness`,
so debug is what the report should quote; `release` is kept only as an
explicit comparison point, not the default:

```bash
export PATH=/nix/store/i7xkw0wd599j23fbsz8ydmsfj4dp9831-ghc-native-bignum-9.12.2-with-packages/bin:$PATH
export TIDEPOOL_EXTRACT=/home/inanna/dev/tidepool/haskell/dist-newstyle/build/x86_64-linux/ghc-9.12.2/tidepool-extract-0.1.0.0/x/tidepool-extract-bin/build/tidepool-extract-bin/tidepool-extract-bin
export TIDEPOOL_TIMING=1

# primary (matches production's debug launch)
cargo build --example turn_latency_bench -p tidepool-harness
flock /tmp/tidepool-ghc.lock ./target/debug/examples/turn_latency_bench

# comparison
cargo build --release --example turn_latency_bench -p tidepool-harness
flock /tmp/tidepool-ghc.lock ./target/release/examples/turn_latency_bench
```

The `cargo build` steps are pure Rust and do not touch GHC/`tidepool-extract`,
so they do not need `flock` — only the run itself (which spawns the real
extract subprocesses) does. Splitting it this way keeps the lock held for the
run only, not for however long the Rust compile takes. Run the two profiles
**serialized, never concurrently** — one `flock` acquisition per run, debug
then release, so nothing on the box is contending against itself.

**Lock discipline under heavy contention:** `flock`'s wait queue is not
FIFO-fair. Under sustained multi-agent load (several sibling `flock
/tmp/tidepool-ghc.lock cargo nextest ...` processes queued at once), a
`flock -w <timeout>` that repeatedly times out and re-bids can starve
indefinitely — a fresh, short-lived bidder can keep winning the race against
an older waiter. If a bounded wait keeps timing out, block with NO timeout
(`flock /tmp/tidepool-ghc.lock <cmd>`, no `-w`) instead of retrying a timed
one; it cannot lose the race because it never releases and re-bids. A `flock`
acquisition of unknown duration should run outside any process-lifetime cap
your environment enforces on foreground/backgrounded commands — detach it
(`setsid nohup ... &` disowned) and poll a marker file for completion rather
than holding a call open waiting on it.

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

`attributed_ms` is the sum of every `record_stage` sample seen while THAT
TURN was in flight — an in-flight turn marker on the collector, set right
before and cleared right after each `drive_turn`/`run_to_hole_or_done` call,
NOT the sample's `node` field. This matters because two of the nine Rust
stages, `jit_codegen` and `run_exec`, are emitted from `tidepool-runtime`
with no answerer node id at all (a `NO_NODE` sentinel, `u64::MAX` —
`meta.no_node_stages` names them); keying attribution off `node` would have
silently dropped those two stages into every turn's residual, overstating
`unattributed_ms` by exactly the amount they measured. `unattributed_ms` is
`wall_ms - attributed_ms` — the residual is itself a finding (see below).
`stages` is `RUST_STAGES` (pipeline order) filtered to what actually fired,
plus any `extract.*` phase stages, sorted. Percentiles are documented in
`meta.percentile_definition`: a plain sorted-index nearest-rank
(`round((n-1)*p)`) — with the single-digit `n` this bench uses, "p90" mostly
coincides with the max, a worst-observed marker rather than a true quantile.

## Measured runs (2026-08-08)

Two rounds of measurement, separated by an operational incident (below).
**The locked, post-directive numbers are authoritative; the pre-directive
numbers are contextual only** — they predate the box-wide lock-discipline
fix and were taken while unlocked or under undocumented sibling load, so
treat any gap between the two pairs as informative about contention, not
about debug-vs-release per se.

Box-wide operational note: around 02:00 the same day, a swap-full/high-load
incident traced in part to GHC-heavy work (this bench included) running
without holding `/tmp/tidepool-ghc.lock`. Root's remediation: hold the lock
for every GHC-heavy invocation (this bench's own binary counts, not just
`cargo nextest`); no LSP/rust-analyzer tooling on this box (each instance
costs 3–5Gi, six concurrent tipped the machine over). Root also confirmed two
lock-discipline hazards surfaced while re-measuring here, now fixed
swarm-wide: (1) an orphaned `flock` holder (reparented to PPID 1, zero
children — a stuck waiter, not slow work) can wedge the lock indefinitely
until manually cleared; (2) `flock`'s wait queue is not FIFO — a `flock -w
<timeout>` that keeps timing out and re-bidding can starve indefinitely
against shorter-lived siblings, so a contended re-acquisition should block
with no timeout (see the lock-discipline note above) rather than retry a
timed wait.

### Pre-directive (unlocked, contended — NOT authoritative)

A full invocation (`N=5`, `size_n=3`, `retry_repeats=3`, the defaults above),
release profile, unlocked: **113.7s wall clock** (`overall_wall_ms: 113666`).
Per-scenario wall totals: `cold_vs_warm` 28.9s (5 turns), `small_vs_large_block`
35.0s (6 turns), `compile_error_retry` 22.6s (3 round-trips). Every turn
reached `Suspended` (the `Finalize` hole) — including every retry round-trip,
which proves the corrective-retry loop actually recovered from the injected
error, not just that it ran.

The same invocation, debug profile, also unlocked (launched moments before
the lock-discipline directive landed, mid-flight when it did): **282.5s
wall clock** (`overall_wall_ms: 282485`) — a ~2.5x gap over the release
figure. Taken concurrently with box load average 17–40 from sibling GHC work,
so this number cannot yet be trusted as the debug/release delta; it is
exactly the contended-vs-quiet question the locked pair below settles.

**Confirmed this drives real `tidepool-extract`, not a stub** (from the
release run above): turns cost 5.7–7.9s wall clock each (two real GHC-session
spawns per turn: the `--emit-stmt-binders` classify lane, then the full
compile), and the retry scenario's pushed-back diagnostic is a byte-for-byte
real GHC error, e.g. (from `run_to_hole_or_done`'s corrective user turn, read
off the scenario's `log.jsonl`):

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

**One directional observation from the pre-directive release run** (not a
finding — measurement only, no fix proposed here): `small_vs_large_block`'s
per-turn wall clock did not move appreciably between the one-line block and
the ~60-line block (both ~5.7–5.9s), suggesting turn cost was dominated by a
flat per-spawn floor rather than scaling with source size — but with zero
stage attribution this run cannot say WHERE that floor is (GHC session
startup vs. typecheck vs. something else).

### Post-directive (locked — AUTHORITATIVE)

Re-measured under `flock /tmp/tidepool-ghc.lock` per the lock-discipline
directive above, via a detached single-acquisition runner (blocks on the
lock with no timeout, so it cannot lose the non-FIFO race described above;
runs debug then release back to back inside ONE acquisition so both numbers
are taken under identical box conditions). Sample reduced to `N=2`,
`size_n=1`, `retry_repeats=1` — small enough to be a good citizen on a
contended lock and to keep total hold time short.

Runner timeline: queued 09:28:01Z, acquired the lock 10:10:32Z (a ~42.5
minute wait — the swarm-wide non-FIFO-starvation and orphan-holder fixes
above landed in the middle of that wait, from this same incident), ran debug
then release back to back, released at 10:12:41Z. The lock was ITSELF
contended when the runner queued and when it acquired — per root's
disposition (measure now under contention with the caveat, rather than chase
a quiet box; a dedicated clean re-run is separately scheduled after the
current wave folds), these numbers are reported **AS TAKEN, under real
sibling GHC load**, not a pristine box. Per root, that is arguably closer to
lived dogfood experience than an idle-box number would be anyway.

**Debug (primary, matches production):** `overall_wall_ms: 76947` (77.0s).
Per-scenario wall: `cold_vs_warm` 15.8s (2 turns, 7900ms/7852ms),
`small_vs_large_block` 15.8s (2 turns, 7831ms small / 7985ms large — again no
appreciable small-vs-large gap), `compile_error_retry` 9.6s (1 round-trip).
Every turn `Suspended` (finalize reached), including the retry round-trip.

**Release (comparison):** `overall_wall_ms: 51628` (51.6s). Per-scenario
wall: `cold_vs_warm` 11.8s (2 turns, 5847ms/5998ms), `small_vs_large_block`
11.9s (2 turns, 5965ms/5888ms), `compile_error_retry` 7.6s (1 round-trip).

**The debug/release gap under lock is ~1.3–1.5x, not the ~2.5x the unlocked
pair suggested** (single non-retry turns: 7830–7985ms debug vs 5847–5998ms
release, ≈1.33x; the retry round-trip: 9624ms vs 7600ms, ≈1.27x; the overall
totals, 76947ms vs 51628ms, ≈1.49x — pulled up by `N` being too small for the
three scenarios' fixed per-`Harness`-boot cost to average out evenly, not by
a per-turn effect). Per root's framing: most of the pre-directive pair's 2.5x
gap was contention, not a true debug-vs-release difference — the real,
uncontended-in-relative-terms gap is real but smaller. Debug is still
genuinely and consistently slower than release on every single-turn
comparison, so **debug remains the number the report should quote** (it
matches production), with this ~1.3–1.5x figure as the caveat on how much of
the pre-directive 2.5x to trust.

A cheap non-blocking probe (`flock -n /tmp/tidepool-ghc.lock true`) taken
just before submitting this measurement found the lock still held by a
sibling — per root's optional-extra guidance, that means skip the quiet-box
comparison run rather than wait for one; the scheduled clean re-run after
this wave folds is where that comparison belongs.

**stages tables** were empty in the runs above: they were taken before the
`timing::record_stage` call sites merged, so every `stages` array is empty and
every `attributed_ms` is 0. Those runs are wall-clock baselines only. The
section below supersedes them for anything stage-level.

## Stage attribution (one run, debug, exclusive lock)

Taken after all three instrumentation branches merged, via
`scripts/ghc-slots.sh exclusive` (whole-box quiet, so no sibling GHC load),
debug profile, `N=2 / size_n=1 / retry_n=1`. `TIDEPOOL_TIMING=1` was set but
the extract on `TIDEPOOL_EXTRACT` predated the phase lines, so no `extract.*`
rows appear — see the gap note below. Medians in ms:

| stage | cold/warm | small/large | retry round-trip |
|---|---|---|---|
| `extract_spawn` | 6778 | 9425 | 12620 |
| `jit_codegen` | 2704 | 4070 | 4361 |
| `classify_extract` **RETIRED** (see tombstone below) | 33 | 75 | 72 |
| `cbor_deserialize` | 9 | 17 | 21 |
| `run_exec` | 0 | 0 | 3 |
| `template`, `cbor_read`, `asks_parse` | 0 | 0 | 0 |
| `provider_call` | 0 | 0 | 0 (replayed, not a live model) |

> **`classify_extract` — RETIRED at `f320d21949feffe3d22ac64bee9dc0ffe0802d79`, successor
> `extract.classify`.** Its semantics were a SEPARATE process spawn's wall
> clock — the parse-only `--emit-stmt-binders` lane `run_block` called before
> compiling. That spawn no longer exists: the one-spawn-per-turn migration
> (`plans/one-spawn-turn-protocol-phase-b.md`) folded classification into the
> single `--turn` process a harness turn now makes, which times its own
> in-process classify substep as `extract.classify` (an `extract.*` phase
> forwarded from INSIDE `extract_spawn`, not a sibling Rust-side stage — see
> `timing.rs`'s module doc). A same-named stage carrying that different
> meaning would silently poison any longitudinal comparison against the rows
> above, so the constant is retired rather than repurposed — same fails-loud
> principle as the one-format wire policy. The 33–75ms row above stays
> exactly as measured: it is what reframed this whole workstream (a
> parse-only spawn costing tens of milliseconds against a ~6.8s compile,
> settled below), and remains the reproducible baseline `extract.classify`
> should be compared against once it is next measured — read it as a
> retired-but-interpretable historical figure, not a stale one.

Per-turn residual (`wall_ms − attributed_ms`) is 32–76ms against 9–21s turns —
under 0.4%, so the stage set accounts for essentially the whole turn and
nothing material is hiding between stages.

Three things this settles:

- **`extract_spawn` and `jit_codegen` are the turn.** Roughly 70% and 28%.
  Every other stage combined is under 1%.
- **The double extract spawn is not the headline cost.** `classify_extract` —
  the parse-only `--emit-stmt-binders` lane, the second of the two spawns per
  round — costs 33–75ms, not seconds. Collapsing it would win milliseconds.
  This was the pre-measurement suspicion and the numbers do not support it.
- **`jit_codegen` at 2.7–4.4s is larger than anyone predicted**, and it is
  Rust-side Cranelift work measured in an unoptimized build. Production
  (`harness-dogfooding/run.sh`) launches `target/debug/tidepool-selfharness`,
  so this cost is real as currently configured, but it is a build-profile
  artifact as much as an algorithmic one — the release comparison above runs
  ~1.3–1.5x faster overall, consistent with most of the debug penalty landing
  here.

**The open gap:** where the ~6.8s inside `extract_spawn` goes is NOT answered.
A synthetic single-module extract run (captured while building the Haskell-side
instrumentation) totals 194ms with `ghc_session` at 122ms and `typecheck` at
~1ms — but a real templated turn's spawn is ~35x that, and the difference lives
entirely in the preamble + `Tidepool.Prelude` + effect-stack decls compiled as
home modules, which the synthetic module doesn't have. So the synthetic split
cannot be extrapolated. Whether the real spawn is dominated by session/interface
loading or by typechecking those extra home modules is still open, and the two
answers point at different mechanisms. Re-running the bench with
`TIDEPOOL_TIMING=1` against an extract built from this branch's `haskell/`
answers it in one shot — that capture was queued and then deliberately
abandoned when the measurement was shelved, not attempted and failed.
