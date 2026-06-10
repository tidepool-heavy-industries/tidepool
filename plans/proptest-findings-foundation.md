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

## Confirmed bugs

<!-- one row per distinct bug; dedup = repros differ only in literals/arity -->

| ID | Class | Component | Shrunk repro (#[ignore]) | Seed | Observed / Expected |
|----|-------|-----------|--------------------------|------|---------------------|
| _(hunt in progress)_ | | | | | |

## Hunt log

<!-- filled from hunt runs: cases run per property, outcomes -->
