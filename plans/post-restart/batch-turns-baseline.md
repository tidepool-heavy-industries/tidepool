# One spawn per BLOCK: the BEFORE baseline

**Lane:** `batch-turns`. Measurement only — no production code changed. This
is the baseline `plans/post-restart/batch-turns-feasibility.md` §1 predicts
from code reading; every number below is MEASURED, from the real
`session_run` entry point, not re-derived.

Branch tip at measurement time: `4063e048` (spike findings folded in;
`plans/post-restart/batch-turns-spike-findings.md` is the sibling's
writeup).

---

## 1. Spawns per item shape (measured)

New test: `tidepool-repl/tests/batch_turns_spawn_census.rs`,
`spawn_census_per_item_shape` — drives one `Repl`/server
(`build_full_server`, full effect stack) through all six shapes in sequence,
resetting `tidepool_extract_cmd::reset_extract_spawn_count()` before each
and reading `extract_spawn_count()` after. Each shape's block must complete
`ok` to be measured (a failed block takes a different, non-steady-state
route). Ran twice (once plain, once under `TIDEPOOL_TIMING=1` for §2) — the
counts reproduced byte-for-byte both times:

| Shape | Measured spawns | §1 reading, predicted | Disagreement |
|---|---|---|---|
| single decl | **4** | 3 (1 validate + 1 probe, + 1 block-level classify) | **+1, unexplained** |
| 3 consecutive decls | **6** | 5 (1 batched validate + 3 probes, + 1 classify) | **+1, unexplained** |
| pure bind | **4** | 3 (1 decl-route validate + 1 probe, + 1 classify) | **+1, unexplained** |
| effectful bind | **2** | 2 (1 `--turn` + 1 classify) | matches |
| bare expression | **3** | 2 baseline, +1 "when query_inner_type fires" | matches the caveat — see below |
| mixed 5-item block | **12** | ~8 baseline / ~10 with both bare-expr items firing the +1 | **+2 over the adjusted estimate** |

