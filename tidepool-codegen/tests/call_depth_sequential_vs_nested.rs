//! Finding 5 (repo-review-2026-07-06/01-gc-memory-safety.md): the call-depth
//! counter (`MachineState::call_depth`, checked in `debug_app_check` against
//! `MAX_CALL_DEPTH = 20_000`) used to be incremented on every non-tail `App`
//! call and never decremented on return — it counted TOTAL calls made, not
//! live nesting. `debug_app_return` (paired with `debug_app_check` at every
//! exit from the regular, non-tail `App` emission) fixes this: the counter
//! now tracks actual concurrently-live call nesting.
//!
//! Two tests, matching the plan's acceptance criteria (the strict-fold
//! false-positive; the genuine-recursion clean overflow) at a slightly
//! smaller scale than "50k" for the sequential case — see
//! `SEQUENTIAL_CALL_COUNT`'s doc for why:
//!
//! (a) a STRICT, purely SEQUENTIAL chain of applications (`case f r0 of r1
//!     -> case f r1 of r2 -> ...` — deliberately NOT `let`-bound; this Core
//!     IR's `let` is lazy, so a `let`-chain would bind thunks and force them
//!     in a genuinely NESTED chain instead, defeating the point) — each call
//!     fully returns (the native frame pops) before the next begins, so real
//!     nesting never exceeds 1. Before the fix this false-positived
//!     `StackOverflow` past ~20k calls even though nothing was ever actually
//!     nested; after the fix it must succeed with the correct sum.
//! (b) a GENUINELY non-tail-recursive fold (`sumList (x:xs) = x + sumList
//!     xs` — the recursive call sits in `+`'s argument position, so each
//!     level's native frame stays live until the inner call returns: REAL
//!     O(n) nesting) over a list deep enough to exceed `MAX_CALL_DEPTH` must
//!     still overflow — cleanly (a typed `StackOverflow`, not a SIGSEGV) —
//!     proving the fix didn't turn the guard into a no-op.

use tidepool_eval::value::Value;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, CoreFrame, TreeBuilder};
use tidepool_testing::proptest::build_table_for_expr;

const NIL: DataConId = DataConId(5);
const CONS: DataConId = DataConId(6);

thread_local! {
    static VAR_CTR: std::cell::Cell<u64> = const { std::cell::Cell::new(1000) };
}
fn fresh_var() -> VarId {
    VAR_CTR.with(|c| {
        let v = c.get();
        c.set(v + 1);
        VarId(v)
    })
}
fn reset_ctr() {
    VAR_CTR.with(|c| c.set(1000));
}

fn fixup_root(tree: &mut CoreExpr, root: usize) -> CoreExpr {
    if root == tree.nodes.len() - 1 {
        return tree.clone();
    }
    let binder = fresh_var();
    let var_idx = tree.nodes.len();
    tree.nodes.push(CoreFrame::Var(binder));
    tree.nodes.push(CoreFrame::LetNonRec {
        binder,
        rhs: root,
        body: var_idx,
    });
    tree.clone()
}

