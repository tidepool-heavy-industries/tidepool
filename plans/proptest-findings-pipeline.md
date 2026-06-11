# W6 — Haskell → Core → JIT pipeline: differential proptest findings

Differential testing of the full `Translate.hs` → Core → Cranelift JIT pipeline
by generating small **total** Haskell source programs (a Rust AST, pretty
printed) and comparing `compile_and_run_pure(...).to_json()` against a Rust
**reference interpreter** (the oracle, total by construction).

This layer is unreachable by the Rust-side IR generators (`proptest_jit_vs_eval`
et al.) — those start from Core; this starts from **Haskell source** and
exercises GHC -O2 Core shapes and the 17 documented translation gotchas
(CLAUDE.md).

Harness: `tidepool-runtime/tests/proptest_haskell_pipeline.rs`.

## Result (TL;DR)

**Verified negative.** 1400 pure long-haul cases across 4 rounds (depths 3–6,
including a forced Integer-defaulting probe) + 90 committed pure/determinism
cases + 8 effectful A/B cases + 1000 reference self-check cases — **0 pipeline
bugs found.** Every generated total program produced JIT output identical to the
reference interpreter; compilation was deterministic; the lazy/eager effect
paths agreed with each other and with the reference.

Two **harness** bugs were found and fixed during authoring (not pipeline bugs):
a `LetShadowInt` var-id desync in the generator (caught by the reference
self-check), and a libtest `--nocapture` marker-parsing bug in the effect-worker
driver (the worker's `__EFFECT_JSON__` marker landed mid-line after libtest's
newline-less `test <name> ... ` preamble).

