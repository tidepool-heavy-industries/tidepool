//! Fixture-INDEPENDENT guards for the LetRec lazy-default spine (Stage 1).
//!
//! emit_letrec_phases now binds every simple (non-Lam/non-Con) LetRec binding
//! under the lazy-default rule: thunkify unless trivially resolvable now, in
//! topological order. These hand-built IR cases pin the behaviour that the
//! 5-phase knot-tie must preserve, so it survives the later consolidation:
//!   1. a Var alias to a still-pending sibling resolves (the old Gap A, now
//!      subsumed by the rule);
//!   2. a self-referential Con knot (`xs = 1 : xs`) works;
//!   3. a mutually-referential Con knot (`xs = 1:ys; ys = 2:xs`) works;
//!   4. a SIMPLE binding that calls a closure which pattern-matches a sibling
//!      Con — the documented Phase-3c SIGSEGV — is safe under lazy-default
//!      (the binding is a thunk forced after all Con fields are filled).
use tidepool_eval::value::Value;
use tidepool_repr::{Alt, AltCon, CoreFrame, DataConId, Literal, PrimOpKind, TreeBuilder, VarId};
use tidepool_testing::proptest::{
    build_table_for_expr, check_jit_vs_eval_captured, CapturedOutcome,
};

const NURSERY: usize = 1 << 20;
const CONS: u64 = 0x00C0_FFEE; // 2 fields
const NIL: u64 = 0x0000_0411; // 0 fields

fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn lit_int(v: &Value) -> Option<i64> {
    match v {
        Value::Lit(Literal::LitInt(n)) => Some(*n),
        Value::Con(_, fields) => match fields.first() {
            Some(Value::Lit(Literal::LitInt(n))) => Some(*n),
            _ => None,
        },
        _ => None,
    }
}

fn assert_agrees(name: &'static str, expr: tidepool_repr::CoreExpr, expected: i64) {
    on_big_stack(move || {
        let table = build_table_for_expr(&expr);
        match check_jit_vs_eval_captured(&expr, &table, NURSERY) {
            CapturedOutcome::Agree(v) => assert_eq!(
                lit_int(&v),
                Some(expected),
                "{name}: both engines must agree on {expected}, got {v:?}"
            ),
            other => panic!("{name}: must Agree({expected}) in both engines, got {other:?}"),
        }
    });
}

/// `letrec { caf = (\x -> x) 5 ; result = caf } in result` == 5.
/// Both bindings are SIMPLE (the all-simple path). `result` is a Var alias to
/// the still-pending App-CAF `caf`; topo order binds `caf` first so the alias
/// resolves (eager Var lookup) instead of trapping on an unresolved var.
#[test]
fn var_alias_to_pending_sibling_resolves() {
    let caf = VarId(1);
    let result = VarId(2);
    let x = VarId(3);
    let mut b = TreeBuilder::new();
    let xv = b.push(CoreFrame::Var(x));
    let idlam = b.push(CoreFrame::Lam {
        binder: x,
        body: xv,
    });
    let five = b.push(CoreFrame::Lit(Literal::LitInt(5)));
    let caf_rhs = b.push(CoreFrame::App {
        fun: idlam,
        arg: five,
    }); // (\x->x) 5  — an App = simple/deferred
    let result_rhs = b.push(CoreFrame::Var(caf));
    let body = b.push(CoreFrame::Var(result));
    let root = b.push(CoreFrame::LetRec {
        bindings: vec![(caf, caf_rhs), (result, result_rhs)],
        body,
    });
    let _ = root;
    assert_agrees("var_alias_to_pending", b.build(), 5);
}

/// `letrec { xs = Cons 1 xs } in case xs of { Cons h _ -> h }` == 1.
/// Self-referential Con knot — the Phase-1 pre-alloc + self-capture.
#[test]
fn self_referential_con_knot() {
    let xs = VarId(1);
    let h = VarId(2);
    let t = VarId(3);
    let cb = VarId(4);
    let mut b = TreeBuilder::new();
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let xs_ref = b.push(CoreFrame::Var(xs));
    let xs_rhs = b.push(CoreFrame::Con {
        tag: DataConId(CONS),
        fields: vec![one, xs_ref],
    });
    let xs_scrut = b.push(CoreFrame::Var(xs));
    let hv = b.push(CoreFrame::Var(h));
    let case = b.push(CoreFrame::Case {
        scrutinee: xs_scrut,
        binder: cb,
        alts: vec![Alt {
            con: AltCon::DataAlt(DataConId(CONS)),
            binders: vec![h, t],
            body: hv,
        }],
    });
    let _root = b.push(CoreFrame::LetRec {
        bindings: vec![(xs, xs_rhs)],
        body: case,
    });
    assert_agrees("self_referential_con_knot", b.build(), 1);
}

