use core_eval::{env::Env, eval::eval, heap::VecHeap};
use core_repr::{
    Alt, AltCon, CoreExpr, CoreFrame, DataConId, Literal, PrimOpKind, RecursiveTree, VarId,
};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

/// Builds a small expression: (\x -> x + 1) 41
/// Baseline expectation: < 1us (interpreter overhead only)
fn small_expr() -> CoreExpr {
    RecursiveTree {
        nodes: vec![
            CoreFrame::Var(VarId(1)),            // 0: x
            CoreFrame::Lit(Literal::LitInt(1)),  // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2: x + 1
            CoreFrame::Lam {
                binder: VarId(1),
                body: 2,
            }, // 3: \x -> x + 1
            CoreFrame::Lit(Literal::LitInt(41)), // 4
            CoreFrame::App { fun: 3, arg: 4 },   // 5: (\x -> x + 1) 41
        ],
    }
}

/// Builds a medium expression (~50 nodes): arithmetic chain via let-bindings
/// Baseline expectation: < 10us
fn medium_expr() -> CoreExpr {
    let mut nodes = Vec::new();
    
    // Build from inside out
    // result = Var(24)
    nodes.push(CoreFrame::Var(VarId(24)));
    let mut current_body = nodes.len() - 1;
    
    for i in (0..25).rev() {
        let binder = VarId(i as u64);
        let rhs = if i == 0 {
            nodes.push(CoreFrame::Lit(Literal::LitInt(1)));
            nodes.len() - 1
        } else {
            nodes.push(CoreFrame::Var(VarId(i as u64 - 1)));
            let v_ref = nodes.len() - 1;
            nodes.push(CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![v_ref, v_ref],
            });
            nodes.len() - 1
        };
        
        nodes.push(CoreFrame::LetNonRec { binder, rhs, body: current_body });
        current_body = nodes.len() - 1;
    }

    RecursiveTree { nodes }
}

/// Builds a large expression (~500 nodes): deeply nested arithmetic chain
/// Baseline expectation: < 100us
fn large_expr() -> CoreExpr {
    let mut nodes = Vec::new();
    
    nodes.push(CoreFrame::Var(VarId(124)));
    let mut current_body = nodes.len() - 1;
    
    for i in (0..125).rev() {
        let binder = VarId(i as u64);
        let rhs = if i == 0 {
            nodes.push(CoreFrame::Lit(Literal::LitInt(1)));
            nodes.len() - 1
        } else {
            nodes.push(CoreFrame::Var(VarId(i as u64 - 1)));
            let v_ref = nodes.len() - 1;
            nodes.push(CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![v_ref, v_ref],
            });
            nodes.len() - 1
        };
        
        nodes.push(CoreFrame::LetNonRec { binder, rhs, body: current_body });
        current_body = nodes.len() - 1;
    }

    RecursiveTree { nodes }
}

/// Builds a thunk expression: let x = expensive in x + x
/// Exercises thunk caching.
/// Baseline expectation: should be faster than double evaluation.
fn thunk_expr() -> CoreExpr {
    let mut nodes = Vec::new();
    // Expensive sub-expression (100 additions)
    nodes.push(CoreFrame::Lit(Literal::LitInt(1)));
    for _ in 0..100 {
        let last = nodes.len() - 1;
        nodes.push(CoreFrame::PrimOp {
            op: PrimOpKind::IntAdd,
            args: vec![last, last],
        });
    }
    let expensive_rhs = nodes.len() - 1;

    nodes.push(CoreFrame::Var(VarId(1))); // x
    let x_ref = nodes.len() - 1;
    nodes.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![x_ref, x_ref],
    }); // x + x
    let body = nodes.len() - 1;

    nodes.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: expensive_rhs,
        body,
    });

    RecursiveTree { nodes }
}

/// Builds a case expression: nested pattern matching
/// Baseline expectation: < 5us
fn case_expr() -> CoreExpr {
    RecursiveTree {
        nodes: vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0],
            }, // 1: Just 42
            CoreFrame::Var(VarId(10)),           // 2: x
            CoreFrame::Case {
                scrutinee: 1,
                binder: VarId(11),
                alts: vec![
                    Alt {
                        con: AltCon::DataAlt(DataConId(2)), // Nothing
                        binders: vec![],
                        body: 0,
                    },
                    Alt {
                        con: AltCon::DataAlt(DataConId(1)), // Just x
                        binders: vec![VarId(10)],
                        body: 2,
                    },
                ],
            }, // 3
        ],
    }
}

fn bench_eval(c: &mut Criterion) {
    let small = small_expr();
    let medium = medium_expr();
    let large = large_expr();
    let thunk = thunk_expr();
    let case = case_expr();

    let env = Env::new();

    c.bench_function("eval_small", |b| {
        b.iter(|| {
            let mut heap = VecHeap::new();
            black_box(eval(&small, &env, &mut heap).unwrap())
        })
    });

    c.bench_function("eval_medium", |b| {
        b.iter(|| {
            let mut heap = VecHeap::new();
            black_box(eval(&medium, &env, &mut heap).unwrap())
        })
    });

    c.bench_function("eval_large", |b| {
        b.iter(|| {
            let mut heap = VecHeap::new();
            black_box(eval(&large, &env, &mut heap).unwrap())
        })
    });

    c.bench_function("eval_thunk", |b| {
        b.iter(|| {
            let mut heap = VecHeap::new();
            black_box(eval(&thunk, &env, &mut heap).unwrap())
        })
    });

    c.bench_function("eval_case", |b| {
        b.iter(|| {
            let mut heap = VecHeap::new();
            black_box(eval(&case, &env, &mut heap).unwrap())
        })
    });
}

criterion_group!(benches, bench_eval);
criterion_main!(benches);