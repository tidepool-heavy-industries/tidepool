use crate::{arb_int, arb_text, run_template};
use proptest::prelude::*;
use serde_json::json;

// Template 1 (fmap-maybe): `pure (fmap (+{n}) ({maybe} :: Maybe Int))`
fn gen_fmap_maybe() -> impl Strategy<Value = (String, serde_json::Value)> {
    (arb_int(), proptest::option::of(arb_int())).prop_map(|(n, maybe_m): (i64, Option<i64>)| {
        let src = match maybe_m {
            Some(m) => format!("(fmap (+({})) (Just ({}) :: Maybe Int))", n, m),
            None => format!("(fmap (+({})) (Nothing :: Maybe Int))", n),
        };
        let expected = match maybe_m {
            Some(m) => json!(m + n),
            None => json!(null),
        };
        (src, expected)
    })
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

// Template 3 (fmap-either): `pure (fmap (+{n}) (Right {m} :: Either Text Int))`
// and `pure (fmap (+{n}) (Left "{s}" :: Either Text Int))`
fn gen_fmap_either() -> impl Strategy<Value = (String, serde_json::Value)> {
    prop_oneof![
        (arb_int(), arb_int()).prop_map(|(n, m): (i64, i64)| {
            let src = format!("(fmap (+({})) (Right ({}) :: Either T.Text Int))", n, m);
            (src, json!({"constructor": "Right", "fields": [m + n]}))
        }),
        (arb_int(), arb_text()).prop_map(|(n, s): (i64, String)| {
            let src = format!("(fmap (+({})) (Left {:?} :: Either T.Text Int))", n, s);
            (src, json!({"constructor": "Left", "fields": [s]}))
        })
    ]
}

#[test]
fn test_fmap_either() {
    run_template(50, gen_fmap_either());
}

// Template 4 (fmap-nested): `pure (fmap (fmap (+{n})) (Just (Just {m} :: Maybe Int) :: Maybe (Maybe Int)))`
fn gen_fmap_nested() -> impl Strategy<Value = (String, serde_json::Value)> {
    (
        arb_int(),
        proptest::option::of(proptest::option::of(arb_int())),
    )
        .prop_map(|(n, maybe_maybe_m): (i64, Option<Option<i64>>)| {
            let src = match maybe_maybe_m {
                Some(Some(m)) => format!(
                    "(fmap (fmap (+({}))) (Just (Just ({}) :: Maybe Int) :: Maybe (Maybe Int)))",
                    n, m
                ),
                Some(None) => format!(
                    "(fmap (fmap (+({}))) (Just (Nothing :: Maybe Int) :: Maybe (Maybe Int)))",
                    n
                ),
                None => format!("(fmap (fmap (+({}))) (Nothing :: Maybe (Maybe Int)))", n),
            };
            let expected = match maybe_maybe_m {
                Some(Some(m)) => json!(m + n),
                Some(None) => json!(null),
                None => json!(null),
            };
            (src, expected)
        })
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
