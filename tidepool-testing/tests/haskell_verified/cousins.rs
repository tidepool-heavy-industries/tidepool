use crate::{arb_int, run_template};
use proptest::prelude::*;
use serde_json::json;

// Template 12 (list-comprehension)
fn gen_list_comprehension() -> impl Strategy<Value = (String, serde_json::Value)> {
    (arb_int(), proptest::collection::vec(arb_int(), 0..=8)).prop_map(|(n, xs): (i64, Vec<i64>)| {
        let xs_str = xs
            .iter()
            .map(|x: &i64| x.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("([ x * ({}) | x <- [{}], even x ] :: [Int])", n, xs_str);
        let expected_list: Vec<i64> = xs.iter().filter(|x| *x % 2 == 0).map(|x| x * n).collect();
        (src, json!(expected_list))
    })
}

#[test]
fn test_list_comprehension() {
    run_template(50, gen_list_comprehension());
}

// Template 13 (maybe-monadic)
fn gen_maybe_monadic() -> impl Strategy<Value = (String, serde_json::Value)> {
    (
        proptest::option::of(arb_int()),
        proptest::option::of(arb_int()),
    )
        .prop_map(|(maybe_x, maybe_y): (Option<i64>, Option<i64>)| {
            let x_src = match maybe_x {
                Some(x) => format!("(Just ({}) :: Maybe Int)", x),
                None => "(Nothing :: Maybe Int)".to_string(),
            };
            let y_src = match maybe_y {
                Some(y) => format!("(Just ({}) :: Maybe Int)", y),
                None => "(Nothing :: Maybe Int)".to_string(),
            };
            let src = format!(
                "(do {{ x <- {}; y <- {}; pure (x + y) }} :: Maybe Int)",
                x_src, y_src
            );

            let expected = match (maybe_x, maybe_y) {
                (Some(x), Some(y)) => json!(x + y),
                _ => json!(null),
            };
            (src, expected)
        })
}

#[test]
fn test_maybe_monadic() {
    run_template(50, gen_maybe_monadic());
}

// Template 14 (list-fold)
fn gen_list_fold() -> impl Strategy<Value = (String, serde_json::Value)> {
    prop_oneof![
        proptest::collection::vec(arb_int(), 0..=8).prop_map(|xs: Vec<i64>| {
            let xs_str = xs
                .iter()
                .map(|x: &i64| x.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let src = format!("(foldl' (+) 0 [{}] :: Int)", xs_str);
            let expected: i64 = xs.iter().sum();
            (src, json!(expected))
        }),
        proptest::collection::vec(arb_int(), 0..=8).prop_map(|xs: Vec<i64>| {
            let xs_str = xs
                .iter()
                .map(|x: &i64| x.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let src = format!("(foldr (+) 0 [{}] :: Int)", xs_str);
            let expected: i64 = xs.iter().sum();
            (src, json!(expected))
        })
    ]
}

#[test]
fn test_list_fold() {
    run_template(50, gen_list_fold());
}
