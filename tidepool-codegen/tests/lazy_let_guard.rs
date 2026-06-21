//! Fixture-INDEPENDENT guard for lazy-let semantics (synthetic IR → JIT-vs-eval).
//!
//! GHC Core `let` is NON-STRICT. The JIT used to evaluate every LetNonRec RHS
//! eagerly, which force-evaluated productive corecursions (e.g. ReadP `expect`'s
//! `let x = F k in <Con using x>`) into infinite self-recursion → StackOverflow,
//! while the lazy tree-walker stayed bounded by demand. The fix: thunkify
//! non-trivial LetNonRec RHS; keep the trivial (WHNF / strict-primop) fast-path
//! eager.
//!
//! These hand-built IR cases pin both halves so the behaviour survives the later
//! eager-eval-class consolidation refactor (they don't go through the Haskell
//! extractor, so no fixture drift).
use tidepool_eval::value::Value;
use tidepool_repr::{Alt, AltCon, CoreFrame, DataConId, Literal, PrimOpKind, TreeBuilder, VarId};
use tidepool_testing::proptest::{
    build_table_for_expr, check_jit_vs_eval_captured, CapturedOutcome,
};

const NURSERY: usize = 1 << 20;

fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn boxed_or_lit_int(v: &Value) -> Option<i64> {
    match v {
        Value::Lit(Literal::LitInt(n)) => Some(*n),
        Value::Con(_, fields) => match fields.first() {
            Some(Value::Lit(Literal::LitInt(n))) => Some(*n),
            _ => None,
        },
        _ => None,
    }
}

/// A productive infinite corecursion consumed BOUNDEDLY:
///
/// ```text
/// go = \n -> let x = go n in Cons n x      -- infinite list  n : n : n : …
/// root = case (go 7) of { Cons h _ -> h }  -- take the head = 7
/// ```
///
/// Lazy let: `go 7` is WHNF `Cons 7 <thunk x>`; the head is 7, the tail thunk is
/// never forced → terminates. Eager let would force `x = go 7` → `go 7` → … →
/// StackOverflow on the JIT (eval stays lazy → divergence). So `Agree(7)` holds
/// ONLY with lazy-let thunkification — this is the regression guard.
#[test]
fn corecursive_let_consumed_boundedly_terminates() {
    let go = VarId(1);
    let n = VarId(2);
    let x = VarId(3);
    let h = VarId(4);
    let t = VarId(5);
    let case_bind = VarId(6);
    let cons = DataConId(0xC0_FFEE);

    let mut b = TreeBuilder::new();
    // go n   (the recursive call — must be deferred as a thunk)
    let go_v = b.push(CoreFrame::Var(go));
    let n_v = b.push(CoreFrame::Var(n));
    let app_go_n = b.push(CoreFrame::App {
        fun: go_v,
        arg: n_v,
    });
    // Cons n x
    let n_v2 = b.push(CoreFrame::Var(n));
    let x_v = b.push(CoreFrame::Var(x));
    let con = b.push(CoreFrame::Con {
        tag: cons,
        fields: vec![n_v2, x_v],
    });
    // let x = go n in Cons n x
    let body = b.push(CoreFrame::LetNonRec {
        binder: x,
        rhs: app_go_n,
        body: con,
    });
    // \n -> ...
    let lam = b.push(CoreFrame::Lam { binder: n, body });
    // go 7
    let go_v2 = b.push(CoreFrame::Var(go));
    let lit7 = b.push(CoreFrame::Lit(Literal::LitInt(7)));
    let app_go_7 = b.push(CoreFrame::App {
        fun: go_v2,
        arg: lit7,
    });
    // case (go 7) of { Cons h t -> h }
    let h_v = b.push(CoreFrame::Var(h));
    let case = b.push(CoreFrame::Case {
        scrutinee: app_go_7,
        binder: case_bind,
        alts: vec![Alt {
            con: AltCon::DataAlt(cons),
            binders: vec![h, t],
            body: h_v,
        }],
    });
    // letrec go = \n -> ... in case ...
    let _root = b.push(CoreFrame::LetRec {
        bindings: vec![(go, lam)],
        body: case,
    });
    let expr = b.build();

    on_big_stack(move || {
        let table = build_table_for_expr(&expr);
        match check_jit_vs_eval_captured(&expr, &table, NURSERY) {
            CapturedOutcome::Agree(v) => assert_eq!(
                boxed_or_lit_int(&v),
                Some(7),
                "corecursive `let x = go n` consumed boundedly must be 7 in both engines, got {v:?}"
            ),
            other => panic!(
                "lazy-let regression: a productive corecursion consumed boundedly must Agree(7) \
                 (eager-let force-recurses the JIT into StackOverflow). got {other:?}"
            ),
        }
    });
}

/// A trivial let (literal RHS) stays on the eager fast-path and evaluates
/// correctly: `let y = 5 in y +# 10` == 15. Guards that the trivial branch of
/// the thunk-vs-eager split is still taken and correct after the change.
#[test]
fn trivial_let_fast_path_is_correct() {
    let y = VarId(1);
    let mut b = TreeBuilder::new();
    let five = b.push(CoreFrame::Lit(Literal::LitInt(5)));
    let y_v = b.push(CoreFrame::Var(y));
    let ten = b.push(CoreFrame::Lit(Literal::LitInt(10)));
    let add = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![y_v, ten],
    });
    let _root = b.push(CoreFrame::LetNonRec {
        binder: y,
        rhs: five,
        body: add,
    });
    let expr = b.build();

    on_big_stack(move || {
        let table = build_table_for_expr(&expr);
        match check_jit_vs_eval_captured(&expr, &table, NURSERY) {
            CapturedOutcome::Agree(v) => assert_eq!(
                boxed_or_lit_int(&v),
                Some(15),
                "trivial `let y = 5 in y +# 10` must be 15, got {v:?}"
            ),
            other => panic!("trivial let must Agree(15), got {other:?}"),
        }
    });
}
