use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, FileFailurePersistence, TestRunner};
use std::sync::{Arc, Mutex};
use tidepool_eval::value::Value;
use tidepool_eval::{deep_force, eval, Env, VecHeap};
use tidepool_repr::{CoreFrame, DataConId, Literal, MapLayer, RecursiveTree, VarId};
use tidepool_testing::{compare, proptest as tp};

/// Proptest config with seed persistence that works for integration tests.
/// The default `SourceParallel` needs a sibling `lib.rs`/`main.rs` and fails
/// silently under `tests/`; `WithSource` writes the sibling
/// `proptest_infra_selftest.proptest-regressions` file (repo convention).
fn cfg(cases: u32) -> Config {
    Config {
        cases,
        source_file: Some(file!()),
        failure_persistence: Some(Box::new(FileFailurePersistence::WithSource(
            "proptest-regressions",
        ))),
        ..Config::default()
    }
}

// --- NAIVE REFERENCE COMPARATORS ---

/// Naive recursive comparator matching compare.rs intent.
fn naive_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Lit(la), Value::Lit(lb)) => lits_equal_naive(la, lb),
        (Value::Con(ta, fa), Value::Con(tb, fb)) => {
            ta == tb
                && fa.len() == fb.len()
                && fa.iter().zip(fb.iter()).all(|(x, y)| naive_eq(x, y))
        }
        (Value::ConFun(ta, aa, ga), Value::ConFun(tb, ab, gb)) => {
            ta == tb
                && aa == ab
                && ga.len() == gb.len()
                && ga.iter().zip(gb.iter()).all(|(x, y)| naive_eq(x, y))
        }
        (Value::Closure(..), Value::Closure(..)) => true,
        (Value::JoinCont(..), Value::JoinCont(..)) => true,
        _ => false,
    }
}

/// Helper for naive_eq handling NaN consistency.
fn lits_equal_naive(a: &Literal, b: &Literal) -> bool {
    match (a, b) {
        (Literal::LitFloat(x), Literal::LitFloat(y)) => {
            let fx = f32::from_bits(*x as u32);
            let fy = f32::from_bits(*y as u32);
            (fx.is_nan() && fy.is_nan()) || x == y
        }
        (Literal::LitDouble(x), Literal::LitDouble(y)) => {
            let fx = f64::from_bits(*x);
            let fy = f64::from_bits(*y);
            (fx.is_nan() && fy.is_nan()) || x == y
        }
        _ => a == b,
    }
}

/// Strict literal variant for differentials against tp::values_equal (which uses derived Literal Eq).
fn naive_eq_strict_lits(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Lit(la), Value::Lit(lb)) => la == lb,
        (Value::Con(ta, fa), Value::Con(tb, fb)) => {
            ta == tb
                && fa.len() == fb.len()
                && fa
                    .iter()
                    .zip(fb.iter())
                    .all(|(x, y)| naive_eq_strict_lits(x, y))
        }
        (Value::ConFun(ta, aa, ga), Value::ConFun(tb, ab, gb)) => {
            ta == tb
                && aa == ab
                && ga.len() == gb.len()
                && ga
                    .iter()
                    .zip(gb.iter())
                    .all(|(x, y)| naive_eq_strict_lits(x, y))
        }
        (Value::Closure(..), Value::Closure(..)) => true,
        (Value::JoinCont(..), Value::JoinCont(..)) => true,
        _ => false,
    }
}

// --- VALUE GENERATORS & MUTATORS ---

fn arb_literal() -> impl Strategy<Value = Literal> {
    prop_oneof![
        any::<i64>().prop_map(Literal::LitInt),
        any::<u64>().prop_map(Literal::LitWord),
        any::<char>().prop_map(Literal::LitChar),
        any::<u32>().prop_map(|b| Literal::LitFloat(b as u64)),
        any::<u64>().prop_map(Literal::LitDouble),
        // NaN patterns
        Just(Literal::LitFloat(f32::NAN.to_bits() as u64)),
        Just(Literal::LitDouble(f64::NAN.to_bits())),
        Just(Literal::LitDouble(0x7ff8000000000001u64)),
    ]
}

