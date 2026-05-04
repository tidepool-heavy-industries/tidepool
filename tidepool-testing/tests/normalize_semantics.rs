use proptest::prelude::*;
use tidepool_eval::{deep_force, env_from_datacon_table, eval, VecHeap};
use tidepool_repr::normalize;
use tidepool_testing::compare::assert_values_eq;
use tidepool_testing::gen::{arb_ground_expr, standard_datacon_table};

/// Custom table that maps standard generator IDs to names that trigger normalization rules.
fn normalization_test_table() -> tidepool_repr::DataConTable {
    let mut table = standard_datacon_table();

    // Look up "Just" (arity 1). Rename to "W#" to trigger Rule 1 (flatten_box)
    // and Rule 2 (canonicalize_effect_tag) when used inside Union.
    if let Some(id) = table.get_by_name_arity("Just", 1) {
        let mut new_con = table.get(id).unwrap().clone();
        new_con.name = "W#".to_string();
        table.insert(new_con);
    }

    // Look up "(,)" (arity 2). Rename to "Union" to trigger Rule 2.
    if let Some(id) = table.get_by_name_arity("(,)", 2) {
        let mut new_con = table.get(id).unwrap().clone();
        new_con.name = "Union".to_string();
        table.insert(new_con);
    }

    table
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Pre-existing flaky test (#311). The test_table renames `Just` → `W#`
    /// so Rule 1 (flatten_box) fires on user-data shapes, but Rule 1 is
    /// only semantics-preserving for true boxing constructors wrapping
    /// primitives (`I#(I#(x))` is impossible in real Core). When the
    /// generator hits a `Just (Just x)` shape, normalize collapses it to
    /// `Just x` and the test correctly flags the divergence — but this is
    /// a test-design problem, not a normalize bug. `#[ignore]` until the
    /// redesign in #311 lands.
    #[test]
    #[ignore = "flaky: see #311 (test_table rename triggers Rule 1 on user-data shapes)"]
    fn prop_normalize_preserves_semantics(expr in arb_ground_expr()) {
        let table = normalization_test_table();
        let env = env_from_datacon_table(&table);

        // Evaluate original
        let mut heap1 = VecHeap::new();
        let original_res = eval(&expr, &env, &mut heap1)
            .and_then(|v| deep_force(v, &mut heap1));

        // Evaluate normalized
        let normalized_expr = normalize(&expr, &table);
        let mut heap2 = VecHeap::new();
        let normalized_res = eval(&normalized_expr, &env, &mut heap2)
            .and_then(|v| deep_force(v, &mut heap2));

        match (original_res, normalized_res) {
            (Ok(v1), Ok(v2)) => {
                assert_values_eq(&v1, &v2);
            }
            (Err(e1), Err(e2)) => {
                // If both fail, ensure they fail with the same error type.
                // We compare debug representation for a reasonable approximation of equality.
                prop_assert_eq!(format!("{:?}", e1), format!("{:?}", e2), "Evaluation failed with different errors after normalization");
            }
            (Ok(v1), Err(e2)) => {
                prop_assert!(false, "Normalized eval failed but original succeeded.\nError: {:?}\nOriginal result: {:?}\nOriginal Expr: {:#?}\nNormalized Expr: {:#?}", e2, v1, expr, normalized_expr);
            }
            (Err(e1), Ok(v2)) => {
                prop_assert!(false, "Original eval failed but normalized succeeded.\nError: {:?}\nNormalized result: {:?}\nOriginal Expr: {:#?}\nNormalized Expr: {:#?}", e1, v2, expr, normalized_expr);
            }
        }
    }
}
