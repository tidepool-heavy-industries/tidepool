use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_repr::{CoreExpr, CoreFrame, DataConTable, Literal, PrimOpKind, TreeBuilder, VarId};

/// Builds a simple arithmetic expression tree of given size.
/// result = (((1 + 0) + 1) + 2) + ...
fn build_expr(size: usize) -> CoreExpr {
    assert!(size > 0, "size must be > 0");
    let mut b = TreeBuilder::new();
    let mut current = b.push(CoreFrame::Lit(Literal::LitInt(1)));
    for i in 0..((size.saturating_sub(1)) / 2) {
        let next = b.push(CoreFrame::Lit(Literal::LitInt(i as i64)));
        current = b.push(CoreFrame::PrimOp {
            op: PrimOpKind::IntAdd,
            args: vec![current, next],
        });
    }
    // If size is not odd, we might be slightly off, but it's fine for a benchmark scale.
    b.build()
}

/// Builds a tree with many let-bindings.
fn build_let_expr(size: usize) -> CoreExpr {
    let mut b = TreeBuilder::new();
    // Start with a literal to avoid unbound variables
    let mut current_body = b.push(CoreFrame::Lit(Literal::LitInt(0)));
    for i in 1..=size {
        let binder = VarId(i as u64);
        let rhs = b.push(CoreFrame::Lit(Literal::LitInt(i as i64)));
        current_body = b.push(CoreFrame::LetNonRec {
            binder,
            rhs,
            body: current_body,
        });
    }
    b.build()
}

fn bench_codegen(c: &mut Criterion) {
    let table = DataConTable::new();

    // 1. Compilation latency (Arithmetic tree)
    let mut group = c.benchmark_group("codegen_compile_arith");
    for &size in &[50, 200, 500] {
        let expr = build_expr(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                let machine = JitEffectMachine::compile(&expr, &table, 1 << 20).unwrap();
                black_box(machine)
            });
        });
    }
    group.finish();

    // 2. Compilation latency (Let chain)
    let mut group = c.benchmark_group("codegen_compile_let");
    for &size in &[50, 200, 500] {
        let expr = build_let_expr(size);
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            b.iter(|| {
                let machine = JitEffectMachine::compile(&expr, &table, 1 << 20).unwrap();
                black_box(machine)
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_codegen);
criterion_main!(benches);
