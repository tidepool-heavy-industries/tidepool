# W4 lazy-consumption property findings (bug map)

Property suite: `tidepool-runtime/tests/proptest_lazy_consumption.rs`.
Date: 2026-06-10. Workstream W4 (lazy effect-result materialization).

## Method

Each case has a **total, exact Rust reference answer**. The same compiled
Haskell program is run TWICE via a per-case subprocess worker
(`current_exe()` → `worker_run_one`), once with `TIDEPOOL_LAZY_RESULTS=1`
(park/stream) and once with `=0` (eager drain). The oracle requires

> lazy-ON == lazy-OFF == reference

for every case. Any disagreement is a bug:

| Class | Meaning |
|-------|---------|
| B1 | result mismatch vs reference |
| B2 | runtime error / trap where the reference succeeds |
| B3 | fatal signal (SIGSEGV/SIGILL/…) |
| B4 | lazy-ON vs lazy-OFF (or nursery) divergence |

`TIDEPOOL_LAZY_RESULTS` is **never** set in-process (it is process-global and
read in `jit_machine.rs`); it is set on each subprocess `Command`. Templates
are a FIXED set (producer-shape × consumer-shape × `k∈{1,255,256,257}`) so the
GHC-extract disk cache stays hot; sizes flow through handler data, not source.

### The matrix

- **Producers** (delivery × static type):
  `List/{complete,stream,list}` (`[Text]` via `glob`, delivered by
  `respond` / `respond_stream` / `respond_list`), `StringLines` (`Text` via
  `readFile`), `TupleStringList` (`(Int,Text,Text)` via `run` — the #313
  shape), `TwoList/{…}` (two `glob`s).
- **Consumers**: `Full` (length), `Prefix(k)`, `PrefixThenEffect(k)`,
  `MapFilterPrefix(k)`, `LinesOfPartial(k)`, `ZipInterleave(k)`,
  `ForceTwice(k)`, `BranchOnly`, plus three #313-bisection consumers on
  `TupleStringList` (`TupleShowCode`, `TupleAppendStdout`, `TupleAllFields`).
- **Sizes** straddle the 2000-node Complete-spine threshold, the 256-element
  stream chunk boundary (`255/256/257`), empty/singleton edges, and the 100k
  node cap. **Nursery** ∈ {default 64 MiB, tiny 512 KiB}.

## Coverage

33 matrix cells, every cell executed **≥3 times** (counter-asserted in
`lazy_consumption_property_suite`). Three properties, all within the ≤60-case
budget:

| Property | Cases | Notes |
|----------|-------|-------|
| P1 full-matrix | 37 | every cell ×1 at primary threshold size, k=255, + empty/singleton edges |
| P2 hot-weighted | 56 | full base pass (k=257) + priority cells across sizes/k |
| P3 tiny-nursery | 36 | full base pass at 512 KiB nursery, tiny-safe sizes (≤300) |
| **total** | **129** | |

Plus dedicated tests: `cap_boundary_clean_error`, `repro_313_boundary`,
`repro_313_lib_t7_case_trap` (`#[ignore]`), `repro_313_inline_t7_is_clean`
(`#[ignore]`), `smoke_subprocess_roundtrip`, `warmup_compile_all_templates`.

## Bug table

| Cell | Class | Status |
|------|-------|--------|
| **all 33 lazy-machinery cells** | — | **GREEN** — 0 divergences across 129 cases, both modes |
| `TupleStringList × lib-module t7` | B2 | **#313 reproduced** (case trap) — *not* a lazy-results bug; see below |

**Headline: the lazy effect-result machinery is solid.** Across all delivery
methods (Complete dismantle/re-park, Stream chunked pull, IndexedList element
thunks), chunk-boundary fenceposts (255/256/257), the 2000-node spine
threshold, two simultaneously-parked streams (`ZipInterleave`), parked-stream
abandonment + registry re-entry (`PrefixThenEffect`), thunk memoization
(`ForceTwice`), unforced branches (`BranchOnly`), partial consumption
(`Prefix`/`LinesOfPartial`/`MapFilterPrefix`), empty/singleton producers, and
the 512 KiB tiny nursery — **lazy-ON, lazy-OFF, and the reference all agree.**
No B1/B3/B4 anywhere. The only B2 is #313, and it is isolated to cross-module
compilation, not the lazy channel.

### 100k node cap (`cap_boundary_clean_error`)

This is the ONE place lazy-ON and lazy-OFF legitimately differ (documented
kill-switch semantic), so it is tested separately, not under the equality
property:

- under cap (n=10k): both modes succeed and equal the reference.
- over cap (n=40k): lazy-ON **parks** (the streamed path has no node cap) and
  succeeds; lazy-OFF drains and returns a **clean** `EffectResponseTooLarge`
  error — **not** a trap or signal. Off-by-one is clean.

## #313 — reproduction + boundary characterization

