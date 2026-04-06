use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use tidepool_eval::{env::Env, heap::Heap, heap::VecHeap};
use tidepool_heap::arena::ArenaHeap;
use tidepool_repr::{CoreExpr, CoreFrame, RecursiveTree, VarId};

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
                    black_box(heap.alloc_raw(64).unwrap());
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

    // 4. GC stress test
    let mut group = c.benchmark_group("gc_stress");
    for &size in &[1000, 10000, 100000] {
        for &ratio in &[10, 50, 90] {
            group.bench_with_input(
                BenchmarkId::from_parameter(format!("{}_{}%", size, ratio)),
                &(size, ratio),
                |b, &(size, ratio)| {
                    b.iter_with_setup(
                        || {
                            let mut heap = ArenaHeap::with_capacity(size * 256); // Plenty of space
                            let mut roots = Vec::new();
                            for i in 0..size {
                                let id = heap.alloc(env.clone(), expr.clone());
                                if (i * 100 / size) < ratio {
                                    roots.push(id);
                                }
                            }
                            (heap, roots)
                        },
                        |(mut heap, roots)| {
                            black_box(heap.collect_garbage(&roots));
                        },
                    );
                },
            );
        }
    }
    group.finish();

    // 5. GC deep nested structure
    let mut group = c.benchmark_group("gc_deep_nested");
    for &depth in &[1000, 5000, 10000] {
        group.bench_with_input(
            BenchmarkId::from_parameter(depth),
            &depth,
            |b, &depth| {
                b.iter_with_setup(
                    || {
                        let mut heap = ArenaHeap::with_capacity(depth * 256);
                        let mut last_id = heap.alloc(env.clone(), expr.clone());

                        for _ in 0..depth {
                            let mut next_env = Env::new();
                            next_env.insert(VarId(0), tidepool_eval::value::Value::ThunkRef(last_id));
                            last_id = heap.alloc(next_env, expr.clone());
                        }
                        (heap, vec![last_id])
                    },
                    |(mut heap, roots)| {
                        black_box(heap.collect_garbage(&roots));
                    },
                );
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_heap);
criterion_main!(benches);
