//! Fault-injection acceptance test for the GC signal-recovery path.
//!
//! Invariants this file gates, exercised through the real production entry
//! (`JitEffectMachine::compile` + `run_pure`, which installs signal handlers,
//! sets up `RegistryGuard`, and wraps the JIT call in `with_signal_protection`):
//!
//! - A `SIGILL` raised inside `perform_gc`'s Cheney-copy window, at either
//!   fault point (`GcFaultPoint::DuringCopy` right before the copy,
//!   `AfterCopy` right after it), surfaces as
//!   `JitError::Yield(YieldError::Signal(sig))` with `sig == libc::SIGILL` —
//!   an exact match, not a loose `is_err()` (which would equally accept a
//!   compile error, an OOM, or an unrelated signal).
//! - `RegistryGuard::drop` completes without panicking. `GcState` is TAKEN
//!   out of its cell before the fault window and left EMPTY when the fault
//!   abandons it; every teardown path already treats an empty cell as the
//!   ordinary "no GC state" case, so `clear_run_scratch` has nothing to
//!   double-borrow. A test reaching its own end past the fault assertion IS
//!   the proof: a panic inside `Drop` here would propagate out of `run_pure`
//!   and surface as this test failing on a panic instead of matching the
//!   clean `Err` — or, nested under an already-active unwind, abort the
//!   whole test process rather than fail an assertion.
//! - A fresh `JitEffectMachine`, compiled and run in the SAME process right
//!   after a faulted run, still reaches `perform_gc` and produces the
//!   correct value: the faulted machine's own abandoned heap state (its
//!   to-space buffer leaked on the dead stack frame, its `GcState` cell left
//!   empty) does not poison the process for anyone else.
//!
//! Fault injection uses the process-global one-shot hook
//! (`host_fns::arm_gc_fault` / `GcFaultPoint`, see `host_fns/gc.rs`). Every
//! test here proves its program actually reaches `perform_gc` BEFORE arming
//! (via `gc_trigger_call_count`) — a program that never triggers a
//! collection would make the fault assertions pass vacuously.
//!
//! `nextest` isolates tests in separate processes, while ordinary `cargo
//! test` runs this integration binary's tests in parallel. The `serial`
//! guards below make both runners honor the process-global hook's actual
//! ownership contract.

use serial_test::serial;

use tidepool_codegen::host_fns::{
    self, arm_gc_fault, gc_trigger_call_count, reset_test_counters, GcFaultPoint,
};
use tidepool_codegen::jit_machine::{JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_eval::value::Value;
use tidepool_repr::types::{Alt, AltCon, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};
use tidepool_testing::proptest::build_table_for_expr;

// This file only needs the cons-spine builders; `gc_scaffold`'s Pair-tree
// tags/builders (used by its other consumers, `heap_verify_lane.rs` and
// `proptest_gc_recursion.rs`) are unused here.
#[allow(dead_code)]
use crate::gc_scaffold;
use gc_scaffold::{fixup_root, fresh_var, push_spine, reset_ctr, CONS, NIL};

const NURSERY_SIZE: usize = 2 * 1024;
const SPINE_LEN: i64 = 300;

struct DiagnosticOverrides;

impl DiagnosticOverrides {
    fn enabled() -> Self {
        host_fns::set_gc_poison(true);
        host_fns::set_heap_verify(true);
        Self
    }
}

impl Drop for DiagnosticOverrides {
    fn drop(&mut self) {
        host_fns::clear_gc_poison_override();
        host_fns::clear_heap_verify_override();
    }
}

/// Tail-recursive sum fold over a cons-spine, expressed as a self-recursive
/// `LetRec` lambda (JIT tail-call-optimizes it): same shape as
/// `heap_verify_lane.rs`'s `push_sum_fold`, copied rather than shared so this
/// file's builder stays self-contained within its own test boundary.
fn push_sum_fold(b: &mut TreeBuilder, list_var: VarId) -> usize {
    let go = fresh_var();
    let acc = fresh_var();
    let xs = fresh_var();

    let xs_v = b.push(CoreFrame::Var(xs));
    let case_binder = fresh_var();
    let h = fresh_var();
    let t = fresh_var();

    let nil_body = b.push(CoreFrame::Var(acc));

    let av = b.push(CoreFrame::Var(acc));
    let hv = b.push(CoreFrame::Var(h));
    let combined = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![av, hv],
    });
    let go_v = b.push(CoreFrame::Var(go));
    let app1 = b.push(CoreFrame::App {
        fun: go_v,
        arg: combined,
    });
    let t_v = b.push(CoreFrame::Var(t));
    let recur = b.push(CoreFrame::App {
        fun: app1,
        arg: t_v,
    });

    let case_node = b.push(CoreFrame::Case {
        scrutinee: xs_v,
        binder: case_binder,
        alts: vec![
            Alt {
                con: AltCon::DataAlt(NIL),
                binders: vec![],
                body: nil_body,
            },
            Alt {
                con: AltCon::DataAlt(CONS),
                binders: vec![h, t],
                body: recur,
            },
        ],
    });

    let inner_lam = b.push(CoreFrame::Lam {
        binder: xs,
        body: case_node,
    });
    let go_lam = b.push(CoreFrame::Lam {
        binder: acc,
        body: inner_lam,
    });

    let go_b = b.push(CoreFrame::Var(go));
    let seed = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let call1 = b.push(CoreFrame::App {
        fun: go_b,
        arg: seed,
    });
    let list_v = b.push(CoreFrame::Var(list_var));
    let call2 = b.push(CoreFrame::App {
        fun: call1,
        arg: list_v,
    });

    b.push(CoreFrame::LetRec {
        bindings: vec![(go, go_lam)],
        body: call2,
    })
}