fn arb_value(depth: u32) -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        arb_literal().prop_map(Value::Lit),
        Just(Value::Closure(
            Env::new(),
            VarId(0),
            RecursiveTree {
                nodes: vec![CoreFrame::Var(VarId(0))]
            }
        )),
        Just(Value::JoinCont(
            vec![VarId(0)],
            RecursiveTree {
                nodes: vec![CoreFrame::Var(VarId(0))]
            },
            Env::new()
        )),
    ];

    leaf.prop_recursive(depth, 60, 4, |inner| {
        prop_oneof![
            (
                any::<u64>().prop_map(|n| DataConId(n % 10)),
                prop::collection::vec(inner.clone(), 0..4)
            )
                .prop_map(|(tag, fields)| Value::Con(tag, fields)),
            (
                any::<u64>().prop_map(|n| DataConId(n % 10)),
                1..5usize,
                prop::collection::vec(inner, 0..3)
            )
                .prop_map(|(tag, arity, args)| { Value::ConFun(tag, arity.max(args.len()), args) }),
        ]
    })
}

/// Generator that ensures the root is a Con with at least one field, for near-miss robustness.
fn arb_value_root_mutable(depth: u32) -> impl Strategy<Value = Value> {
    (
        any::<u64>().prop_map(|n| DataConId(n % 10)),
        prop::collection::vec(arb_value(depth - 1), 1..4),
    )
        .prop_map(|(tag, fields)| Value::Con(tag, fields))
}

fn mutate_value(v: &mut Value, path: &mut Vec<usize>, seed: u64) -> bool {
    match v {
        Value::Lit(l) => {
            match l {
                Literal::LitInt(n) => *n = n.wrapping_add(1),
                Literal::LitWord(n) => *n = n.wrapping_add(1),
                Literal::LitChar(c) => {
                    *c = char::from_u32((*c as u32).wrapping_add(1)).unwrap_or('A')
                }
                Literal::LitFloat(n) => {
                    let mut bits = *n as u32;
                    let f = f32::from_bits(bits);
                    bits = if f.is_nan() {
                        0x3f800000
                    } else {
                        bits.wrapping_add(1)
                    };
                    *n = bits as u64;
                }
                Literal::LitDouble(n) => {
                    let f = f64::from_bits(*n);
                    *n = if f.is_nan() {
                        0x3ff0000000000000
                    } else {
                        n.wrapping_add(1)
                    };
                }
                Literal::LitString(bs) => {
                    if bs.is_empty() {
                        bs.push(1);
                    } else {
                        bs[0] = bs[0].wrapping_add(1);
                    }
                }
            }
            true
        }
        Value::Con(tag, fields) => {
            if fields.is_empty() || path.is_empty() {
                if (seed & 1) == 0 {
                    tag.0 = tag.0.wrapping_add(1);
                } else if fields.is_empty() {
                    fields.push(Value::Lit(Literal::LitInt(1)));
                } else {
                    fields.pop();
                }
                true
            } else {
                let idx = path.remove(0) % fields.len();
                mutate_value(&mut fields[idx], path, seed >> 1)
            }
        }
        Value::ConFun(tag, arity, args) => {
            if args.is_empty() || path.is_empty() {
                if (seed & 1) == 0 {
                    tag.0 = tag.0.wrapping_add(1);
                } else {
                    *arity = arity.wrapping_add(1);
                }
                true
            } else {
                let idx = path.remove(0) % args.len();
                mutate_value(&mut args[idx], path, seed >> 1)
            }
        }
        _ => false,
    }
}

