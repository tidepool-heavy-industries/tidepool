use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use tidepool_eval::pass::Pass;
use tidepool_optimize::beta::BetaReduce;
use tidepool_optimize::case_reduce::CaseReduce;
use tidepool_optimize::dce::Dce;
use tidepool_optimize::inline::Inline;
use tidepool_optimize::pipeline::{default_passes, run_pipeline};
use tidepool_repr::{
    Alt, AltCon, CoreExpr, CoreFrame, DataConId, Literal, PrimOpKind, RecursiveTree, TreeBuilder, VarId,
};

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

/// Builds an expression that requires multiple optimization passes.
/// Result = (\xN -> ... (\x1 -> x1 + 1) x2 ... ) (41 + 1)
fn convergence_expr(depth: usize) -> CoreExpr {
    let mut b = TreeBuilder::new();
    
    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    let var1 = b.push(CoreFrame::Var(VarId(1)));
    let mut current = b.push(CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![var1, one] });
    
    for i in 1..depth {
        let binder = VarId(i as u64);
        let lam = b.push(CoreFrame::Lam { binder, body: current });
        let arg_var = b.push(CoreFrame::Var(VarId(i as u64 + 1)));
        current = b.push(CoreFrame::App { fun: lam, arg: arg_var });
    }
    
    let lit41 = b.push(CoreFrame::Lit(Literal::LitInt(41)));
    let reducible_arg = b.push(CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![lit41, one] });
    
    let final_lam = b.push(CoreFrame::Lam { binder: VarId(depth as u64), body: current });
    let _root = b.push(CoreFrame::App { fun: final_lam, arg: reducible_arg });
    
    b.build()
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
            black_box(run_pipeline(&passes, &mut e).unwrap())
        })
    });

    // Pipeline on already optimized expr
    let mut optimized = expr.clone();
    run_pipeline(&default_passes(), &mut optimized).unwrap();
    c.bench_function("opt_pipeline_already_optimized", |b| {
        b.iter(|| {
            let mut e = optimized.clone();
            let passes = default_passes();
            black_box(run_pipeline(&passes, &mut e).unwrap())
        })
    });

    // Pipeline convergence
    let mut group = c.benchmark_group("opt_pipeline_convergence");
    for &depth in &[10, 50, 100] {
        let e = convergence_expr(depth);
        group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, _| {
            b.iter(|| {
                let mut e = e.clone();
                let passes = default_passes();
                black_box(run_pipeline(&passes, &mut e).unwrap())
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_optimize);
criterion_main!(benches);
