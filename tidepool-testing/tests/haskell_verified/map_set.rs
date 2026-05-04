//! Verified-generator templates for Data.Map and Data.Set operations:
//! Map.fromList/lookup/insert/union, Set.fromList/member/union (over Int keys
//! and values).
//!
//! Stub: filled in by the `map_set` leaf. Follows the patterns in
//! `fmap.rs` / `text.rs` / `cousins.rs`. ASCII-only, Int-only, pinned types,
//! `ProptestConfig::with_cases(50)` per template.

use crate::{arb_int, run_template_with_imports};
use proptest::prelude::*;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn arb_map_kvs() -> impl Strategy<Value = BTreeMap<i64, i64>> {
    proptest::collection::btree_map(arb_int(), arb_int(), 0..=8)
}

fn arb_set_ks() -> impl Strategy<Value = BTreeSet<i64>> {
    proptest::collection::btree_set(arb_int(), 0..=8)
}

#[test]
fn map_fromlist_lookup() {
    let strat = (arb_int(), arb_map_kvs()).prop_map(|(k, kvs)| {
        let pairs: Vec<_> = kvs
            .iter()
            .map(|(key, val)| format!("({}, {})", key, val))
            .collect();
        let pairs_str = pairs.join(", ");
        let src = format!(
            "Map.lookup ({}) (Map.fromList [{}] :: Map Int Int) :: Maybe Int",
            k, pairs_str
        );
        let expected = match kvs.get(&k) {
            Some(v) => json!(v),
            None => json!(null),
        };
        (src, expected)
    });
    run_template_with_imports(50, strat, &["import qualified Data.Map.Strict as Map"]);
}

#[test]
fn map_insert_lookup() {
    let strat = (arb_int(), arb_int(), arb_map_kvs()).prop_map(|(k, v, kvs)| {
        let pairs: Vec<_> = kvs
            .iter()
            .map(|(key, val)| format!("({}, {})", key, val))
            .collect();
        let pairs_str = pairs.join(", ");
        let src = format!(
            "Map.lookup ({}) (Map.insert ({}) ({}) (Map.fromList [{}] :: Map Int Int)) :: Maybe Int",
            k, k, v, pairs_str
        );
        let expected = json!(v);
        (src, expected)
    });
    run_template_with_imports(50, strat, &["import qualified Data.Map.Strict as Map"]);
}

#[test]
fn map_union() {
    let strat = (arb_int(), arb_map_kvs(), arb_map_kvs()).prop_map(|(k, left, right)| {
        let l_pairs: Vec<_> = left
            .iter()
            .map(|(key, val)| format!("({}, {})", key, val))
            .collect();
        let r_pairs: Vec<_> = right
            .iter()
            .map(|(key, val)| format!("({}, {})", key, val))
            .collect();
        let l_str = l_pairs.join(", ");
        let r_str = r_pairs.join(", ");
        let src = format!(
            "Map.lookup ({}) (Map.union (Map.fromList [{}] :: Map Int Int) (Map.fromList [{}] :: Map Int Int)) :: Maybe Int",
            k, l_str, r_str
        );

        let expected_val = left.get(&k).or_else(|| right.get(&k));
        let expected = match expected_val {
            Some(v) => json!(v),
            None => json!(null),
        };
        (src, expected)
    });
    run_template_with_imports(50, strat, &["import qualified Data.Map.Strict as Map"]);
}

#[test]
fn set_fromlist_member() {
    let strat = (arb_int(), arb_set_ks()).prop_map(|(k, ks)| {
        let elems: Vec<_> = ks.iter().map(|key| format!("{}", key)).collect();
        let elems_str = elems.join(", ");
        let src = format!(
            "Set.member ({}) (Set.fromList [{}] :: Set Int) :: Bool",
            k, elems_str
        );
        let expected = json!(ks.contains(&k));
        (src, expected)
    });
    run_template_with_imports(50, strat, &["import qualified Data.Set as Set"]);
}

#[test]
fn set_union() {
    let strat = (arb_int(), arb_set_ks(), arb_set_ks()).prop_map(|(k, left, right)| {
        let l_elems: Vec<_> = left.iter().map(|key| format!("{}", key)).collect();
        let r_elems: Vec<_> = right.iter().map(|key| format!("{}", key)).collect();
        let l_str = l_elems.join(", ");
        let r_str = r_elems.join(", ");
        let src = format!(
            "Set.member ({}) (Set.union (Set.fromList [{}] :: Set Int) (Set.fromList [{}] :: Set Int)) :: Bool",
            k, l_str, r_str
        );

        let expected = json!(left.contains(&k) || right.contains(&k));
        (src, expected)
    });
    run_template_with_imports(50, strat, &["import qualified Data.Set as Set"]);
}
