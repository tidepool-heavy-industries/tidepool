# Proptest Foundation-Depth Findings (W1)

Lifting the depth-3 generator cap: stack-safe comparison, depth/weight-parameterized
strategies, a deep differential hunt instrument, and a tightened differential.

## What changed

| Area | Before | After |
|------|--------|-------|
| `compare::values_equal` / `heap_to_value` / `contains_closure` | recursive (overflowed host stack on deep trees → forced the depth-3 cap) | explicit `Vec` worklists (stack-safe) |
| `proptest::values_equal` | recursive | worklist |
| Generators | `arb_core_expr` / `arb_ground_expr` hardcoded depth 3 | added `arb_core_expr_depth`, `arb_ground_expr_depth`, `arb_core_expr_weighted` (weights threaded via `Context`); defaults unchanged |
| `proptest_differential` | `jit_only_error` silently counted; floor `compared >= 50` | non-whitelisted JIT-only error = **B2 failure**; floor `compared >= 120` (measured ~169/200) |
| Deep differential | none | `proptest_deep_differential.rs`: 4 properties × 200 cases, subprocess-contained |

The depth-3 cap is no longer required for stack safety. `arb_core_expr()` behavior is
byte-for-byte unchanged (`Weights::default` == the prior hardcoded `prop_oneof!` weights),
so the ~19 dependent test files keep their runtime envelope.

## Generator reach (300 samples each)

| Strategy | nodes/expr (avg) | Join/expr | Jump/expr | LetRec/expr | Case/expr |
|----------|------------------|-----------|-----------|-------------|-----------|
| `arb_ground_expr_depth(5)` | ~42 | ~0.8 | ~0.8 | ~0.8 | ~1.6 |
| `arb_core_expr_weighted(7,5,4,4)` | ~718 | ~36 | ~36 | ~29 | ~28 |

Depth-7 weighted produces ~718-node expressions dense in Join/LetRec/Case — precisely the
regime that overflowed the old recursive comparison. Measured via `generator_reach_stats`.

## Differential reach (measured, depth-3 ground, 200 cases)

`compared≈169, jit_only_error≈31 (all UnresolvedVar / synthetic-LetRec),
both_error≈0, eval_only_error≈0, deep_force_fail≈0`. Floor set to 120 (≈30% margin)
so a coverage regression fails while seed variance does not.

## Known-divergence filter (NOT bugs)

- Eval-side error / both error → skip (interpreter laziness gap; JIT is eager).
- Deep-force failure → skip.
- JIT compile failure on synthetic IR → skip.
- Whitelisted JIT runtime errors → skip: `UnresolvedVar` (synthetic `LetRec` with simple
  inter-referencing RHS — interpreter thunks, JIT evaluates sequentially; GHC never emits
  this), `HeapOverflow` (nursery exhausted after GC), `StackOverflow` (eager-eval gap).

## Reportable taxonomy

- **B1** both succeed, deep-forced values differ.
- **B2** JIT-only error outside whitelist when eval succeeded.
- **B3** any fatal signal (SIGSEGV/SIGILL/SIGBUS/SIGABRT) — always.
- **B4** divergence under a semantics-preserving knob (nursery size, optimize-vs-not).
- **B5** roundtrip non-identity.

## Outcome: no confirmed eval/JIT bugs at depth 5/7

The deep differential hunt (multiple passes, fresh seeds, up to 200 cases/property
at depth 5 and 7) surfaced **no confirmed tidepool eval/JIT bug**. Every divergence
observed traced to a known synthetic-IR class that GHC never emits and that the
backends are not contracted to agree on. Reporting any of them as a tidepool bug
would be incorrect. What the hunt *did* produce:

### Divergence classes found (all non-bugs, all characterized + filtered)

| ID | Class | Trigger | Resolution | Seed | `#[ignore]`d characterization |
|----|-------|---------|------------|------|-------------------------------|
| D1 | HeapBridge `UnexpectedHeapTag` | depth-5 synthetic expr → JIT result is an unreduced garbage heap object (tag 0); eval yields a ground `Pair` | production `run_pure`/`heap_bridge` rejects it → whitelisted `HeapBridge`; worker uses `run_pure` as oracle | `tests/deep_differential_seeds/heapbridge_unexpected_tag_d5.hex` | `diag_heapbridge_unexpected_tag` |
| D2 | Synthetic-LetRec value divergence | depth-7 `LetRec` with bare-`Var` rec RHS (`let rec a = a`); both backends succeed with ground values that differ (`Con(0,[])` vs `Con(4,[Int,Word])`) | documented `UnresolvedVar` class manifesting as a value diff; filtered via `has_synthetic_letrec` | `tests/deep_differential_seeds/synthetic_letrec_d7.hex` | `repro_synthetic_letrec_divergence` |

Dedup note: D1 is an *error*-class divergence (JIT can't produce a value), D2 is a
*value*-class divergence (both produce values that differ). Distinct repros, distinct
mechanisms.

### Harness robustness fix (found while triaging D1)

This crate's raw `compare::heap_to_value` silently mis-decodes an unexpected tag-0
heap object as a `Closure` (because `TAG_CLOSURE == 0`), turning a synthetic-IR
non-result into a bogus `Pair != Closure` B1. The production `heap_bridge` instead
rejects tag-0 as `UnexpectedHeapTag`. Fix: the deep-diff worker uses the production
`JitEffectMachine::run_pure` path as its JIT oracle (correct reconstruction under GC,
correct synthetic-IR rejection), and skips closure-valued results. `heap_to_value`
itself is unchanged in behavior (still used by `proptest_differential` on clean
depth-3 ground results) — only made stack-safe.

## Hunt log

Final validation pass (200 cases/property, all GREEN, no host stack overflow,
no timeout, no infra error):

| Property | depth | knob | compared | skipped |
|----------|-------|------|----------|---------|
| `deep_diff_ground_depth5` | 5 ground | 64KB | 148 | 52 |
| `deep_diff_ground_depth5_optimized` | 5 ground | optimize | 143 | 57 |
| `deep_diff_ground_depth5_small_nursery` | 5 ground | 4KB | 145 | 55 |
| `deep_diff_join_letrec_heavy_depth7` | 7 weighted | 64KB | 22 | 178 |

Skips are dominated by the known-divergence filter (synthetic-LetRec, HeapBridge,
eval-side laziness gaps, closure-valued results). Depth-7's high skip rate reflects
the prevalence of synthetic-LetRec at `letrec_w=4`; its primary role per the done
criteria is validating stack-safety + crash-freedom at depth 7, which it does.

### Earlier passes

- depth-5 ground (plain + optimized, 64KB): clean across 80 + 120 cases each.
- depth-5/4KB first pass: B1 — later proven a harness false positive (D1 above).
- depth-7 first pass: B1 — later proven the synthetic-LetRec class (D2 above).
- `proptest_differential` tightening: green at depth-3; the 31/200 `jit_only_error`
  cases are all whitelisted `UnresolvedVar`.
