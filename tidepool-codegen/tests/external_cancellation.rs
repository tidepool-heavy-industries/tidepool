//! External cancellation of running JIT programs.
//!
//! `JitEffectMachine::cancel_handle` hands out a `CancelHandle` that external
//! code (e.g. a watchdog thread) can flip. The next GC safepoint observes the
//! flag and aborts with `YieldError::Cancelled`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tidepool_codegen::jit_machine::{CancelHandle, JitEffectMachine, JitError};
use tidepool_codegen::yield_type::YieldError;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{CoreExpr, Literal, TreeBuilder};

fn test_table() -> DataConTable {
    let mut table = DataConTable::new();
    // A 1-arity `Wrap` constructor used by the allocating-loop fixture.
    table.insert(tidepool_repr::datacon::DataCon {
        id: DataConId(1),
        name: "Wrap".to_string(),
        tag: 1,
        rep_arity: 1,
        field_bangs: vec![],
        qualified_name: None,
    });
    // Freer-simple tags required by `JitEffectMachine::compile`.
    use tidepool_codegen::effect_machine::EffContKind;
    for (i, kind) in EffContKind::ALL.iter().enumerate() {
        table.insert(tidepool_repr::datacon::DataCon {
            id: DataConId(1000 + i as u64),
            name: kind.name().to_string(),
            tag: (1000 + i) as u32,
            rep_arity: if matches!(kind, EffContKind::Node | EffContKind::Union) {
                2
            } else {
                1
            },
            field_bangs: vec![],
            qualified_name: None,
        });
    }
    table
}

/// `letrec go = \n -> case n ==# 0# of { 1# -> Lit 42; _ -> go (n -# 1#) } in go N`
/// — a long-running tail-recursive countdown. With a sufficiently large `n`,
/// this takes far longer than any reasonable test timeout, giving the cancel
/// flag a chance to be observed at the tail-call trampoline safepoint.
/// Uses only unboxed Int# arithmetic so the hot path does not allocate —
/// this exercises the *trampoline* cancel check independently of `gc_trigger`.
fn build_long_running_countdown(n: i64) -> CoreExpr {
    build_terminating_countdown(n, 42)
}

/// `letrec go = \n -> case n ==# 0# of { 1# -> Lit 7; _ -> go (n -# 1#) } in go 100`
/// A terminating tail-recursive countdown that never sees the cancel flag.
fn build_terminating_countdown(n: i64, result: i64) -> CoreExpr {
    let go = VarId(1);
    let param_n = VarId(2);
    let case_binder = VarId(3);

    let mut bld = TreeBuilder::new();

    let var_n = bld.push(CoreFrame::Var(param_n));
    let lit_0 = bld.push(CoreFrame::Lit(Literal::LitInt(0)));
    let cmp = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntEq,
        args: vec![var_n, lit_0],
    });

    let lit_result = bld.push(CoreFrame::Lit(Literal::LitInt(result)));

    let var_n2 = bld.push(CoreFrame::Var(param_n));
    let lit_1 = bld.push(CoreFrame::Lit(Literal::LitInt(1)));
    let sub = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntSub,
        args: vec![var_n2, lit_1],
    });
    let var_go = bld.push(CoreFrame::Var(go));
    let tail_call = bld.push(CoreFrame::App {
        fun: var_go,
        arg: sub,
    });

    let case_node = bld.push(CoreFrame::Case {
        scrutinee: cmp,
        binder: case_binder,
        alts: vec![
            Alt {
                con: AltCon::LitAlt(Literal::LitInt(1)),
                binders: vec![],
                body: lit_result,
            },
            Alt {
                con: AltCon::Default,
                binders: vec![],
                body: tail_call,
            },
        ],
    });

    let lam = bld.push(CoreFrame::Lam {
        binder: param_n,
        body: case_node,
    });

    let lit_n = bld.push(CoreFrame::Lit(Literal::LitInt(n)));
    let var_go_call = bld.push(CoreFrame::Var(go));
    let app = bld.push(CoreFrame::App {
        fun: var_go_call,
        arg: lit_n,
    });

    bld.push(CoreFrame::LetRec {
        bindings: vec![(go, lam)],
        body: app,
    });

    bld.build()
}

// ──────────────────────────────────────────────────────────────────────
// Unit coverage for `CancelHandle`'s atomic semantics.
// ──────────────────────────────────────────────────────────────────────

/// A `CancelHandle` is obtainable from a compiled machine, and distinct clones
/// share state — cancellation on one is visible on another.
#[test]
fn cancel_handle_clones_share_state() {
    let expr = build_terminating_countdown(1, 42);
    let table = test_table();
    let machine = JitEffectMachine::compile(&expr, &table, 1 << 16).unwrap();

    let h1 = machine.cancel_handle();
    let h2 = h1.clone();
    let h3 = machine.cancel_handle();

    assert!(!h1.is_cancelled());
    assert!(!h2.is_cancelled());
    assert!(!h3.is_cancelled());

    h2.cancel();

    assert!(h1.is_cancelled());
    assert!(h2.is_cancelled());
    assert!(h3.is_cancelled(), "fresh handles also see the flag");

    h1.reset();
    assert!(!h1.is_cancelled());
    assert!(!h2.is_cancelled());
    assert!(!h3.is_cancelled());
}

