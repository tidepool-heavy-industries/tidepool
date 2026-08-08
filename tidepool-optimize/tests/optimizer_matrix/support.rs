//! Shared plumbing for the optimizer test matrix: the thread+`TestRunner`
//! harness every cell drives its generator through, the constructed-input
//! wrappers the shaped cells build their pinned shapes from, and the
//! full-pipeline pass-preservation oracle the `FullPipeline` cells share.

use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use tidepool_eval::{eval, Env, VecHeap};
use tidepool_optimize::pipeline::optimize;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::{Alt, AltCon, DataConId, VarId};
use tidepool_repr::{CoreExpr, TreeBuilder};
use tidepool_testing::proptest::values_equal;

/// Flatten `expr`'s nodes into a builder so it can be spliced into a larger
/// tree via [`TreeBuilder::push_tree`].
pub fn expr_to_builder(expr: CoreExpr) -> TreeBuilder {
    let mut b = TreeBuilder::new();
    for node in expr.nodes {
        b.push(node);
    }
    b
}

/// `(\binder -> body) arg` — the shape `BetaReduce` fires on.
pub fn wrap_in_beta_reducible(body: CoreExpr, arg: CoreExpr, binder: VarId) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let body_len = body.nodes.len();
    let arg_len = arg.nodes.len();

    let body_off = b.push_tree(expr_to_builder(body));
    let body_root = body_off + body_len - 1;

    let arg_off = b.push_tree(expr_to_builder(arg));
    let arg_root = arg_off + arg_len - 1;

    let lam = b.push(CoreFrame::Lam {
        binder,
        body: body_root,
    });
    b.push(CoreFrame::App {
        fun: lam,
        arg: arg_root,
    });
    b.build()
}

/// `let binder = rhs in body` where `binder` never occurs free in `body` —
/// the shape `Dce` fires on.
pub fn wrap_in_unused_let(rhs: CoreExpr, body: CoreExpr, binder: VarId) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let rhs_len = rhs.nodes.len();
    let body_len = body.nodes.len();

    let rhs_off = b.push_tree(expr_to_builder(rhs));
    let rhs_root = rhs_off + rhs_len - 1;

    let body_off = b.push_tree(expr_to_builder(body));
    let body_root = body_off + body_len - 1;

    b.push(CoreFrame::LetNonRec {
        binder,
        rhs: rhs_root,
        body: body_root,
    });
    b.build()
}

/// `let binder = rhs in binder` — the shape `Inline` fires on (a binder used
/// exactly once).
pub fn wrap_in_used_once_let(rhs: CoreExpr, binder: VarId) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let rhs_len = rhs.nodes.len();

    let rhs_off = b.push_tree(expr_to_builder(rhs));
    let rhs_root = rhs_off + rhs_len - 1;

    let var = b.push(CoreFrame::Var(binder));
    b.push(CoreFrame::LetNonRec {
        binder,
        rhs: rhs_root,
        body: var,
    });
    b.build()
}

/// `case Con(tag, [42]) of { DataAlt(tag, [_]) -> body }` — the shape
/// `CaseReduce` fires on.
pub fn wrap_in_known_con_case(body: CoreExpr, binder: VarId, tag: DataConId) -> CoreExpr {
    let mut b = TreeBuilder::new();
    let lit = b.push(CoreFrame::Lit(tidepool_repr::types::Literal::LitInt(42)));
    let con = b.push(CoreFrame::Con {
        tag,
        fields: vec![lit],
    });

    let body_len = body.nodes.len();
    let body_off = b.push_tree(expr_to_builder(body));
    let body_root = body_off + body_len - 1;

    let alt = Alt {
        con: AltCon::DataAlt(tag),
        binders: vec![VarId(binder.0 + 1)], // dummy binder for the field
        body: body_root,
    };
    b.push(CoreFrame::Case {
        scrutinee: con,
        binder,
        alts: vec![alt],
    });
    b.build()
}

/// Run a strategy built by `mk_strategy` for `cases` proptest cases inside a
/// thread with a `stack_bytes`-sized stack — the generators and passes here
/// recurse deeply enough to blow the default test-thread stack.
///
/// Takes a strategy-builder rather than a strategy: proptest's boxed
/// strategies (which `arb_core_expr` and friends are built from) are not
/// `Send`, so the strategy must be constructed on the worker thread itself
/// rather than moved into it.
pub fn run_matrix<S, F>(
    stack_bytes: usize,
    cases: u32,
    mk_strategy: impl FnOnce() -> S + Send + 'static,
    f: F,
) where
    S: Strategy,
    F: Fn(S::Value) -> Result<(), TestCaseError> + Send + 'static,
{
    let handle = std::thread::Builder::new()
        .stack_size(stack_bytes)
        .spawn(move || {
            let strategy = mk_strategy();
            let mut runner = TestRunner::new(Config {
                cases,
                ..Config::default()
            });
            runner.run(&strategy, f).unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

/// Full-pipeline pass-preservation oracle at WHNF (no `deep_force`): `optimize`
/// must not change what `expr` evaluates to.
///
/// Distinct from `tidepool_testing::proptest::check_pass_preserves_eval`:
/// that oracle deep-forces and takes a single `Pass`, while `optimize` returns
/// `PipelineStats` rather than a `Changed` bool and every `FullPipeline` cell
/// here historically compared at WHNF only.
pub fn check_pipeline_preserves_eval(expr: CoreExpr) -> Result<(), TestCaseError> {
    let mut heap1 = VecHeap::new();
    let env = Env::new();
    let original_res = eval(&expr, &env, &mut heap1);

    let mut optimized = expr.clone();
    optimize(&mut optimized).unwrap();

    let mut heap2 = VecHeap::new();
    let optimized_res = eval(&optimized, &env, &mut heap2);

    match (original_res, optimized_res) {
        (Ok(v1), Ok(v2)) => {
            prop_assert!(
                values_equal(&v1, &v2),
                "Pipeline evaluation results differ.
Original: {:?}
Optimized: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                v1,
                v2,
                expr,
                optimized
            );
        }
        (Err(_), _) => {}
        (Ok(_), Err(e)) => {
            prop_assert!(
                false,
                "Pipeline optimized evaluation failed but original succeeded.
Error: {:?}
Expr: {:#?}
Optimized Expr: {:#?}",
                e,
                expr,
                optimized
            );
        }
    }
    Ok(())
}