> **STATUS UPDATE (2026-06-10, same day): FIXED.** The localization below led
> straight to the root cause: a **VarId collision between top-level simplifier
> floats of different modules**. `Translate.localVarId` hashes
> `(occName, unique-key)`, and GHC unique keys are per-module-compilation —
> so Probe's tuple-unpacking continuation `k_X1` and the eval preamble's
> unrelated `k_X1 :: [Text] -> …` received the SAME VarId when
> `runPipeline` concatenated both modules' binds. The serialized program then
> resumed `run`'s continuation through the wrong `k_X1`, casing the raw
> `(Int,Text,Text)` response tuple as a list — the observed
> `[CASE TRAP] Con: tag=(,,) num_fields=3, expected=[[], (:)]`.
> Fix: `GhcPipeline.externalizeInternalTops` gives internal top-level binders
> module-qualified external names with the unique key baked into the OccName,
> making `stableVarId` globally unique. The `#[ignore]`d repro is now the
> active `regression_313_lib_t7` (asserts the correct value, both lazy modes);
> `repro_313_inline_t7_is_clean` is active as its control.

**#313 reproduces, and is now sharply localized.** It is **NOT** a
lazy-effect-results bug and **NOT** a partial-consumption bug.

### What does NOT trigger it (all clean, both modes)

- **Every inline consumer**, 55/55 cases in `repro_313_boundary`:
  `TupleStringList`/`StringLines` → `lines` → `take`/`filter`/`map`, across
  `k∈{1,2,255,256,257}` and `n∈{2,20,256,257,600}`. Partial consumption
  (`(_,_,e)` discarding the code/stdout fields) is clean.
- **The exact `t7` body written INLINE** in the eval module is clean —
  `filter (\l -> len l > 1) (lines e) <> [pack (show c), o]` with all three
  tuple fields bound (`repro_313_inline_t7_is_clean`, and the bisection in
  `repro_313_boundary`).
- **Sibling `.tidepool/lib/Probe.hs` functions** `t1`/`t2`/`t5`/`t6` (which
  use only Prelude `lines`/`filter`/`map`/`isPrefixOf`) — clean from the lib
  module.

### What DOES trigger it

- **`.tidepool/lib/Probe.hs::t7` called as an imported lib function**
  (`import Probe`) traps with
  `case trap: scrutinee constructor not among case alternatives (tag
  mismatch)`, **identically under lazy-ON and lazy-OFF**
  (`repro_313_lib_t7_case_trap`).

### The boundary, stated precisely

The trigger is the **cross-module compilation of the consumer**, not its
shape and not the lazy channel:

```
              inline source        lib-module source (.tidepool/lib)
  t1/t2/t5/t6  clean                clean      (Prelude-only combinators)
  t7           clean                CASE TRAP  (uses show @Int, <>, pack)
```

The same `t7` source text traps when defined in `Probe.hs` but is clean
inline. Among lib functions only `t7` traps; it is the only one combining
`show @Int` (`pack (show c)`) and `<>` (Semigroup on `[Text]`) — i.e.
typeclass dictionaries resolved through Probe's `.hi` interface unfolding. The
fault is therefore in **cross-module unfolding / dictionary-or-DataCon-tag
identity** for a lib-defined function that mixes `Show`/`Semigroup` over an
effect-result-derived list (cf. memory notes #12/#13/#15: wrapper boxing &
case-trap = constructor tag mismatch). Both lazy modes trap identically, which
**exonerates the lazy effect-results channel**.

### Gold for the fix wave

1. Reproduce minimally with `repro_313_lib_t7_case_trap` (deterministic, ~4 s).
2. The discriminator is `t7` − `t6`: add `pack (show c)` and `<> [..]` to a
   lib function over `lines e`; that is the smallest delta that flips clean →
   trap.
3. Localize to cross-module unfolding: the inline control
   (`repro_313_inline_t7_is_clean`) compiles the byte-identical body in the
   eval module and is clean — so the fix belongs in how `Translate.hs` / the
   JIT consume a `.hi` unfolding that carries `Show`/`Semigroup` dictionaries,
   NOT in `host_fns.rs` / `dispatch.rs` / the parked-stream registry.

## Regression seeds

The randomized/enumerated lazy property suite found **zero** divergences, so
there is no `.proptest-regressions` entry to commit. The single reproduced bug
(#313) is captured as a **deterministic** `#[ignore]`d repro
(`repro_313_lib_t7_case_trap`) plus its exonerating control
(`repro_313_inline_t7_is_clean`). Both will start failing (i.e. need updating)
the moment #313 is fixed — a built-in fix-detector.

## Running

```bash
cargo test -p tidepool-runtime --test proptest_lazy_consumption -- --test-threads=1
# the #313 repros (ignored by default):
cargo test -p tidepool-runtime --test proptest_lazy_consumption -- \
  --ignored --exact repro_313_lib_t7_case_trap --nocapture
```

`--test-threads=1` is required: the compile cache and the parked-stream
thread-local registry are process-wide.
