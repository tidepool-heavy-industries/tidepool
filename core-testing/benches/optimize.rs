use core_optimize::beta::BetaReduce;
use core_optimize::case_reduce::CaseReduce;
use core_optimize::dce::Dce;
use core_optimize::inline::Inline;
use core_optimize::pipeline::{default_passes, run_pipeline};
use core_repr::{
    Alt, AltCon, CoreExpr, CoreFrame, DataConId, Literal, PrimOpKind, RecursiveTree, VarId,
};
use core_eval::pass::Pass;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// Builds a reducible expression (~30 nodes).
/// Contains:
/// - Beta redexes
/// - Dead let bindings
/// - Known-con case
/// - Inlineable single-use lets
fn reducible_expr() -> CoreExpr {
    let mut nodes = Vec::new();

    // Body of the innermost lambda
    // case Just x of Just y -> y + 1
    nodes.push(CoreFrame::Var(VarId(1))); // 0: x
    nodes.push(CoreFrame::Con {
        tag: DataConId(1),
        fields: vec![0],
    }); // 1: Just x
    nodes.push(CoreFrame::Var(VarId(2))); // 2: y
    nodes.push(CoreFrame::Lit(Literal::LitInt(1))); // 3
    nodes.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![2, 3],
    }); // 4: y + 1
    nodes.push(CoreFrame::Case {
        scrutinee: 1,
        binder: VarId(3),
        alts: vec![Alt {
            con: AltCon::DataAlt(DataConId(1)),
            binders: vec![VarId(2)],
            body: 4,
        }],
    }); // 5

    // Let binding (dead)
    nodes.push(CoreFrame::Lit(Literal::LitInt(999))); // 6
    nodes.push(CoreFrame::LetNonRec {
        binder: VarId(4),
        rhs: 6,
        body: 5,
    }); // 7

    // Lambda
    nodes.push(CoreFrame::Lam {
        binder: VarId(1),
        body: 7,
    }); // 8: \x -> let dead = 999 in case Just x of Just y -> y + 1

    // Application (Beta redex)
    nodes.push(CoreFrame::Lit(Literal::LitInt(41))); // 9
    nodes.push(CoreFrame::App { fun: 8, arg: 9 }); // 10

    // Repeat similar patterns to reach ~100 nodes
    let mut current_root = 10;
    for i in 0..10 {
        let binder = VarId(100 + i);
        nodes.push(CoreFrame::Lit(Literal::LitInt(i as i64))); // rhs
        let rhs = nodes.len() - 1;
        nodes.push(CoreFrame::LetNonRec {
            binder,
            rhs,
            body: current_root,
        });
        current_root = nodes.len() - 1;
    }

    RecursiveTree { nodes }
}

fn bench_optimize(c: &mut Criterion) {
    let expr = reducible_expr();

    c.bench_function("opt_beta_reduce", |b| {
        b.iter(|| {
            let mut e = expr.clone();
            black_box(BetaReduce.run(&mut e))
        })
    });

    c.bench_function("opt_case_reduce", |b| {
        b.iter(|| {
            let mut e = expr.clone();
            black_box(CaseReduce.run(&mut e))
        })
    });

    c.bench_function("opt_inline", |b| {
        b.iter(|| {
            let mut e = expr.clone();
            black_box(Inline.run(&mut e))
        })
    });

    c.bench_function("opt_dce", |b| {
        b.iter(|| {
            let mut e = expr.clone();
            black_box(Dce.run(&mut e))
        })
    });

    c.bench_function("opt_full_pipeline", |b| {
        b.iter(|| {
            let mut e = expr.clone();
            let passes = default_passes();
            black_box(run_pipeline(&passes, &mut e))
        })
    });

    // Pipeline on already optimized expr
    let mut optimized = expr.clone();
    run_pipeline(&default_passes(), &mut optimized);
    c.bench_function("opt_pipeline_already_optimized", |b| {
        b.iter(|| {
            let mut e = optimized.clone();
            let passes = default_passes();
            black_box(run_pipeline(&passes, &mut e))
        })
    });
}

criterion_group!(benches, bench_optimize);
criterion_main!(benches);