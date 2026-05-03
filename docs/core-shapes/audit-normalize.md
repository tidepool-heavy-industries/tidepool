# Audit: Core normalization pass (`tidepool-repr/src/normalize.rs`)

The normalization pass runs `CoreExpr → CoreExpr` between translation and codegen. Its purpose is to canonicalize shape divergence that arises from cross-module compilation: GHC's optimizer runs per-module, so cross-module inlining via `resolveExternals` can leave shapes unoptimized that single-module compilation would have collapsed.

The pass is bounded (max 100 fixpoint iterations) and idempotent. Each rule is conservative: it only rewrites shapes it can prove safe.

## Pipeline placement

- **Location:** `tidepool-codegen/src/jit_machine.rs:130` (`JitEffectMachine::compile`)
- **Order:** `tidepool_repr::normalize(expr, table) → wrap_with_datacon_env(expr, table) → emit::compile_expr`
- **Rationale:** Normalization runs before `wrap_with_datacon_env` so the wrapped DataCon refs (which `wrap` introduces) don't need re-normalization.

## Rule 1: flatten_box

- **Location:** `tidepool-repr/src/normalize.rs:136` (`try_flatten_box`)
- **Trigger shape:** `Con(BOX, [Con(BOX, [inner])])` where `BOX ∈ {I#, W#, C#, F#, D#}` and inner Con has matching tag
- **Rewrites to:** `Con(BOX, [inner])` (collapses one layer)
- **Assumes:** Box constructors always have arity 1.
- **Mode:** `always-on`
- **Test coverage:** `tidepool-repr/src/normalize.rs::tests::flatten_nested_int_boxes`; proptest `prop_idempotence`
- **Notes:** Cross-module inlining can produce nested boxes when the wrapper for a primitive constructor is re-applied. Fixpoint folding handles arbitrarily deep nesting.

## Rule 2: canonicalize_effect_tag

- **Location:** `tidepool-repr/src/normalize.rs:194` (`transform_canonicalize_effect_tag`)
- **Trigger shape:** `Con(Union, [W#(x_or_var), payload])` where `x_or_var` resolves to a `Lit(LitWord, _)` via `resolve_var`
- **Rewrites to:** `Con(Union, [Lit(LitWord, n), payload])` — strips the `W#` boxing so `effect_machine` reads the raw position index without indirection
- **Assumes:** `Union` has arity 2; `W#` has arity 1; effect tag is always `LitWord`.
- **Mode:** `always-on`
- **Test coverage:** `tidepool-repr/src/normalize.rs::tests::effect_tag_canonicalized`, `effect_tag_canonicalized_through_var`; cross-mode integration via `tidepool-runtime/tests/cross_mode_targeted.rs::dimension_a1_single_gadt_dispatch`
- **Notes:** `effect_machine.rs:~245` retains a runtime fallback that peels boxed `W#` if Rule 2 missed the shape (e.g. deeply Var-indirected). PR #289 originally removed the fallback under the canonical-shape assumption; PR #293 restored it after the cross-mode harness caught a missed shape (`Var → Lit` chain through nested lets).

## Rule 3: unbox_prim_args

- **Location:** `tidepool-repr/src/normalize.rs:163` (`transform_unbox_prim_args`)
- **Trigger shape:** `PrimOp { args }` where ALL args are `Con(BOX, [Lit(_)])` for known box constructors
- **Rewrites to:** `PrimOp { args: [Lit(_)..] }` — peels the boxing on each arg
- **Assumes:** All-or-nothing — any non-boxed arg cancels the rewrite for the whole call.
- **Mode:** `always-on`
- **Test coverage:** `tidepool-repr/src/normalize.rs::tests::prim_args_unboxed_when_all_boxed`, `prim_args_not_unboxed_when_mixed`; semantics-preservation proptest in `tidepool-testing/tests/normalize_semantics.rs`
- **Notes:** Heuristic conservatism — `PrimOp { args: [Lit, Var] }` could often be unboxed via local let-binding inspection but isn't, since the all-or-nothing rule is provably semantics-preserving and easy to audit.

## resolve_var fuel

- **Location:** `tidepool-repr/src/normalize.rs:114` (`resolve_var`)
- **Limit:** 10 chained `Var → Var → ...` resolutions
- **Mode:** `always-on`
- **Failure mode:** Returns the last resolved index; caller treats unresolved Var as opaque.
- **Notes:** Defensive against pathological let-chains and accidental cycles in the normalization input. 10 is generous — typical real-world chains are 1-2 deep.

## Fixpoint limit

- **Location:** `tidepool-repr/src/normalize.rs:47` (top-level `normalize` loop)
- **Limit:** 100 iterations
- **Mode:** `always-on`
- **Failure mode:** `debug_assert!(false, "normalize did not reach fixpoint within 100 iterations")` in debug builds; returns last result in release.
- **Test coverage:** Proptest `prop_bounded_iteration` confirms convergence within bound for arbitrary CoreExpr.
- **Notes:** All current rules are guaranteed to converge within ~3 iterations on real programs. The bound exists to surface bugs where a future rule introduces oscillation.

## Properties

- **Idempotent:** `normalize(normalize(x)) == normalize(x)` — proptest `prop_idempotence`.
- **Semantics-preserving:** `eval(x) == eval(normalize(x))` for closed expressions — proptest `tidepool-testing/tests/normalize_semantics.rs::prop_normalize_preserves_semantics` (PR #294).
- **Bounded:** Always terminates within the fixpoint limit — proptest `prop_bounded_iteration`.

## Coverage gaps

- **Rule 2 effectiveness on real cross-mode programs is unverified.** The runtime fallback in `effect_machine.rs:~245` is what currently makes cross-mode tests pass; it's unknown how often Rule 2 actually fires vs falling through. Adding a debug-only counter would surface this. (Issue: file as TODO; see dossier `core-shapes.md` Coverage Gaps.)
- **Rule 3's all-or-nothing heuristic** is conservative; mixed-arg cases could be peeled with more sophistication. Not a correctness issue, only optimization quality.
- **No proptest specifically exercises depth-N `Var → Lit` chains** through Rule 2 — only the depth-1 case PR #293 added.