/// A cons-spine tail-summed via `push_sum_fold`, sized to force multiple
/// collections at `NURSERY_SIZE` — mirrors `heap_verify_lane.rs`'s
/// `cons_spine_sum` case, which already relies on this exact shape/size pair
/// to exercise `perform_gc` repeatedly.
fn build_program() -> (CoreExpr, i64) {
    reset_ctr();
    let elems: Vec<i64> = (0..SPINE_LEN).collect();
    let expected = elems.iter().sum();

    let mut b = TreeBuilder::new();
    let spine = push_spine(&mut b, &elems);
    let lst = fresh_var();
    let fold = push_sum_fold(&mut b, lst);
    let root = b.push(CoreFrame::LetNonRec {
        binder: lst,
        rhs: spine,
        body: fold,
    });
    let mut tree = b.build();
    (fixup_root(&mut tree, root), expected)
}

fn compile_and_run(expr: &CoreExpr) -> Result<Value, JitError> {
    let table = build_table_for_expr(expr);
    let mut machine = JitEffectMachine::compile(expr, &table, NURSERY_SIZE)
        .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
    machine.run_pure()
}

/// Run `expr` to completion (unarmed) and assert both the correct result and
/// that `perform_gc` actually ran — the guard against a vacuous pass.
fn assert_program_forces_gc_and_is_correct(expr: &CoreExpr, expected: i64, label: &str) {
    reset_test_counters();
    let before = gc_trigger_call_count();
    let result = compile_and_run(expr).unwrap_or_else(|e| panic!("{label}: run failed: {e:?}"));
    let after = gc_trigger_call_count();
    assert!(
        after > before,
        "{label}: gc_trigger_call_count did not increase ({before} -> {after}) — \
         this program never reached perform_gc, so a fault armed on it would pass vacuously"
    );
    match result {
        Value::Lit(Literal::LitInt(n)) => assert_eq!(n, expected, "{label}: wrong result"),
        other => panic!("{label}: expected LitInt, got {other:?}"),
    }
}

/// Arm `point`, run the fault-forcing program through the real production
/// entry, and assert the fault surfaces as a precise `SIGILL` signal error.
fn assert_fault_yields_clean_sigill(point: GcFaultPoint, expr: &CoreExpr) {
    arm_gc_fault(point);
    let result = compile_and_run(expr);
    match result {
        Err(JitError::Yield(YieldError::Signal(sig))) => {
            assert_eq!(sig, libc::SIGILL, "expected SIGILL, got signal {sig}");
        }
        other => panic!(
            "expected Err(JitError::Yield(YieldError::Signal(SIGILL))) for {point:?}, got {other:?}"
        ),
    }
    // Reaching this point is the proof that `RegistryGuard::drop` did not
    // panic during the `Err` early-return inside `run_pure` — see the module
    // docstring.
}

#[test]
#[serial]
fn during_copy_fault_surfaces_clean_signal_and_process_stays_usable() {
    let (expr, expected) = build_program();
    assert_program_forces_gc_and_is_correct(&expr, expected, "during_copy baseline");

    let (fault_expr, _) = build_program();
    assert_fault_yields_clean_sigill(GcFaultPoint::DuringCopy, &fault_expr);

    // State hygiene: a FRESH machine, compiled and run in this same process
    // right after the faulted run, must still reach perform_gc and produce
    // the correct value.
    let (fresh_expr, fresh_expected) = build_program();
    assert_program_forces_gc_and_is_correct(
        &fresh_expr,
        fresh_expected,
        "during_copy post-fault fresh machine",
    );
}

#[test]
#[serial]
fn after_copy_fault_surfaces_clean_signal_and_process_stays_usable() {
    let (expr, expected) = build_program();
    assert_program_forces_gc_and_is_correct(&expr, expected, "after_copy baseline");

    let (fault_expr, _) = build_program();
    assert_fault_yields_clean_sigill(GcFaultPoint::AfterCopy, &fault_expr);

    let (fresh_expr, fresh_expected) = build_program();
    assert_program_forces_gc_and_is_correct(
        &fresh_expr,
        fresh_expected,
        "after_copy post-fault fresh machine",
    );
}

/// Same DuringCopy fault, with both diagnostic knobs forced on via the
/// process-global test setters (independent of whatever the environment has
/// set — see `TIDEPOOL_GC_POISON`/`TIDEPOOL_HEAP_VERIFY` in the crate
/// `CLAUDE.md`). The baseline and post-fault runs exercise real collections
/// with poison-writes and the post-GC verifier both active, proving neither
/// knob perturbs the clean-recovery behavior.
#[test]
#[serial]
fn during_copy_fault_is_clean_under_gc_poison_and_heap_verify() {
    let _overrides = DiagnosticOverrides::enabled();

    let (expr, expected) = build_program();
    assert_program_forces_gc_and_is_correct(&expr, expected, "poison+verify baseline");

    let (fault_expr, _) = build_program();
    assert_fault_yields_clean_sigill(GcFaultPoint::DuringCopy, &fault_expr);

    let (fresh_expr, fresh_expected) = build_program();
    assert_program_forces_gc_and_is_correct(
        &fresh_expr,
        fresh_expected,
        "poison+verify post-fault fresh machine",
    );
}
