# W6 — Haskell → Core → JIT pipeline: differential proptest findings

Differential testing of the full `Translate.hs` → Core → Cranelift JIT pipeline
by generating small **total** Haskell source programs (a Rust AST, pretty
printed) and comparing `compile_and_run_pure(...).to_json()` against a Rust
**reference interpreter** (the oracle, total by construction).

This layer is unreachable by the Rust-side IR generators (`proptest_jit_vs_eval`
et al.) — those start from Core; this starts from Haskell source and exercises
GHC -O2 Core shapes and the 17 documented translation gotchas (CLAUDE.md).

Harness: `tidepool-runtime/tests/proptest_haskell_pipeline.rs`.

## Bug classes

| Class | Meaning |
|-------|---------|
| B1 | JIT result ≠ reference interpreter |
| B2 | compile or runtime error on a valid total program |
| B3 | fatal signal (SIGILL / SIGSEGV) on a total program |
| B4 | compile-twice nondeterminism, or lazy-A/B (TIDEPOOL_LAZY_RESULTS) divergence |

## Bug table

| ID | Class | Gotcha# | Status | Minimal repro | Seed |
|----|-------|---------|--------|---------------|------|
| _none yet_ | | | | | |

## Idiom coverage (constructs the generator emits)

- Int: literals, +/-/* (wrapping), length, sum, product, foldl' (+), negate,
  if/then/else
- **Join-point / recursion / shadow shapes (≥40% of programs):**
  - `WhereGo` — `let go 0 acc = acc; go n acc = go (n-1) (acc OP n) in go N 0`
    (joinrec/LetRec in -O2; flagged unsigned variant = Integer-defaulting probe,
    gotcha #3)
  - `CaseOfCase` — nested `case (case … of …) of …` (source-level join factory,
    gotcha #2 tagToEnum# territory)
  - `Guard` — multi-way guard function (join points; unsigned variant)
  - `LetShadowInt` — nested same-name lets (occurrence analysis / env
    save-restore, gotchas #9 #LetRec-thunk #case-binder)
- Bool conditions: comparisons, even/odd, &&/||/not
- Text: literals, `<>`, `show :: Int -> Text`, toUpper/toLower/strip/tReverse,
  unwords, if
- [Int]: literals, map, filter, take, drop, reverse, concatMap, enumFromTo,
  `take n (iterate f x)` (laziness), `++`, if
- [Text]: literals, `map show`, words, `map toUpper`, filter-by-prefix, take
- (Int, Text): pair literal (tuple → JSON array)

## Properties

| Property | Cases | Oracle |
|----------|-------|--------|
| `reference_self_check_1000` | 1000 | reference determinism + total, instant |
| `reference_text_semantics_pinned` | — | hand-computed words/unwords/strip/show |
| `reference_algebraic_identities` | 400 | `reverse∘reverse=id`, `length∘map=length` |
| `committed::pure_reference_x30` | 30 | JIT == reference (B1/B2) |
| `committed::determinism_x30` | 30 | compile twice, fresh nonce (B4) |
| `committed::effectful_lazy_ab_x30` | 30 | subprocess LAZY=1 vs =0 vs reference (B1/B3/B4) |
| `long_haul` (#[ignore]) | 300 | pure, B1/B2; hand-run |

## Long-haul session log

_(filled in during the authoring session)_
