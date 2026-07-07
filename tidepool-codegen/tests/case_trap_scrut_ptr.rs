//! M5 (repo-review-2026-07-06/01-gc-memory-safety.md, Medium findings):
//! the Lit-dispatch case-miss path (and the fully-empty-alts path) passed
//! the unboxed scrutinee VALUE as `scrut_ptr` to `runtime_shape_trap`, which
//! dereferences it before its own null/validity check — a case-miss on an
//! unboxed `Int#` scrutinee (e.g. `case (40# +# 2#) of { 0# -> ... }` with
//! no `DEFAULT`) fed the raw value `42` in as if it were a heap address,
//! SIGSEGVing inside the very diagnostic meant to prevent a crash.
//!
//! Fixed: pass the heap pointer only when the scrutinee is actually a
//! `SsaVal::HeapPtr`; a `Raw` (unboxed) scrutinee passes 0 instead, which
//! `runtime_shape_trap`'s existing null/low-address guard already handles
//! cleanly (`RuntimeError::BadPointer`, no crash).

use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_repr::types::{Alt, AltCon, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};
use tidepool_testing::proptest::build_table_for_expr;

/// `case (40# +# 2#) of { 0# -> 0# }` — no alt matches 42, and there's no
/// `DEFAULT`, so this must hit the Lit-dispatch case-miss trap. The
/// scrutinee is a strict `PrimOp` result (kept UNBOXED as `SsaVal::Raw` by
/// the emitter, unlike a bare `Lit` node which boxes to a real heap
/// pointer) — exactly the shape that fed a non-pointer value into
/// `runtime_shape_trap`'s `scrut_ptr` pre-fix.
fn build_tree() -> CoreExpr {
    let scrut = VarId(1);
    let mut b = TreeBuilder::new();
    let forty = b.push(CoreFrame::Lit(Literal::LitInt(40)));
    let two = b.push(CoreFrame::Lit(Literal::LitInt(2)));
    let scrutinee = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![forty, two],
    });
    let alt_body = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    b.push(CoreFrame::Case {
        scrutinee,
        binder: scrut,
        alts: vec![Alt {
            con: AltCon::LitAlt(Literal::LitInt(0)),
            binders: vec![],
            body: alt_body,
        }],
    });
    b.build()
}

#[test]
fn lit_dispatch_case_miss_does_not_deref_the_unboxed_scrutinee() {
    let expr = build_tree();
    let table = build_table_for_expr(&expr);
    let mut machine = JitEffectMachine::compile(&expr, &table, 65536)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    // A case-miss on an unboxed Int# scrutinee is an ordinary compiler-bug
    // diagnostic path, not a memory-safety hazard — it must surface as a
    // clean typed `RuntimeError` (here: `BadPointer`, from the trap's own
    // null-address guard), NOT as a caught `YieldError::Signal` (meaning the
    // process actually SIGSEGV'd and `with_signal_protection` recovered via
    // `siglongjmp` — the crash this finding is about, just not fatal thanks
    // to that separate safety net).
    let result = machine.run_pure();
    match result {
        Err(JitError::Yield(YieldError::Signal(sig))) => panic!(
            "case-miss trap dereferenced a non-pointer scrutinee value and \
             SIGSEGV'd (signal {sig}); with_signal_protection caught it, but \
             the deref itself is the M5 regression"
        ),
        Err(JitError::Yield(YieldError::Runtime(_))) => {} // expected: clean typed error
        other => panic!("expected Err(Yield(Runtime(_))), got {other:?}"),
    }
}