/// `letrec { xs = Cons 1 ys ; ys = Cons 2 xs } in
///    case xs of Cons h1 t -> case t of Cons h2 _ -> h1 +# h2` == 3.
/// Mutually-referential Con knot.
#[test]
fn mutual_con_knot() {
    let xs = VarId(1);
    let ys = VarId(2);
    let (h1, t1, c1) = (VarId(3), VarId(4), VarId(5));
    let (h2, t2, c2) = (VarId(6), VarId(7), VarId(8));
    let mut b = TreeBuilder::new();
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let ys_ref = b.push(CoreFrame::Var(ys));
    let xs_rhs = b.push(CoreFrame::Con {
        tag: DataConId(CONS),
        fields: vec![one, ys_ref],
    });
    let two = b.push(CoreFrame::Lit(Literal::LitInt(2)));
    let xs_ref = b.push(CoreFrame::Var(xs));
    let ys_rhs = b.push(CoreFrame::Con {
        tag: DataConId(CONS),
        fields: vec![two, xs_ref],
    });
    // inner: case t1 of Cons h2 _ -> h1 +# h2
    let h1v = b.push(CoreFrame::Var(h1));
    let h2v = b.push(CoreFrame::Var(h2));
    let add = b.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![h1v, h2v],
    });
    let t1_scrut = b.push(CoreFrame::Var(t1));
    let inner = b.push(CoreFrame::Case {
        scrutinee: t1_scrut,
        binder: c2,
        alts: vec![Alt {
            con: AltCon::DataAlt(DataConId(CONS)),
            binders: vec![h2, t2],
            body: add,
        }],
    });
    let xs_scrut = b.push(CoreFrame::Var(xs));
    let outer = b.push(CoreFrame::Case {
        scrutinee: xs_scrut,
        binder: c1,
        alts: vec![Alt {
            con: AltCon::DataAlt(DataConId(CONS)),
            binders: vec![h1, t1],
            body: inner,
        }],
    });
    let _root = b.push(CoreFrame::LetRec {
        bindings: vec![(xs, xs_rhs), (ys, ys_rhs)],
        body: outer,
    });
    assert_agrees("mutual_con_knot", b.build(), 3);
}

/// `letrec { c = Cons 1 Nil ; f = \_ -> case c of Cons h _ -> h ; r = f 0 } in r`
/// == 1. `r` is a SIMPLE binding that, when eager, would call closure `f` which
/// pattern-matches Con `c` BEFORE its fields are filled (the documented
/// Phase-3c SIGSEGV). Under lazy-default `r` is a thunk forced after the fills.
#[test]
fn simple_binding_calls_closure_matching_con() {
    let c = VarId(1);
    let f = VarId(2);
    let r = VarId(3);
    let (h, t, cb) = (VarId(4), VarId(5), VarId(6));
    let ignore = VarId(7);
    let mut b = TreeBuilder::new();
    // c = Cons 1 Nil
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let nil = b.push(CoreFrame::Con {
        tag: DataConId(NIL),
        fields: vec![],
    });
    let c_rhs = b.push(CoreFrame::Con {
        tag: DataConId(CONS),
        fields: vec![one, nil],
    });
    // f = \_ -> case c of Cons h _ -> h
    let c_scrut = b.push(CoreFrame::Var(c));
    let hv = b.push(CoreFrame::Var(h));
    let f_case = b.push(CoreFrame::Case {
        scrutinee: c_scrut,
        binder: cb,
        alts: vec![Alt {
            con: AltCon::DataAlt(DataConId(CONS)),
            binders: vec![h, t],
            body: hv,
        }],
    });
    let f_rhs = b.push(CoreFrame::Lam {
        binder: ignore,
        body: f_case,
    });
    // r = f 0
    let fv = b.push(CoreFrame::Var(f));
    let zero = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    let r_rhs = b.push(CoreFrame::App { fun: fv, arg: zero });
    let body = b.push(CoreFrame::Var(r));
    let _root = b.push(CoreFrame::LetRec {
        bindings: vec![(c, c_rhs), (f, f_rhs), (r, r_rhs)],
        body,
    });
    assert_agrees("simple_calls_closure_matching_con", b.build(), 1);
}
