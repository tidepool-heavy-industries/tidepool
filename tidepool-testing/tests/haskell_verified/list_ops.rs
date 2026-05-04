//! Verified-generator templates for list operations: filter, map, take, drop,
//! zip/unzip, reverse, sort, replicate, elem.
//!
//! Stub: filled in by the `list_ops` leaf. Follows the patterns in
//! `fmap.rs` / `text.rs` / `cousins.rs`. ASCII-only, Int-only, pinned types,
//! `ProptestConfig::with_cases(50)` per template.

#![allow(unused_imports)]

use crate::{arb_int, run_template};
use proptest::prelude::*;
use serde_json::json;

// Template 1 (list-map): `map (+ ({n})) [{xs}] :: [Int]`
fn gen_list_map() -> impl Strategy<Value = (String, serde_json::Value)> {
    (arb_int(), proptest::collection::vec(arb_int(), 0..=8)).prop_map(|(n, xs): (i64, Vec<i64>)| {
        let xs_str = xs
            .iter()
            .map(|x: &i64| x.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("(map (+ ({})) [{}] :: [Int])", n, xs_str);
        let expected_list: Vec<i64> = xs.iter().map(|x: &i64| x + n).collect();
        (src, json!(expected_list))
    })
}

#[test]
fn test_list_map() {
    run_template(50, gen_list_map());
}

// Template 2 (list-filter): `filter even [{xs}] :: [Int]`
fn gen_list_filter() -> impl Strategy<Value = (String, serde_json::Value)> {
    proptest::collection::vec(arb_int(), 0..=8).prop_map(|xs: Vec<i64>| {
        let xs_str = xs
            .iter()
            .map(|x: &i64| x.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("(filter even [{}] :: [Int])", xs_str);
        let expected_list: Vec<i64> = xs.iter().filter(|x| *x % 2 == 0).copied().collect();
        (src, json!(expected_list))
    })
}

#[test]
fn test_list_filter() {
    run_template(50, gen_list_filter());
}

// Template 3 (list-take-drop): `take {n} [{xs}] :: [Int]` and `drop {n} [{xs}] :: [Int]`
fn gen_list_take_drop() -> impl Strategy<Value = (String, serde_json::Value)> {
    prop_oneof![
        (0usize..=12, proptest::collection::vec(arb_int(), 0..=8)).prop_map(|(n, xs)| {
            let xs_str = xs
                .iter()
                .map(|x: &i64| x.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let src = format!("(take {} [{}] :: [Int])", n, xs_str);
            let expected_list: Vec<i64> = xs.iter().take(n).copied().collect();
            (src, json!(expected_list))
        }),
        (0usize..=12, proptest::collection::vec(arb_int(), 0..=8)).prop_map(|(n, xs)| {
            let xs_str = xs
                .iter()
                .map(|x: &i64| x.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let src = format!("(drop {} [{}] :: [Int])", n, xs_str);
            let expected_list: Vec<i64> = xs.iter().skip(n).copied().collect();
            (src, json!(expected_list))
        }),
    ]
}

#[test]
fn test_list_take_drop() {
    run_template(50, gen_list_take_drop());
}

// Template 4 (list-zip): `zip [{xs}] [{ys}] :: [(Int, Int)]`
fn gen_list_zip() -> impl Strategy<Value = (String, serde_json::Value)> {
    (
        proptest::collection::vec(arb_int(), 0..=8),
        proptest::collection::vec(arb_int(), 0..=8),
    )
        .prop_map(|(xs, ys): (Vec<i64>, Vec<i64>)| {
            let xs_str = xs
                .iter()
                .map(|x: &i64| x.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let ys_str = ys
                .iter()
                .map(|y: &i64| y.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let src = format!("(zip [{}] [{}] :: [(Int, Int)])", xs_str, ys_str);
            let expected_list: Vec<serde_json::Value> = xs
                .iter()
                .zip(ys.iter())
                .map(|(a, b)| json!([a, b]))
                .collect();
            (src, json!(expected_list))
        })
}

#[test]
fn test_list_zip() {
    run_template(50, gen_list_zip());
}

// Template 5 (list-unzip): `unzip [{pairs}] :: ([Int], [Int])`
fn gen_list_unzip() -> impl Strategy<Value = (String, serde_json::Value)> {
    proptest::collection::vec((arb_int(), arb_int()), 0..=8).prop_map(|pairs: Vec<(i64, i64)>| {
        let pairs_str = pairs
            .iter()
            .map(|(a, b)| format!("({}, {})", a, b))
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("(unzip [{}] :: ([Int], [Int]))", pairs_str);
        let (xs, ys): (Vec<i64>, Vec<i64>) = pairs.into_iter().unzip();
        (src, json!([xs, ys]))
    })
}

#[test]
fn test_list_unzip() {
    run_template(50, gen_list_unzip());
}

// Template 6 (list-reverse): `reverse [{xs}] :: [Int]`
fn gen_list_reverse() -> impl Strategy<Value = (String, serde_json::Value)> {
    proptest::collection::vec(arb_int(), 0..=8).prop_map(|xs: Vec<i64>| {
        let xs_str = xs
            .iter()
            .map(|x: &i64| x.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("(reverse [{}] :: [Int])", xs_str);
        let expected_list: Vec<i64> = xs.iter().rev().copied().collect();
        (src, json!(expected_list))
    })
}

#[test]
fn test_list_reverse() {
    run_template(50, gen_list_reverse());
}

// Template 7 (list-sort): `sort [{xs}] :: [Int]`
fn gen_list_sort() -> impl Strategy<Value = (String, serde_json::Value)> {
    proptest::collection::vec(arb_int(), 0..=8).prop_map(|mut xs: Vec<i64>| {
        let xs_str = xs
            .iter()
            .map(|x: &i64| x.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("(sort [{}] :: [Int])", xs_str);
        xs.sort_unstable();
        (src, json!(xs))
    })
}

#[test]
fn test_list_sort() {
    run_template(50, gen_list_sort());
}

// Template 8 (list-replicate): `replicate {n} ({x}) :: [Int]`
fn gen_list_replicate() -> impl Strategy<Value = (String, serde_json::Value)> {
    (0usize..=8, arb_int()).prop_map(|(n, x): (usize, i64)| {
        let src = format!("(replicate {} ({}) :: [Int])", n, x);
        let expected_list: Vec<i64> = vec![x; n];
        (src, json!(expected_list))
    })
}

#[test]
fn test_list_replicate() {
    run_template(50, gen_list_replicate());
}

// Template 9 (list-elem): `elem ({n}) [{xs}] :: Bool`
fn gen_list_elem() -> impl Strategy<Value = (String, serde_json::Value)> {
    (arb_int(), proptest::collection::vec(arb_int(), 0..=8)).prop_map(|(n, xs): (i64, Vec<i64>)| {
        let xs_str = xs
            .iter()
            .map(|x: &i64| x.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("(elem ({}) ([{}] :: [Int]) :: Bool)", n, xs_str);
        let expected: bool = xs.contains(&n);
        (src, json!(expected))
    })
}

#[test]
fn test_list_elem() {
    run_template(50, gen_list_elem());
}
