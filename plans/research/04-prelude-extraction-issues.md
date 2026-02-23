# Research: Prelude Extraction Issues and Fix Strategy

**Priority:** HIGH — Blocks extraction of standard library functions and complex user code
**Status:** IN-PROGRESS
**Date:** 2026-02-20

## Summary

The Tidepool compiler's Prelude extraction process currently suffers from five distinct but interrelated issues that prevent a "closed" translation of standard Haskell programs. These issues range from interference between manual desugaring and the resolution model, to GHC-internal metadata causing resolution failures, and unboxed tuple returns from worker-wrapper transformations.

This report analyzes these issues and proposes a move toward a "Trusted Boundary" model where the Tidepool Prelude is the source of truth, and references to `base` are strictly controlled or desugared into runtime primitives.

---

## Issue 1: `isDesugaredVar` Interference

### Findings
`Tidepool.Translate` contains a list of "desugared variables" (`eqString`, `++`, `unpackCString#`, etc.) that are manually rewritten into Core-like structures during translation. 

1. **Redundancy:** Since the introduction of `Tidepool.Prelude`, many of these functions (like `eqString` and `++`) have high-quality implementations available in `prBinds`.
2. **Interference:** The manual desugaring triggers *even if* a valid implementation is found in `prBinds`. This makes the Prelude implementation dead code and bloats every call site with an inlined copy of the logic.
3. **Shadowing Bug:** The current check is based on `occNameString`, meaning a local variable or parameter named `eqString` will be incorrectly rewritten into a recursive string comparison.

### Empirical Evidence
- `test_eq_string_local`: A function `testLocal eqString = eqString "a" "a"` results in 38 nodes instead of ~12 because the parameter is desugared.
- Node counts for `reverse "a" == "b"` are significantly higher than expected due to inlined `eqString` logic at every `==` site.

### Recommendation
- **Restrict Desugaring:** Only desugar if the variable is a `GlobalId` AND lacks an implementation in `prBinds`.
- **Prefer Prelude:** Move logic for `eqString` and `++` entirely into `Tidepool.Prelude` and remove them from `isDesugaredVar`.
- **Wired-in Only:** Keep `isDesugaredVar` only for magic built-ins that have no Haskell representation (e.g., `unpackCString#`).

---

## Issue 2: `error` and `undefined` Lacking Unfoldings

### Findings
Functions like `head`, `tail`, and `minimum` depend on `error` from `base`. GHC does not provide unfoldings for `error` or `undefined` in the `.hi` files, causing `resolveExternals` to fail.

1. **Extraction Failure:** In `--all-closed` mode, any function transitively using `error` is skipped.
2. **SIGSEGV:** In `--target` mode, if `isDesugaredVar` doesn't catch it, an unresolved `NVar` is emitted, which crashes the evaluator.

### Empirical Evidence
- `investigate-allclosed-prelude.tmp.md`: `head` is skipped because of `GHC.Internal.Err.error`.
- `investigate-error-removal.tmp.md`: Replacing `error` with an infinite loop `head [] = head []` allows `head` to be successfully extracted (14 nodes).

### Recommendation
- **Synthetic Error Node:** `Tidepool.Translate` already maps `divZeroError` and `overflowError` to a special `NVar` with tag `'E'`. Extend this to `GHC.Err.error` and `GHC.Err.undefined`.
- **Prelude Stubs:** Provide `error :: String -> a` in `Tidepool.Prelude` that calls a primitive `error#` or a known synthetic binder.

---

## Issue 3: `krep$Constraint` Metadata

### Findings
GHC 9.x generates `KindRep` metadata for typeclass constraints (Eq, Ord, etc.) to support `Typeable`. These appear as `krep$Constraint2`, `krep$Constraint1`, etc.

1. **Resolution Failure:** These symbols are internal to `base` and lack unfoldings.
2. **Blocking:** This prevents the extraction of any function with a class constraint, including `sort`, `nub`, and `isPrefixOf`.

### Empirical Evidence
- `investigate-which-tests-pass.tmp.md`: `test_plain_sort` fails with `Unresolved external: GHC.Types.krep$Constraint2`.

### Recommendation
- **Metadata Stripping:** Update `isMetadataBinder` and `reachableBinds` to aggressively skip any binder starting with `$krep` or `$tc`.
- **Type-Erasure:** Since Tidepool's runtime is type-erased, we should never need `KindRep` at runtime. Ensure `reachableBinds` does not follow these dependencies.

---

## Issue 4: Unboxed Tuple Worker-Wrapper

### Findings
GHC optimizes functions returning pairs (like `break`, `span`, `splitAt`) by creating workers that return unboxed tuples `(# a, b #)`.

1. **Runtime Incompatibility:** The Tidepool evaluator expects a single `HeapObject` return value. Unboxed tuples violate this assumption.
2. **Wrapper Handling:** While wrappers box the result back, inlining the wrapper into user code exposes the unboxed tuple `case` expression to the translator.

### Empirical Evidence
- `investigate-words-break-unboxed.tmp.md`: `break` generates `$wbreak :: ... -> (# [a], [a] #)`.
- Tests for `words` currently pass only because GHC happens not to inline the wrapper into the specific test case, but this is fragile.

### Recommendation
- **Generalized Unboxed Tuples:** Update `Tidepool.Translate` to handle `Case` over unboxed tuples generally, not just for `quotRemInt#`.
- **Alternative:** Use `-fno-worker-wrapper` for the Prelude and user code compilation to ensure uniform boxed returns. (Tradeoff: slight performance hit).

---

## Issue 5: Cross-Module Resolution Model

### Findings
The current model merges `Tidepool.Prelude` bindings into `prBinds` via `GhcPipeline`, making them "local" to the translation unit.

1. **The Gap:** `reachableBinds` collects everything needed by the target. If the Prelude is "leaky" (references `base` functions without unfoldings), the whole tree fails to close.
2. **Trusted Boundary:** We currently treat `base` as a repository of unfoldings, but it's unreliable.

### Recommendation
- **Principled Fix:** Treat `Tidepool.Prelude` as the **Trusted Boundary**. 
    - Any symbol used by the Prelude must either be defined in the Prelude, be a PrimOp, or be in a restricted "Wired-in" list (like `I#`, `C#`, `unpackCString#`).
    - Audit `Tidepool.Prelude` to ensure zero leakage to `base` for non-primitive logic.

---

## Prioritized Action Plan

1. **Phase 1: Metadata & Error (Immediate)**
    - Update `isMetadataBinder` to skip `$krep`, `$tc`, and `$trModule`.
    - Map `GHC.Err.error` and `undefined` to synthetic Error nodes in `Translate.hs`.
    - *Outcome:* `sort`, `nub`, `head`, and `tail` become extractable.

2. **Phase 2: Prelude Hardening (Short Term)**
    - Fix `eqString` in `Tidepool.Prelude` (make it recursive, fix the `eqChar` bug).
    - Remove `eqString` and `++` from `isDesugaredVar` in `Translate.hs`.
    - Add `-fno-worker-wrapper` to the `runPipeline` DynFlags.
    - *Outcome:* Cleaner code generation, resolution of `words`/`break` crashes.

3. **Phase 3: Resolution Refinement (Medium Term)**
    - Update `resolveExternals` to properly handle shadowing by checking if an ID is in the current `Rec` group before desugaring.
    - Implement a `Tidepool.Prim` module for low-level stubs to completely sever the dependency on `base` unfoldings.

4. **Phase 4: Generalized Unboxed Tuples (Long Term)**
    - If `-fno-worker-wrapper` is insufficient, implement full `Case` support for multi-value returns in the translator and evaluator.