This is consistent with the codebase state recorded in CLAUDE.md / memory: the
17 translation gotchas are documented as **fixed**, and the one OPEN bug (#313)
lives on the **effect** path — lib-module `map`/`filter` over `lines` of a
*partially-consumed effect tuple* — which a pure source generator cannot reach.
That cell is the sibling **W4 (proptest-lazy-consumption)** workstream's target.

## Bug classes (what would be reported)

| Class | Meaning |
|-------|---------|
| B1 | JIT result ≠ reference interpreter |
| B2 | compile or runtime error on a valid total program |
| B3 | fatal signal (SIGILL / SIGSEGV) on a total program |
| B4 | compile-twice nondeterminism, or lazy-A/B (TIDEPOOL_LAZY_RESULTS) divergence |

## Bug table

| ID | Class | Gotcha# | Status | Minimal repro | Seed |
|----|-------|---------|--------|---------------|------|
| _none_ | — | — | No pipeline bug found in 1400+ cases | — | — |

(Harness bugs fixed in-session, for the record — NOT pipeline bugs:)

| ID | What | Fix commit |
|----|------|------------|
| H1 | `LetShadowInt` printer/eval hardcoded `v0`/`vA` while generated `Var` nodes used fresh ids → "unbound var" / un-compilable source | `e671b33` (node carries real ids) |
| H2 | effect-worker `__EFFECT_JSON__` marker mis-parsed: libtest `--nocapture` emits `test <name> ... ` with no trailing newline, so the marker was mid-line and `strip_prefix` never matched (every effect case mis-flagged B2/B3) | `0f9f8a8` (substring match + leading newline) |

## Properties

| Property | Cases | Oracle | Status |
|----------|-------|--------|--------|
| `reference_self_check_1000` | 1000 | reference determinism + totality (no GHC, instant) | ✅ |
| `reference_text_semantics_pinned` | — | hand-computed `words`/`unwords`/`strip`/`show` | ✅ |
| `reference_algebraic_identities` | 400 | `reverse∘reverse=id`, `length∘map=length` | ✅ |
| `committed::pure_reference_x30` | 30 | JIT == reference (B1/B2) | ✅ |
| `committed::determinism_x30` | 30 | compile twice, fresh nonce → equal (B4) | ✅ |
| `committed::effectful_lazy_ab_x8` | 8 | subprocess LAZY=1 vs =0 vs reference (B1/B3/B4) | ✅ |
| `long_haul` (#[ignore]) | 300/run | pure, B1/B2; hand-run, env-tunable depth | ✅ |
| `coverage_census` (#[ignore]) | 5000 | construct-frequency tally (below) | ✅ |

Committed suite (all three) runs green in **339 s** (~5.6 min) with
`--test-threads=1`. The effectful property is the cost driver: each case spawns
two subprocesses that each JIT-compile the full MCP effect preamble (~10–30 s),
so it is capped at 8 cases (a 30-case version cost ~33 min). Deep effect-path
hunting is W4's job; this property is a bonus A/B oracle.

## Idiom coverage census (5000 programs, default depth 3–4)

**Programs containing a join-point / recursion / shadow shape: 2190/5000
(43.8%)** — meets the ≥40% design target.

Result types: `Int` 2225, `ListInt` 1025, `Text` 974, `Pair` 776.

Key construct occurrences (full list emitted by `coverage_census`):

| Construct | Count | Gotcha exercised |
|-----------|------:|------------------|
| `WhereGo(sig)` | 1520 | #1 joinrec→LetRec, #6 join arity, #10 join across lambda |
| `WhereGo(unsigned)` | 372 | #3 Integer-defaulting probe |
| `CaseOfCase` | 1853 | #2 tagToEnum#, source-level join factory |
| `Guard(sig)` | 1507 | join points from multi-way guards |
| `Guard(unsigned)` | 363 | #3 Integer-defaulting probe |
| `LetShadowInt` | 1890 | #9 LetRec phasing, env save/restore, occurrence analysis |
| `Var` | 1113 | env capture / shadowing |
| `ConcatMapI` | 507 | nested list laziness |
| `TakeIterate` | 590 | `take n (iterate f x)` — infinite-list laziness |
| `ShowInt` | 531 | `show :: Int → Text` (Text-returning shadow) |
| `FoldlAdd`/`Sum`/`Product` | 913/957/958 | strict fold / poison-closure paths |
| text ops (`Append`,`ToUpper`,`Strip`,`TReverse`,`Unwords`,`Words`) | ~3200 | Text-vs-String repr (#7 unpackCString#, #12 box mismatch) |

Per-construct generation is type-directed (Int/Text/[Int]/[Text]/(Int,Text)),
depth-bounded, every program total by construction (no division, `head []`,
`error`, or `read`). Arithmetic uses i64 **wrapping** in the reference to match
GHC machine `Int#`.

## Long-haul session log (2026-06-10)

| Round | Depth | Knobs | Base seed | Cases | Failures | Wall |
|-------|-------|-------|-----------|------:|---------:|------|
| 1 | 3–4 | — | `0xC0FFEE` | 300 | 0 | ~6 min |
| 2 | 3–4 | — | `987654321` | 300 | 0 | ~6 min |
| A | 6 | `PIPELINE_DEPTH=6` | `0xBEEF` | 400 | 0 | 1093 s |
| B | 6 | `PIPELINE_DEPTH=6 PIPELINE_FORCE_UNSIGNED=1` | `0xD00D` | 400 | 0 | 1369 s |

**Total: 1400 pure cases, 0 failures.** No B1/B2 mismatches, no B3 signals, no
stderr breadcrumbs (`undefined forced`, `CASE TRAP`, `SIGILL`) observed on any
total program.

Re-run any round by hand:

```
PIPELINE_DEPTH=6 LONGHAUL_BASE=48879 LONGHAUL_N=400 \
  cargo test -p tidepool-runtime --test proptest_haskell_pipeline \
  -- --ignored --test-threads=1 long_haul --nocapture
```

`PIPELINE_FORCE_UNSIGNED=1` forces every `WhereGo`/`Guard` helper to omit its
type signature (maximizes the gotcha-#3 Integer-defaulting trap); even so, with
`default (Int, Text)` GHC pins these helpers to `Int`, so the trap did not fire
— consistent with the GHC-specialization notes in memory.

## Interpretation & where the bugs actually are

A clean 1400-case sweep is a meaningful *positive* signal for `Translate.hs` on
the **pure** surface: joinrec/letrec lowering, case-of-case join factories,
multi-way guards, nested shadowing lets, lazy list combinators, and the
Text/String boundary all round-trip correctly through GHC -O2 → Core → JIT.

The remaining live risk is the **effect/laziness path** (#313 and neighbors),
which this pure generator structurally cannot reach. The effectful A/B oracle
here (8 cases, sizes straddling the 2000-element lazy threshold) found no
divergence, but its coverage is intentionally shallow — the deep matrix
(TupleStringList × MapFilterPrefix/LinesOfPartial × just-over-threshold, stream
chunk fenceposts, two interleaved parked streams) belongs to W4.

**Recommendation:** treat the pure pipeline as well-covered by this harness; the
next marginal bug-hunting hour is better spent in W4's effect/laziness matrix
than widening this pure generator further.