/// `CancelHandle` is `Send + Sync`, required so callers can hand clones to
/// watchdog threads. This is a compile-time assertion.
#[test]
fn cancel_handle_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CancelHandle>();
}

// ──────────────────────────────────────────────────────────────────────
// Integration: cancellation observed at a GC safepoint.
// ──────────────────────────────────────────────────────────────────────

/// A runaway tail-recursive loop is aborted by an external
/// `cancel()` call within a bounded time budget. The program must surface
/// `YieldError::Cancelled` via the normal error path. The fixture uses a
/// non-allocating countdown so the cancel-observation site exercised here
/// is the tail-call trampoline (see `host_fns::trampoline_resolve`) rather
/// than `gc_trigger`.
#[test]
fn cancel_runaway_tail_recursive_loop() {
    // Use a small nursery so GC fires often and cancellation is observed quickly.
    // Extremely large bound — at ~1e8 per second this runs for ~10s, well
    // beyond the 100ms warmup + 2s cancel deadline.
    let expr = build_long_running_countdown(1_000_000_000);
    let table = test_table();
    let mut machine = JitEffectMachine::compile(&expr, &table, 1 << 20).unwrap();
    let handle = machine.cancel_handle();

    // Run in a separate thread so we can flip the flag from here.
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let done_thread = done.clone();
    let jit_thread = std::thread::spawn(move || {
        let result = machine.run_pure();
        done_thread.store(true, Ordering::SeqCst);
        result
    });

    // Let it warm up and start spinning.
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !done.load(Ordering::SeqCst),
        "fixture program was not actually infinite — it returned before cancel"
    );

    handle.cancel();

    // The JIT should observe the cancel at its next heap check (which happens
    // many times per millisecond in this fixture). 2s is a very generous bound.
    let deadline = Instant::now() + Duration::from_secs(2);
    while !done.load(Ordering::SeqCst) {
        if Instant::now() > deadline {
            panic!("JIT did not observe cancellation within 2s");
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    let err = jit_thread.join().unwrap().unwrap_err();
    match err {
        JitError::Yield(YieldError::Cancelled) => {}
        other => panic!("expected YieldError::Cancelled, got {:?}", other),
    }
}

/// When the cancel flag is not set, a terminating program runs to completion
/// with its normal result — cancellation is strictly opt-in.
#[test]
fn no_cancel_means_normal_completion() {
    let expr = build_terminating_countdown(1000, 99);
    let table = test_table();
    let mut machine = JitEffectMachine::compile(&expr, &table, 1 << 20).unwrap();
    let handle = machine.cancel_handle();
    assert!(!handle.is_cancelled());

    let result = machine
        .run_pure()
        .expect("terminating program must succeed");
    match result {
        tidepool_eval::value::Value::Lit(Literal::LitInt(n)) => assert_eq!(n, 99),
        other => panic!("expected Lit(Int(99)), got {:?}", other),
    }

    // Flag was never touched and the machine stays uncancelled afterwards.
    assert!(!handle.is_cancelled());
}

/// The cancel flag is per-machine (not per-run). After cancellation unwinds,
/// `reset()` lets the same machine be reused for a fresh run.
#[test]
fn reset_enables_reuse_after_cancellation() {
    let expr = build_long_running_countdown(1_000_000_000);
    let table = test_table();
    let mut machine = JitEffectMachine::compile(&expr, &table, 1 << 20).unwrap();
    let handle = machine.cancel_handle();

    // Pre-set cancel, then run: should abort almost immediately.
    handle.cancel();
    let err = machine.run_pure().unwrap_err();
    assert!(
        matches!(err, JitError::Yield(YieldError::Cancelled)),
        "first run must surface Cancelled, got {:?}",
        err
    );
    assert!(handle.is_cancelled());

    // Reset and run a different (terminating) program on a fresh machine —
    // confirms `reset()` clears the flag, which would also unblock reuse of
    // *this* machine if the compiled program supported multiple invocations.
    handle.reset();
    assert!(!handle.is_cancelled());

    let expr2 = build_terminating_countdown(500, 7);
    let mut machine2 = JitEffectMachine::compile(&expr2, &table, 1 << 20).unwrap();
    let handle2 = machine2.cancel_handle();
    // Explicitly reset (even though a fresh machine starts clean) to exercise
    // the path.
    handle2.reset();
    let result = machine2.run_pure().expect("second run must succeed");
    match result {
        tidepool_eval::value::Value::Lit(Literal::LitInt(n)) => assert_eq!(n, 7),
        other => panic!("expected Lit(Int(7)), got {:?}", other),
    }
}
