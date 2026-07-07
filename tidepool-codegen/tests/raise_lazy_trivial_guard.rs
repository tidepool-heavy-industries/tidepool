//! M2 (repo-review-2026-07-06/01-gc-memory-safety.md, Medium findings):
//! `is_trivial_field` classified `PrimOp Raise` with a trivial arg (or no
//! arg) as trivial, so a `LetNonRec` binding a `raise#` RHS evaluated it
//! EAGERLY — right at the `let`, unconditionally — instead of only when the
//! binder is actually forced. `let x = raise# e in if False then x else 0`
//! must return 0 (GHC Core `let` is non-strict; only `case` forces), but
//! raised instead because the eager path calls `runtime_error` as soon as
//! the RHS is compiled-and-run, before the (never-taken) branch that would
//! reference `x` is even reached.
//!
//! Fixed: `PrimOpKind::Raise => false` in `is_trivial_field`, so a `raise#`
//! RHS always thunkifies and only raises on demand.
//!
//! Uses the `JitEffectMachine`/`run_pure` harness (not the bare
//! compile-and-call-the-fn-pointer harness some other emit tests use) —
//! `host_fns::take_runtime_error`/`RuntimeError` state is only meaningful
//! once `CURRENT_MACHINE` is installed, which happens inside
//! `run_pure`/`RegistryGuard`, not in the bare harness.

use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_eval::value::Value;
use tidepool_repr::types::{Alt, AltCon, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};
use tidepool_testing::proptest::build_table_for_expr;

/// `let x = raise# in case 0# of { 0# -> 0#; DEFAULT -> x }`
///
/// Mirrors `let x = raise# ex in if False then x else 0` — `x` is bound but
/// the taken branch never references it. Must return 0, not raise.
fn build_tree() -> CoreExpr {
    let x = VarId(1);
    let scrut = VarId(2);

    let mut b = TreeBuilder::new();
    let raise_rhs = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::Raise,
        args: vec![],
    });
    let scrutinee = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let taken_branch = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let not_taken_branch = b.push(CoreFrame::Var(x));
    let case = b.push(CoreFrame::Case {
        scrutinee,
        binder: scrut,
        alts: vec![
            Alt {
                con: AltCon::LitAlt(Literal::LitInt(0)),
                binders: vec![],
                body: taken_branch,
            },
            Alt {
                con: AltCon::Default,
                binders: vec![],
                body: not_taken_branch,
            },
        ],
    });
    b.push(CoreFrame::LetNonRec {
        binder: x,
        rhs: raise_rhs,
        body: case,
    });
    b.build()
}

#[test]
fn letnonrec_raise_rhs_stays_lazy_when_unused_branch() {
    let expr = build_tree();
    let table = build_table_for_expr(&expr);
    let mut machine = JitEffectMachine::compile(&expr, &table, 65536)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    let result = machine.run_pure();

    match result {
        Ok(Value::Lit(Literal::LitInt(n))) => {
            assert_eq!(n, 0, "unforced `raise#` binding must not affect the result");
        }
        Ok(other) => panic!("expected LitInt(0), got {other:?}"),
        Err(JitError::Yield(_)) => {
            panic!("raise# bound by `let` but never forced must not raise (M2 regression)")
        }
        Err(e) => panic!("unexpected error: {e:?}"),
    }
}