**How the predicted column was built.** §1's own table gives per-item-shape
costs (`1 validate + 1 type probe *per value decl*`, `1 define_scoped + 1
probe_pure_type`, `1 --turn`, `1 --turn, plus query_inner_type when it
fires`) and separately states block-level classification is its own,
already-amortized spawn (`run_block` calls `classify_block` **once per
block**, not per item). Every item in this census is keyword-free (`f x = x
+ 1`, `let x = e`, a bare expression) — the realistic shape used throughout
the existing suite (`decl_plane.rs`'s `inc x = x + 1`) — so every block pays
that one classify spawn. Predicted = §1's table value + 1.

**Disagreement #1 (the important one): every successful-decl-route shape
(single decl, 3 consecutive decls, pure bind) measures ONE SPAWN HIGHER than
§1's own table + classify accounting predicts — consistently, not
occasionally.** The log shows no failed/retried compile around these shapes
(no `Compilation failed` stanza), so this is not the same mechanism as
finding #2 below; it is a genuine extra successful spawn §1's code reading
didn't account for. This is exactly the kind of thing the checkpoint asked
to surface and it needs the feasibility owner to trace before the batch
planner's per-item spawn budget is trusted. Candidates worth checking first:
a second probe inside `defined_outcome`/`try_pure_bind_as_decl` not visible
from the cited line ranges, or a `:bindings`-adjacent stale-check compile.
Not chased further here — out of this lane's measurement-only boundary.

**Disagreement #2 (mechanism, not just count): the bare-expression "+1"
is not a rare `query_inner_type` special case — it fired on EVERY observed
bare-expression turn in this run** (the standalone shape and both
bare-expression items in the mixed block), and it is not a probe — it is a
**failed first `--turn` compile attempt**, visible in the log as a genuine
`Compilation failed` / `GHC-83865` diagnostic (`Couldn't match expected type
'Eff [...] a' with actual type 'Int'` — the extractor tries the item as a
monadic statement referencing `it` before falling back to the working
template). That failed attempt still pays a full spawn: `startup` +
`ghc_setup` + `ghc_load` (~2.9-3.8s in this run, see §2) for zero result. §1
attributes this to `query_inner_type` (`session.rs:2277`); the visible
diagnostic suggests the actual mechanism is a template-selection retry, not
a type probe. Worth a naming correction in §1, not just a number correction.

---

## 2. TIDEPOOL_TIMING phase breakdown of a real `--turn` spawn (the decisive number)

**Why this matters more than §1.** The sibling spike
(`batch-turns-spike-findings.md`) ran every compile cycle inside ONE
`runGhc` session and measured `load'` at 145-566ms of a ~2.6-2.7s per-cycle
GHC cost ("roughly 5-20%"). That denominator never includes a real process
boot — the spike's session paid `startup`/`ghc_setup` once for its whole
multi-cycle run, not once per cycle. Batching's actual win is amortizing
**per-spawn** fixed cost across a **block**, so the right denominator is a
genuine, separate `tidepool-extract` process spawn's own phase breakdown —
which is what this section measures, isolated (no concurrent GHC load on
the box) via `TIDEPOOL_TIMING=1` on the same census test.

Two real, successful spawns, verbatim (`ms`, in phase order):

### Effectful bind (`censusEff <- run "echo census-effectful" >>= liftEither`)

```
phase=startup     ms=47
phase=ghc_setup   ms=109
phase=ghc_load    ms=4478
phase=typecheck   ms=725
phase=core        ms=3515
phase=translate   ms=120
phase=cbor_encode ms=1
phase=write       ms=0
```
total = 8995 ms. fixed (startup+ghc_setup+ghc_load) = 4634 ms = **51.5% of total**.

### Bare expression (`1 + (1 :: Int)`)

The FIRST attempt for this shape fails (see Disagreement #2 above) and pays a
wholly-wasted spawn before the real one:

```
# wasted spawn (template-selection retry, never reaches typecheck):
phase=startup   ms=30
phase=ghc_setup ms=64
phase=ghc_load  ms=3713
# then: Compilation failed (GHC-83865, "it" reference) — 3807ms for nothing
```

The successful spawn that actually produces the result:

```
phase=startup     ms=30
phase=ghc_setup   ms=48
phase=ghc_load    ms=2947
phase=typecheck   ms=407
phase=core        ms=7792
phase=translate   ms=111
phase=cbor_encode ms=1
phase=write       ms=0
```
total = 11336 ms. fixed = 3025 ms = **26.7% of total** — and that excludes the
~3807ms wasted spawn immediately before it, which is 100% fixed-shaped cost
for a zero-value result.

### The fixed fraction depends heavily on which preamble the turn compiles against

Aggregating every successful timed spawn in the clean run (12 total,
excluding the 3 failed template-retry spawns):

| Turn class | n | fixed % of total (range) | avg |
|---|---|---|---|
| small decl-plane validate/probe (Lib.G\<g\>, ~14-16 top-level bindings) | 5 | 78.4% – 87.1% | **83.2%** |
| full-stack eval-preamble turn (effectful bind / bare expression against the ~2900-binding Console/KV/Fs/Http/Exec/… preamble) | 7 | 26.7% – 51.5% | **31.5%** (28.1% excluding the one effectful-bind sample) |

The gap is `ghc_load` staying roughly flat (2.7-7.5s either way — it's
dominated by interface-file I/O over the same ~40-module stdlib closure) while
`typecheck`+`core` balloons from ~1-1.7s (16 bindings) to ~7-8s (2900+
bindings, the full effect-verb surface). **A batch of decl-shaped items
amortizes a large, dominant fixed cost; a batch of effectful-bind/bare-expr
items amortizes a smaller minority of a cost dominated by per-item
typecheck+core — which matches §1's own framing ("the win is amortization of
GHC boot + stdlib `load'`, not of per-item typecheck") but sizes it
concretely per shape for the first time.**

### The answer to "what fraction of total is startup+ghc_setup+ghc_load, times (N-1)/N"

For a representative N=5-item block:

- **decl-shaped items:** 83.2% × 4/5 = **~66.6% floor** on the batch win.
- **full-stack turn items (effectful bind / bare expression):** 31.5% × 4/5
  = **~25.2% floor** (28.1%-avg basis: ~22.5%).

Both floors are meaningfully above the spike's "5-20%" line, because the
spike's denominator excluded process boot entirely. But they diverge by
~2.5x depending on item shape — a single lane-wide number would hide that a
block dominated by effectful binds/bare expressions (the CLAUDE.md eval
surface's actual majority case) wins closer to 25%, not 66%, and the
`ghc_load` recovery the spike proved (566ms → 1ms via `ModIfaceCache`) is a
small slice of the ~2.7-7.5s this measurement shows `ghc_load` actually
costs on a genuinely cold-process spawn (vs. the 145-566ms the spike saw
reloading inside an already-warm session) — cold-process `ghc_load` is
close to an order of magnitude more expensive than the spike's own
within-session number, which is itself an argument FOR the batching lane:
the biggest win isn't shrinking `load'` on a warm session, it's never
paying a cold process boot for items 2..N in the first place.

### TIDEPOOL_GHC_LIBDIR — confirmed unset in the live repl server's environment

`~/.claude.json`'s `mcpServers.tidepool-repl.env` is `{}` — empty. Per
`GhcPipeline.hs:986-992`, an unset `TIDEPOOL_GHC_LIBDIR` makes `getLibdir`
shell out to `readProcess "ghc" ["--print-libdir"]` — an entire extra
process fork, on every single extract spawn. This measurement's `startup`
phase (24-86ms across all sampled spawns) already reflects that unset state
(the test environment doesn't set it either). Setting it in the live
server's launcher config would delete that subprocess spawn entirely — pure
amortizable waste, and unrelated to the batching lane (a one-line,
independent fix), but small in absolute terms (well under 1% of a full-stack
turn's ~9-11s total). Noted per request; not sized as decisive on its own.

---

## 3. `tidepool-repl` shard wall time

Command (both runs): `export XDG_CACHE_HOME="$PWD/.cache"; scripts/battery-shard.sh tidepool-repl`
(self-slots via `ghc-slots.sh`; `TIDEPOOL_EXTRACT` was already set to the
nix-profile wrapper for the census runs above, so both shard runs built
their own dev `tidepool-extract-bin` per the script's normal fallback).

| Run | `rm -rf` cache first? | Wall time | Tests | Pass | Fail | Skip |
|---|---|---|---|---|---|---|
| **COLD** | yes (`rm -rf .cache`, dir did not previously exist) | **2919.26s / 48m 40s** (nextest summary; shell `time` agrees: 48m40.27s total) | 195 | 195 | 0 | 0 |
| **WARM** | no (reused COLD's `.cache`) | **3313.13s / 55m 14s** (nextest summary; shell `time`: 55m14.18s total) | 195 | 195 | 0 | 0 |

Neither run was killed or partial — both ran to completion with exit code 0.

**Disagreement worth flagging on its own: WARM was SLOWER than COLD for this
crate (+393.87s, +13.5%), not faster.** The repo `CLAUDE.md`'s own reference
point (99s cold vs 47s warm, "three harness binaries") does not generalize to
this shard. The likely reason: `tidepool-repl`'s 195 tests each compile
**distinct, per-test Haskell source** (different decl/bind/expr text per
test) — the content-addressed compile memo
(`$TIDEPOOL_COMPILE_CACHE_DIR`/`plans/compile-memo.md`) only pays off on a
byte-identical re-invocation, which barely occurs across this suite's
integration tests (only the handful of genuinely repeated in-process unit
tests, e.g. `truncate::tests::*`, ran near-instantly at ~0.01-0.02s in the
warm run). The two runs' per-test wall times track each other closely test
by test (e.g. `decl_plane::colliding_pure_bind_shadows_gracefully`: 25.1s
cold vs 24.6s warm) — consistent with "the memo essentially doesn't fire for
this crate's test content" rather than any regression. Report this as
measured, not rounded to the CLAUDE.md's cross-crate expectation.

---

## 4. Exact commands run

```bash
# spawn census (plain)
scripts/battery.sh -p tidepool-repl -E 'binary(batch_turns_spawn_census)' --no-capture

# spawn census + phase breakdown (TIDEPOOL_TIMING), run in isolation (no concurrent GHC load)
TIDEPOOL_TIMING=1 scripts/battery.sh -p tidepool-repl -E 'binary(batch_turns_spawn_census)' --no-capture

# cold shard
rm -rf .cache
export XDG_CACHE_HOME="$PWD/.cache"
time scripts/battery-shard.sh tidepool-repl

# warm shard (same cache dir, no rm)
export XDG_CACHE_HOME="$PWD/.cache"
time scripts/battery-shard.sh tidepool-repl

# TIDEPOOL_GHC_LIBDIR live-server check
python3 -c "import json,os; d=json.load(open(os.path.expanduser('~/.claude.json'))); print(d['projects']['/home/inanna/dev/tidepool']['mcpServers']['tidepool-repl'])"
```

Verify legs (all green): `cargo check --workspace --all-targets`,
`cargo clippy --workspace --all-targets`, `cargo fmt --all -- --check`, and
`spawn_census_per_item_shape` itself (asserts every shape >= 1 spawn and
every measured block succeeded — it reports, it does not gate on the exact
numbers, since reporting them is the test's job).
