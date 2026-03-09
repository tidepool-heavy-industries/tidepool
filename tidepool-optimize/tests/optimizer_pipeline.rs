use tidepool_eval::pass::Pass;
use tidepool_eval::{eval, Env, Value, VecHeap};
use tidepool_optimize::beta::BetaReduce;
use tidepool_optimize::case_reduce::CaseReduce;
use tidepool_optimize::dce::Dce;
use tidepool_optimize::inline::Inline;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, TreeBuilder};

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Lit(l1), Value::Lit(l2)) => l1 == l2,
        (Value::Con(tag1, fields1), Value::Con(tag2, fields2)) => {
            tag1 == tag2
                && fields1.len() == fields2.len()
                && fields1
                    .iter()
                    .zip(fields2.iter())
                    .all(|(f1, f2)| values_equal(f1, f2))
        }
        _ => true,
    }
}

fn assert_eval_equiv(expr: &CoreExpr, optimized: &CoreExpr) {
    let mut heap1 = VecHeap::new();
    let env = Env::new();
    let res1 = eval(expr, &env, &mut heap1).expect("original eval failed");

    let mut heap2 = VecHeap::new();
    let res2 = eval(optimized, &env, &mut heap2).expect("optimized eval failed");

    assert!(values_equal(&res1, &res2));
}

#[test]
fn test_beta_dce_pipeline() {
    // let x = 42 in ((\y -> y) 10)
    let x = VarId(1);
    let y = VarId(2);
    let mut bld = TreeBuilder::new();

    let vy = bld.push(CoreFrame::Var(y));
    let lam = bld.push(CoreFrame::Lam {
        binder: y,
        body: vy,
    });
    let lit10 = bld.push(CoreFrame::Lit(Literal::LitInt(10)));
    let app = bld.push(CoreFrame::App {
        fun: lam,
        arg: lit10,
    });

    let lit42 = bld.push(CoreFrame::Lit(Literal::LitInt(42)));
    bld.push(CoreFrame::LetNonRec {
        binder: x,
        rhs: lit42,
        body: app,
    });

    let expr = bld.build();
    let mut optimized = expr.clone();

    BetaReduce.run(&mut optimized);
    Dce.run(&mut optimized);

    assert_eval_equiv(&expr, &optimized);

    // Check that it's simplified.
    assert!(optimized.nodes.len() < expr.nodes.len());
}

#[test]
fn test_case_reduce_pipeline() {
    // case Con(0, [42]) of { DataAlt(0, [x]) -> x }
    let x = VarId(1);
    let tag = DataConId(0);
    let mut bld = TreeBuilder::new();

    let lit42 = bld.push(CoreFrame::Lit(Literal::LitInt(42)));
    let con = bld.push(CoreFrame::Con {
        tag,
        fields: vec![lit42],
    });

    let vx = bld.push(CoreFrame::Var(x));
    let _case_node = bld.push(CoreFrame::Case {
        scrutinee: con,
        binder: VarId(0),
        alts: vec![Alt {
            con: AltCon::DataAlt(tag),
            binders: vec![x],
            body: vx,
        }],
    });

    let expr = bld.build();
    let mut optimized = expr.clone();

    CaseReduce.run(&mut optimized);
    Dce.run(&mut optimized);

    assert_eval_equiv(&expr, &optimized);
}

#[test]
fn test_full_pipeline() {
    // let f = \x -> x + 1 in f 10
    let f = VarId(1);
    let x = VarId(2);

    let mut bld = TreeBuilder::new();
    let vx = bld.push(CoreFrame::Var(x));
    let lit1 = bld.push(CoreFrame::Lit(Literal::LitInt(1)));
    let add = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![vx, lit1],
    });
    let lam = bld.push(CoreFrame::Lam {
        binder: x,
        body: add,
    });

    let lit10 = bld.push(CoreFrame::Lit(Literal::LitInt(10)));
    let vf = bld.push(CoreFrame::Var(f));
    let app = bld.push(CoreFrame::App {
        fun: vf,
        arg: lit10,
    });

    bld.push(CoreFrame::LetNonRec {
        binder: f,
        rhs: lam,
        body: app,
    });

    let expr = bld.build();
    let mut optimized = expr.clone();

    // Inline (f is used once) -> BetaReduce ((\x -> x + 1) 10) -> Dce
    Inline.run(&mut optimized);
    BetaReduce.run(&mut optimized);
    Dce.run(&mut optimized);

    assert_eval_equiv(&expr, &optimized);
}