fn substitute_closures(v: &mut Value) {
    match v {
        Value::Con(_, fields) => {
            for f in fields {
                substitute_closures(f);
            }
        }
        Value::ConFun(_, _, args) => {
            for a in args {
                substitute_closures(a);
            }
        }
        Value::Closure(_, var, body) => {
            var.0 += 1;
            body.nodes = vec![CoreFrame::Lit(Literal::LitInt(1))];
        }
        Value::JoinCont(vars, body, _) => {
            vars.push(VarId(99));
            body.nodes = vec![CoreFrame::Lit(Literal::LitInt(2))];
        }
        _ => {}
    }
}

// --- INDEPENDENT DEPTH WALKER ---

fn get_tree_depth(tree: &RecursiveTree<CoreFrame<usize>>) -> u32 {
    if tree.nodes.is_empty() {
        return 0;
    }
    let mut depths = vec![0u32; tree.nodes.len()];
    for (i, node) in tree.nodes.iter().enumerate() {
        let mut max_child_depth = None;
        node.clone().map_layer(|child_idx| {
            assert!(
                child_idx < i,
                "Topological invariant violated: child {} >= parent {}",
                child_idx,
                i
            );
            let d = depths[child_idx];
            max_child_depth = Some(max_child_depth.map_or(d, |curr: u32| curr.max(d)));
            child_idx
        });
        depths[i] = max_child_depth.map_or(0, |d| d + 1);
    }
    depths[tree.nodes.len() - 1]
}

fn check_reachability(tree: &RecursiveTree<CoreFrame<usize>>) -> Result<(), usize> {
    if tree.nodes.is_empty() {
        return Ok(());
    }
    let n = tree.nodes.len();
    let mut reachable = vec![false; n];
    let mut stack = vec![n - 1];
    reachable[n - 1] = true;
    while let Some(idx) = stack.pop() {
        tree.nodes[idx].clone().map_layer(|child| {
            if !reachable[child] {
                reachable[child] = true;
                stack.push(child);
            }
            child
        });
    }
    for (i, &r) in reachable.iter().enumerate() {
        if !r {
            return Err(i);
        }
    }
    Ok(())
}

// --- THE 5 LIVE PROPERTY GROUPS ---

