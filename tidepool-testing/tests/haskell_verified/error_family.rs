use crate::{run_template, run_template_with_imports};
use proptest::prelude::*;
use serde_json::json;

fn gen_computed_list() -> impl Strategy<Value = (i64, i64, Vec<i64>)> {
    (0i64..=5, 6i64..=15).prop_map(|(k, n)| {
        let xs: Vec<i64> = (1..=n).filter(|x| *x > k).collect();
        (k, n, xs)
    })
}

#[test]
fn proptest_head() {
    let strat = gen_computed_list().prop_map(|(k, n, xs)| {
        let src = format!("head (filter (\\x -> x > {}) (enumFromTo 1 {}))", k, n);
        (src, json!(xs[0]))
    });
    run_template(25, strat);
}

#[test]
fn proptest_tail() {
    let strat = gen_computed_list().prop_map(|(k, n, xs)| {
        let src = format!("tail (filter (\\x -> x > {}) (enumFromTo 1 {}))", k, n);
        (src, json!(&xs[1..]))
    });
    run_template(25, strat);
}

#[test]
fn proptest_last() {
    let strat = gen_computed_list().prop_map(|(k, n, xs)| {
        let src = format!("last (filter (\\x -> x > {}) (enumFromTo 1 {}))", k, n);
        (src, json!(xs.last().unwrap()))
    });
    run_template(25, strat);
}

#[test]
fn proptest_init() {
    let strat = gen_computed_list().prop_map(|(k, n, xs)| {
        let src = format!("init (filter (\\x -> x > {}) (enumFromTo 1 {}))", k, n);
        (src, json!(&xs[..xs.len() - 1]))
    });
    run_template(25, strat);
}

#[test]
fn proptest_maximum() {
    let strat = gen_computed_list().prop_map(|(k, n, xs)| {
        let src = format!("maximum (filter (\\x -> x > {}) (enumFromTo 1 {}))", k, n);
        (src, json!(xs.iter().max().unwrap()))
    });
    run_template(25, strat);
}

#[test]
fn proptest_minimum() {
    let strat = gen_computed_list().prop_map(|(k, n, xs)| {
        let src = format!("minimum (filter (\\x -> x > {}) (enumFromTo 1 {}))", k, n);
        (src, json!(xs.iter().min().unwrap()))
    });
    run_template(25, strat);
}

#[test]
fn proptest_foldr1_add() {
    let strat = gen_computed_list().prop_map(|(k, n, xs)| {
        let src = format!(
            "foldr1 (+) (filter (\\x -> x > {}) (enumFromTo 1 {}))",
            k, n
        );
        let expected: i64 = xs.iter().sum();
        (src, json!(expected))
    });
    run_template_with_imports(25, strat, &["import Data.List (foldr1)"]);
}

#[test]
fn proptest_foldl1_add() {
    let strat = gen_computed_list().prop_map(|(k, n, xs)| {
        let src = format!(
            "foldl1 (+) (filter (\\x -> x > {}) (enumFromTo 1 {}))",
            k, n
        );
        let expected: i64 = xs.iter().sum();
        (src, json!(expected))
    });
    run_template_with_imports(25, strat, &["import Data.List (foldl1)"]);
}

#[test]
fn proptest_foldl1_sub() {
    let strat = gen_computed_list().prop_map(|(k, n, xs)| {
        let src = format!(
            "foldl1 (-) (filter (\\x -> x > {}) (enumFromTo 1 {}))",
            k, n
        );
        let expected = xs[1..].iter().fold(xs[0], |acc, x| acc - x);
        (src, json!(expected))
    });
    run_template_with_imports(25, strat, &["import Data.List (foldl1)"]);
}

#[test]
#[ignore = "STANDING: cycle from Data.List hits 'unresolved variable' (recursive loop-breaker, no unfolding; fat-iface miss). Remove ignore when resolved."]
fn cycle_unresolved() {
    let strat = (0i64..=5, 6i64..=15).prop_map(|(k, n)| {
        let src = format!(
            "take 5 (cycle (filter (\\x -> x > {}) (enumFromTo 1 {})))",
            k, n
        );
        let xs: Vec<i64> = (1..=n).filter(|x| *x > k).collect();
        let expected: Vec<i64> = xs.iter().cycle().take(5).copied().collect();
        (src, json!(expected))
    });
    run_template_with_imports(25, strat, &["import Data.List (cycle)"]);
}
