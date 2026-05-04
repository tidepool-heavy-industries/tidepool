use crate::{arb_int, arb_text, run_template};
use proptest::prelude::*;
use serde_json::json;

// Template 1 (fmap-maybe): `(fmap (+{n}) ({maybe} :: Maybe Int))`
//
// In the `Nothing` arm the function is never applied, so `n` is fixed at 0
// to avoid burning compile-cache slots on semantically-identical sources.
fn gen_fmap_maybe() -> impl Strategy<Value = (String, serde_json::Value)> {
    prop_oneof![
        (arb_int(), arb_int()).prop_map(|(n, m): (i64, i64)| {
            let src = format!("(fmap (+({})) (Just ({}) :: Maybe Int))", n, m);
            (src, json!(m + n))
        }),
        Just(()).prop_map(|()| {
            let src = "(fmap (+(0)) (Nothing :: Maybe Int))".to_string();
            (src, json!(null))
        })
    ]
}

#[test]
fn test_fmap_maybe() {
    run_template(50, gen_fmap_maybe());
}

// Template 2 (fmap-list): `pure (fmap (* {n}) ([{xs}] :: [Int]))`
fn gen_fmap_list() -> impl Strategy<Value = (String, serde_json::Value)> {
    (arb_int(), proptest::collection::vec(arb_int(), 0..=8)).prop_map(|(n, xs): (i64, Vec<i64>)| {
        let xs_str = xs
            .iter()
            .map(|x: &i64| x.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("(fmap (* ({})) ([{}] :: [Int]))", n, xs_str);
        let expected_list: Vec<i64> = xs.iter().map(|x: &i64| x * n).collect();
        (src, json!(expected_list))
    })
}

#[test]
fn test_fmap_list() {
    run_template(50, gen_fmap_list());
}

// Template 3 (fmap-either): `(fmap (+{n}) (Right {m} :: Either Text Int))`
// and `(fmap (+0) (Left "{s}" :: Either Text Int))`
//
// In the `Left` arm the function is never applied; `n` is fixed at 0 to
// keep distinct sources count proportional to actual coverage.
fn gen_fmap_either() -> impl Strategy<Value = (String, serde_json::Value)> {
    prop_oneof![
        (arb_int(), arb_int()).prop_map(|(n, m): (i64, i64)| {
            let src = format!("(fmap (+({})) (Right ({}) :: Either T.Text Int))", n, m);
            (src, json!({"constructor": "Right", "fields": [m + n]}))
        }),
        arb_text().prop_map(|s: String| {
            let src = format!("(fmap (+(0)) (Left {:?} :: Either T.Text Int))", s);
            (src, json!({"constructor": "Left", "fields": [s]}))
        })
    ]
}

#[test]
fn test_fmap_either() {
    run_template(50, gen_fmap_either());
}

// Template 4 (fmap-nested): `(fmap (fmap (+{n})) (Just (Just {m} :: Maybe Int) :: Maybe (Maybe Int)))`
// and `(fmap (fmap (+{n})) (Just [{xs}] :: Maybe [Int]))`.
//
// Nested-Maybe stacks like `Maybe (Maybe Int)` collapse `Just Nothing` and
// `Nothing` to the same JSON `null` (the Haskell-to-JSON rendering convention
// for `Maybe`), so the JSON oracle cannot tell those two apart. We only
// exercise the unambiguous `Just (Just m)` case here. To preserve coverage of
// structural distinguishing — outer-Just-vs-Nothing AND inner-empty-vs-full —
// the second arm uses `Maybe [Int]` which renders as `[…]` vs `null` and
// distinguishes `Just []` from `Nothing`.
fn gen_fmap_nested() -> impl Strategy<Value = (String, serde_json::Value)> {
    prop_oneof![
        (arb_int(), arb_int()).prop_map(|(n, m): (i64, i64)| {
            let src = format!(
                "(fmap (fmap (+({}))) (Just (Just ({}) :: Maybe Int) :: Maybe (Maybe Int)))",
                n, m
            );
            (src, json!(m + n))
        }),
        (arb_int(), proptest::collection::vec(arb_int(), 0..=8)).prop_map(
            |(n, xs): (i64, Vec<i64>)| {
                let xs_str = xs.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
                let src = format!(
                    "(fmap (fmap (+({}))) (Just [{}] :: Maybe [Int]))",
                    n, xs_str
                );
                let expected: Vec<i64> = xs.iter().map(|x| x + n).collect();
                (src, json!(expected))
            }
        ),
        Just(()).prop_map(|()| {
            let src = "(fmap (fmap (+(0))) (Nothing :: Maybe [Int]))".to_string();
            (src, json!(null))
        })
    ]
}

#[test]
fn test_fmap_nested() {
    run_template(50, gen_fmap_nested());
}

// Template 5 (fmap-tuple): `pure (fmap (+{n}) (("{s}", {m}) :: (Text, Int)))`
fn gen_fmap_tuple() -> impl Strategy<Value = (String, serde_json::Value)> {
    (arb_int(), arb_text(), arb_int()).prop_map(|(n, s, m): (i64, String, i64)| {
        let src = format!("(fmap (+({})) (({:?}, {}) :: (T.Text, Int)))", n, s, m);
        let expected = json!([s, m + n]);
        (src, expected)
    })
}

#[test]
fn test_fmap_tuple() {
    run_template(50, gen_fmap_tuple());
}