/// `let f = \x -> x +# 1 in let r0 = 0 in case f r0 of r1 -> case f r1 of r2
/// -> ... -> r<n>` — `n` PURELY SEQUENTIAL, non-nested applications.
///
/// A `Case` scrutinee is ALWAYS forced eagerly (that's what pattern matching
/// requires) — unlike a plain `let`-bound RHS, which this Core IR's `let` is
/// LAZY about (see the `LetNonRec` "non-trivial RHS: thunkify" comment in
/// `emit/expr.rs`): a chain of `let r_i = f r_{i-1}` bindings would each bind
/// a THUNK, and forcing the final result would force them all in a genuinely
/// NESTED chain (`heap_force` recursing into the previous thunk mid-force) —
/// exactly the deeply-recursive shape test (b) covers, not the flat one this
/// test needs. Chaining via `Case(Default)` instead makes each `f r_i` call
/// fully return (the native frame pops) before the next is even reached, so
/// real call nesting never exceeds 1, however large `n` gets.
fn build_sequential_chain(n: usize) -> (CoreExpr, i64) {
    reset_ctr();
    let mut b = TreeBuilder::new();

    let f = fresh_var();
    let x = fresh_var();
    let xv = b.push(CoreFrame::Var(x));
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let fbody = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![xv, one],
    });
    let f_lam = b.push(CoreFrame::Lam {
        binder: x,
        body: fbody,
    });

    let r0 = fresh_var();
    let r0_rhs = b.push(CoreFrame::Lit(Literal::LitInt(0)));

    // Pass 1 (forward): build each `f r_i` scrutinee App node (referencing
    // the PREVIOUS case binder) and mint the case binder it will force into.
    let mut binders: Vec<(VarId, usize)> = Vec::with_capacity(n);
    let mut prev = r0;
    for _ in 0..n {
        let f_v = b.push(CoreFrame::Var(f));
        let arg_v = b.push(CoreFrame::Var(prev));
        let app = b.push(CoreFrame::App {
            fun: f_v,
            arg: arg_v,
        });
        let r_i = fresh_var();
        binders.push((r_i, app));
        prev = r_i;
    }
    let last = prev;

    // Pass 2 (backward): wrap the Case(Default) chain around the final Var.
    let mut chain_body = b.push(CoreFrame::Var(last));
    for &(binder, scrut) in binders.iter().rev() {
        chain_body = b.push(CoreFrame::Case {
            scrutinee: scrut,
            binder,
            alts: vec![Alt {
                con: AltCon::Default,
                binders: vec![],
                body: chain_body,
            }],
        });
    }
    let with_r0 = b.push(CoreFrame::LetNonRec {
        binder: r0,
        rhs: r0_rhs,
        body: chain_body,
    });
    let root = b.push(CoreFrame::LetNonRec {
        binder: f,
        rhs: f_lam,
        body: with_r0,
    });

    let mut tree = b.build();
    (fixup_root(&mut tree, root), n as i64)
}

/// `xs = [0, 1, .., n-1]`; `sumList (h:t) = h + sumList t; sumList [] = 0`;
/// root = `sumList xs`. The recursive call is in `+`'s ARGUMENT position —
/// non-tail, genuinely nested: native call depth grows to `n` before the
/// innermost `[]` case returns.
fn build_deep_nonrec_fold(n: i64) -> (CoreExpr, i64) {
    reset_ctr();
    let elems: Vec<i64> = (0..n).collect();
    let expected: i64 = elems.iter().sum();

    let mut b = TreeBuilder::new();
    let mut tail = b.push(CoreFrame::Con {
        tag: NIL,
        fields: vec![],
    });
    for &e in elems.iter().rev() {
        let head = b.push(CoreFrame::Lit(Literal::LitInt(e)));
        tail = b.push(CoreFrame::Con {
            tag: CONS,
            fields: vec![head, tail],
        });
    }
    let list_root = tail;

    let sum_list = fresh_var();
    let xs = fresh_var();
    let case_binder = fresh_var();
    let h = fresh_var();
    let t = fresh_var();

    let nil_body = b.push(CoreFrame::Lit(Literal::LitInt(0)));

    let sum_list_v1 = b.push(CoreFrame::Var(sum_list));
    let t_v = b.push(CoreFrame::Var(t));
    let recur_call = b.push(CoreFrame::App {
        fun: sum_list_v1,
        arg: t_v,
    });
    let h_v = b.push(CoreFrame::Var(h));
    // NON-TAIL: `+` needs the recursive call's result, so this App can never
    // be compiled as a tail call — it's a genuine value-position use.
    let cons_body = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![h_v, recur_call],
    });

    let xs_v = b.push(CoreFrame::Var(xs));
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
                body: cons_body,
            },
        ],
    });
    let sum_list_lam = b.push(CoreFrame::Lam {
        binder: xs,
        body: case_node,
    });

    let sum_list_v2 = b.push(CoreFrame::Var(sum_list));
    let call_root = b.push(CoreFrame::App {
        fun: sum_list_v2,
        arg: list_root,
    });
    let root = b.push(CoreFrame::LetRec {
        bindings: vec![(sum_list, sum_list_lam)],
        body: call_root,
    });

    let mut tree = b.build();
    (fixup_root(&mut tree, root), expected)
}