#[test]
fn g1_comparator_differential() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(cfg(600));

            runner
                .run(&(arb_value(4), arb_value(4)), |(a, b)| {
                    let eq_tp = compare::values_equal(&a, &b);
                    let eq_naive = naive_eq(&a, &b);
                    let eq_sym = compare::values_equal(&b, &a);
                    prop_assert_eq!(eq_tp, eq_naive, "Differential failure (tp vs naive)");
                    prop_assert_eq!(eq_tp, eq_sym, "Symmetry failure");
                    Ok(())
                })
                .unwrap();

            runner
                .run(&arb_value(4), |v| {
                    let mut v2 = v.clone();
                    substitute_closures(&mut v2);
                    prop_assert!(
                        compare::values_equal(&v, &v2),
                        "Substitution equality failure"
                    );
                    Ok(())
                })
                .unwrap();

            runner
                .run(
                    &(arb_value_root_mutable(4), any::<Vec<usize>>(), any::<u64>()),
                    |(v1, mut path, seed)| {
                        let mut v2 = v1.clone();
                        if mutate_value(&mut v2, &mut path, seed) {
                            prop_assert!(
                                !compare::values_equal(&v1, &v2),
                                "Mutation inequality failure: {:?} vs {:?}",
                                v1,
                                v2
                            );
                            prop_assert!(!naive_eq(&v1, &v2), "Naive mutation inequality failure");
                        }
                        Ok(())
                    },
                )
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn g2_shared_subtree_and_restricted_tp_values_equal() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(cfg(200));

            // Lit/Con only generator
            let arb_lit_con = prop_oneof![arb_literal().prop_map(Value::Lit),].prop_recursive(
                4,
                40,
                4,
                |inner| {
                    (
                        any::<u64>().prop_map(|n| DataConId(n % 10)),
                        prop::collection::vec(inner, 0..4),
                    )
                        .prop_map(|(tag, fields)| Value::Con(tag, fields))
                },
            );

            // Shared subtree adversarial
            runner
                .run(
                    &(arb_lit_con.clone(), any::<Vec<usize>>(), any::<u64>()),
                    |(s, mut path, seed)| {
                        let fields = vec![s.clone(); 16];
                        let mut fields2 = fields.clone();
                        if mutate_value(&mut fields2[8], &mut path, seed) {
                            let v1 = Value::Con(DataConId(0), fields);
                            let v2 = Value::Con(DataConId(0), fields2);
                            prop_assert!(!compare::values_equal(&v1, &v2));
                            prop_assert!(!tp::values_equal(&v1, &v2));
                        }
                        Ok(())
                    },
                )
                .unwrap();

            // tp::values_equal restricted to Lit/Con agreement
            runner
                .run(&(arb_lit_con.clone(), arb_lit_con), |(a, b)| {
                    let eq_tp = tp::values_equal(&a, &b);
                    let eq_naive = naive_eq_strict_lits(&a, &b);
                    prop_assert_eq!(eq_tp, eq_naive);
                    Ok(())
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn g3_tree_builder_invariants() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(cfg(300));

            use tidepool_repr::builder::TreeBuilder;

            runner
                .run(&prop::collection::vec(1..8usize, 2..5), |lens| {
                    let mut builder = TreeBuilder::new();
                    let mut expected_len = 0;
                    for size in lens {
                        let mut sub = TreeBuilder::new();
                        for _ in 0..size {
                            let curr = sub.clone().build().nodes.len();
                            let frame = if curr == 0 {
                                CoreFrame::Lit(Literal::LitInt(0))
                            } else {
                                CoreFrame::App { fun: 0, arg: 0 }
                            };
                            sub.push(frame);
                        }
                        let offset = builder.push_tree(sub);
                        prop_assert_eq!(offset, expected_len);
                        expected_len += size;
                    }
                    let final_tree = builder.build();
                    for (i, node) in final_tree.nodes.iter().enumerate() {
                        node.clone().map_layer(|child| {
                            assert!(child < i);
                            child
                        });
                    }
                    Ok(())
                })
                .unwrap();

            runner
                .run(&tidepool_testing::gen::arb_core_expr_depth(5), |expr| {
                    for (i, node) in expr.nodes.iter().enumerate() {
                        node.clone().map_layer(|child| {
                            assert!(child < i);
                            child
                        });
                    }
                    prop_assert!(
                        check_reachability(&expr).is_ok(),
                        "Orphan node in generated tree"
                    );
                    Ok(())
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn g4_generator_contracts() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(cfg(300));

            for d in [3, 5, 7] {
                let mut max_overage = 0i32;
                let mut max_depth = 0u32;
                let strat = tidepool_testing::gen::arb_core_expr_depth(d);
                for _ in 0..300 {
                    let tree = strat.new_tree(&mut runner).unwrap().current();
                    let measured = get_tree_depth(&tree);
                    max_depth = max_depth.max(measured);
                    max_overage = max_overage.max(measured as i32 - d as i32);
                }
                eprintln!(
                    "depth characterization: d={} max_depth={} max_overage={}",
                    d, max_depth, max_overage
                );
                // Regression bound for the TRUE behavior (see BUG-3): overage
                // scales with d, not a constant. An App spine stacks
                // Fun(ak, Fun(ak-1, ... ty)) types; when the fun position hits
                // the depth-0 leaf fallback, the whole stack collapses into a
                // Lam chain at once — worst case ~2d + type-nesting (~4).
                // Measured maxima: d=3 -> 8, d=5 -> 11, d=7 -> 14 (~2d).
                assert!(
                    max_depth <= 2 * d + 8,
                    "Depth overage beyond the characterized ~2d bound for d={}: {}",
                    d,
                    max_depth
                );
            }

            let compared = std::cell::Cell::new(0usize);
            runner
                .run(&tidepool_testing::gen::arb_ground_expr_depth(3), |expr| {
                    let mut heap = VecHeap::new();
                    if let Ok(v) = eval(&expr, &Env::new(), &mut heap) {
                        if let Ok(fv) = deep_force(v, &mut heap) {
                            prop_assert!(!compare::contains_closure(&fv));
                            if let Value::Closure(..) = fv {
                                prop_assert!(false);
                            }
                            compared.set(compared.get() + 1);
                        }
                    }
                    Ok(())
                })
                .unwrap();
            assert!(compared.get() >= 25);
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn g5_cbor_roundtrip() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(cfg(100));
            runner
                .run(&tidepool_testing::gen::arb_core_expr_depth(5), |expr| {
                    let bytes = tidepool_repr::serial::write_cbor(&expr).unwrap();
                    let recovered = tidepool_repr::serial::read_cbor(&bytes).unwrap();
                    prop_assert_eq!(expr, recovered);
                    Ok(())
                })
                .unwrap();

            let mut runner2 = TestRunner::new(cfg(60));
            runner2
                .run(
                    &tidepool_testing::gen::arb_core_expr_weighted(7, 5, 4, 4),
                    |expr| {
                        let bytes = tidepool_repr::serial::write_cbor(&expr).unwrap();
                        let recovered = tidepool_repr::serial::read_cbor(&bytes).unwrap();
                        prop_assert_eq!(expr, recovered);
                        Ok(())
                    },
                )
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}

// --- NAN CHARACTERIZATION ---

#[test]
fn nan_characterization() {
    let nan1 = Value::Lit(Literal::LitDouble(f64::NAN.to_bits()));
    let nan2 = Value::Lit(Literal::LitDouble(0x7ff8000000000001u64));
    // compare::values_equal uses f64::is_nan() which is true for both
    assert!(compare::values_equal(&nan1, &nan2));
    // tp::values_equal uses bitwise Literal equality (derived)
    assert!(!tp::values_equal(&nan1, &nan2));
}

// --- THE 3 #[ignore] REPROS ---

#[test]
#[ignore = "BUG-1: proptest::values_equal equates heterogeneous pairs (Lit vs Con) — false-positive class"]
fn bug1_proptest_values_equal_heterogeneous() {
    let lit = Value::Lit(Literal::LitInt(1));
    let con = Value::Con(DataConId(0), vec![]);
    // This SHOULD be false, but BUG-1 treats it as true.
    assert!(
        !tp::values_equal(&lit, &con),
        "BUG-1: tp::values_equal incorrectly equated Lit and Con"
    );
}

#[test]
#[ignore = "BUG-2: compare::values_equal is not reflexive on ByteArray — false-negative class"]
fn bug2_compare_values_equal_bytearray_reflexivity() {
    let ba = Arc::new(Mutex::new(vec![1, 2, 3]));
    let v = Value::ByteArray(ba);
    // This SHOULD be true, but BUG-2 returns false for ByteArrays.
    assert!(
        compare::values_equal(&v, &v.clone()),
        "BUG-2: compare::values_equal is not reflexive for ByteArray"
    );
}

#[test]
#[ignore = "BUG-3: arb_core_expr_depth(d) exceeds its depth cap d"]
fn bug3_generator_depth_violation() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(cfg(300));
            let strat = tidepool_testing::gen::arb_core_expr_depth(3);
            runner
                .run(&strat, |expr| {
                    let measured = get_tree_depth(&expr);
                    prop_assert!(
                        measured <= 3,
                        "BUG-3: measured depth {} exceeds cap 3",
                        measured
                    );
                    Ok(())
                })
                .unwrap();
        })
        .unwrap();
    handle.join().unwrap();
}
