//! F2 end-to-end: a user-defined `data Union a b = Union a b`, constructed and
//! pattern-matched inside an EFFECTFUL computation, must not be confused with
//! freer-simple's own `Union` effect-wrapper by `normalize.rs`'s
//! `transform_canonicalize_effect_tag` (plan 05 F2, `tidepool-repr/src/normalize.rs`).
//!
//! Before the fix, the pass resolved the freer `Union` via
//! `table.get_by_name_arity("Union", 2)` — which returns the LAST-INSERTED
//! match on a bare-name collision, not necessarily the real freer-simple
//! constructor. A user program defining its own two-field `Union` risked the
//! pass canonicalizing the WRONG `Con`: either corrupting the user's own value
//! (splicing a raw `LitWord` where the user's field is a genuine boxed `Int`)
//! or leaving the real effect tag boxed (which the JIT's codegen requires
//! unboxed for cheap dispatch) — a production-path bug, `debug_assert`ed
//! downstream, so silent in release. Post-fix, `freer_names::resolve` picks
//! the freer `Union` by its module-qualified name regardless of insertion
//! order or the user's own `Union` being present in the same `DataConTable`.
//!
//! This program performs a real effect (so the freer machinery's own `Union`
//! effect-wrapper is genuinely live and gets normalized) AND separately
//! constructs/pattern-matches a user `Union` of the same bare name and arity,
//! so both collide in the same table exactly as the bug required.
//!
//! Runs on the JIT (`tidepool_runtime::compile_and_run`, via
//! `JitEffectMachine::compile`, the only caller of `normalize()` — see
//! `tidepool-codegen/src/jit_machine.rs`). The tree-walking oracle
//! (`tidepool_eval::eval`) never calls `normalize()` at all, so this specific
//! fix has no oracle-side counterpart to differential-test here; the oracle's
//! OWN equivalent collision (`EffectMachine::new`'s constructor resolution)
//! was already fixed in plan 03 via the same `freer_names::resolve` helper,
//! prior to this work — see `repro_qq_union.rs` for that class of regression.
//!
//! Requires a worktree extract binary — skips cleanly when unavailable (see
//! `tidepool_testing::eval_harness::extract_available`).

use tidepool_testing::eval_harness::{extract_available, mock, EvalHarness};

#[test]
fn user_defined_union_survives_effectful_normalize() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract unavailable (set TIDEPOOL_EXTRACT / nix develop)");
        return;
    }
    let src = mock::mcp_module(
        "data Union a b = Union a b\n\n\
         unionSum :: Union Int Int -> Int\n\
         unionSum (Union a b) = a + b\n\n\
         result :: M Value\n\
         result = do\n\
         \x20 _ <- send (HttpGet \"x\")\n\
         \x20 pure (toJSON (unionSum (Union 3 4)))\n",
    );
    let out = EvalHarness::new()
        .with_stdlib()
        .run(&src, "result", mock::min_stack());
    match out.into_result() {
        Ok(v) => assert_eq!(
            v.to_json(),
            serde_json::json!(7),
            "user Union must evaluate correctly alongside a real effect dispatch"
        ),
        Err(e) => panic!(
            "user-defined Union regression: effectful eval failed \
             (Union tag collision in normalize?): {e}"
        ),
    }
}
