use tidepool_eval::{env::Env, heap::Heap, heap::VecHeap};
use tidepool_heap::arena::ArenaHeap;
use tidepool_repr::{CoreExpr, CoreFrame, RecursiveTree, VarId};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

fn dummy_expr() -> CoreExpr {
    RecursiveTree {
        nodes: vec![CoreFrame::Var(VarId(0))],
    }
}

fn bench_heap(c: &mut Criterion) {
    let expr = dummy_expr();
    let env = Env::new();

    // 1. VecHeap allocation
    let mut group = c.benchmark_group("vecheap_alloc");
    for size in [100, 1000, 10000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter(|| {
                let mut heap = VecHeap::new();
                for _ in 0..size {
                    black_box(heap.alloc(env.clone(), expr.clone()));
                }
            });
        });
    }
    group.finish();

    // 2. ArenaHeap raw allocation (64-byte objects)
    let mut group = c.benchmark_group("arena_alloc_raw_64");
    for size in [100, 1000, 10000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter(|| {
                let heap = ArenaHeap::with_capacity(size * 128); // Ensure enough space
                for _ in 0..size {
                    black_box(heap.alloc_raw(64));
                }
            });
        });
    }
    group.finish();

    // 3. ArenaHeap thunk allocation
    let mut group = c.benchmark_group("arena_thunk_alloc");
    for size in [100, 1000, 10000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            b.iter(|| {
                let mut heap = ArenaHeap::new();
                for _ in 0..size {
                    black_box(heap.alloc(env.clone(), expr.clone()));
                }
            });
        });
    }
    group.finish();

    // 4. GC cycle
    c.bench_function("gc_cycle_1000_500", |b| {
        b.iter_with_setup(
            || {
                let mut heap = ArenaHeap::new();
                let mut roots = Vec::new();
                for i in 0..1000 {
                    let id = heap.alloc(env.clone(), expr.clone());
                    if i % 2 == 0 {
                        roots.push(id);
                    }
                }
                (heap, roots)
            },
            |(mut heap, roots)| {
                black_box(heap.collect_garbage(&roots));
            },
        );
    });
}

criterion_group!(benches, bench_heap);
criterion_main!(benches);