/// Comfortably past the pre-fix `MAX_CALL_DEPTH` (20_000) ceiling. The plan's
/// acceptance criterion names "50k sequential calls"; compiling that many
/// call sites into ONE Cranelift function (each with its own TCO-check basic
/// blocks) takes several minutes, so this uses 25_000 instead — comfortably
/// past the old 20_000 false-positive threshold (proving the property) while
/// keeping the test at roughly a minute and a half.
const SEQUENTIAL_CALL_COUNT: usize = 25_000;

/// (a) many thousands of purely sequential, non-nested applications must
/// succeed — no false StackOverflow from an un-decremented call-depth counter.
///
/// Runs on a large stack thread mirroring the REAL eval entry point's own
/// stack budget (`tidepool_runtime::EVAL_STACK_SIZE`, 256 MiB — not
/// duplicated as a dependency here, just the same literal) rather than
/// libtest's default per-test thread: `MAX_CALL_DEPTH` is a SOFTWARE guard
/// meant to trip cleanly well before the REAL native stack is exhausted, and
/// that headroom is what production actually gives it.
#[test]
fn fifty_thousand_sequential_calls_do_not_false_positive_overflow() {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(|| {
            let (expr, expected) = build_sequential_chain(SEQUENTIAL_CALL_COUNT);
            let table = build_table_for_expr(&expr);
            let mut machine =
                tidepool_codegen::jit_machine::JitEffectMachine::compile(&expr, &table, 1 << 16)
                    .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
            let result = machine.run_pure().unwrap_or_else(|e| {
                panic!("{SEQUENTIAL_CALL_COUNT} sequential calls must not overflow: {e:?}")
            });
            match result {
                Value::Lit(Literal::LitInt(n)) => assert_eq!(n, expected),
                other => panic!("expected LitInt({expected}), got {other:?}"),
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

/// (b) genuinely deep (non-tail) recursion still overflows CLEANLY — a typed
/// `StackOverflow`, not a signal/crash — proving the fix didn't neuter the
/// guard into a no-op. Same large-stack rationale as (a): `MAX_CALL_DEPTH`
/// (20_000) must trip well before 25k REAL nested native frames physically
/// exhaust the stack, which only holds on a stack sized like production's.
#[test]
fn genuinely_deep_recursion_still_overflows_cleanly() {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(|| {
            // Comfortably past MAX_CALL_DEPTH (20_000); the list itself (not a
            // sequence of calls) is what's large here, so this compiles fast.
            let (expr, _expected_if_it_somehow_completed) = build_deep_nonrec_fold(25_000);
            let table = build_table_for_expr(&expr);
            let mut machine =
                tidepool_codegen::jit_machine::JitEffectMachine::compile(&expr, &table, 1 << 20)
                    .unwrap_or_else(|e| panic!("compile failed: {e:?}"));
            let result = machine.run_pure();
            match result {
                Err(tidepool_codegen::jit_machine::JitError::Yield(
                    tidepool_codegen::yield_type::YieldError::Runtime(
                        tidepool_codegen::host_fns::RuntimeError::StackOverflow,
                    ),
                )) => {
                    // Exactly the clean, typed overflow we want.
                }
                Err(other) => panic!(
                    "expected a clean typed StackOverflow, got a DIFFERENT error \
                     (still not a crash, but not the expected guard either): {other:?}"
                ),
                Ok(v) => panic!(
                    "expected genuinely deep non-tail recursion (25k levels) to overflow \
                     the call-depth guard, but it completed with {v:?} — the guard may have \
                     been neutered"
                ),
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
