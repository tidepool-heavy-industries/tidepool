use proptest::prelude::*;
use tidepool_eval::{deep_force, env_from_datacon_table, eval, VecHeap};
use tidepool_repr::normalize;
use tidepool_testing::compare::assert_values_eq;
use tidepool_testing::gen::{arb_ground_expr, standard_datacon_table};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Normalize is identity on the natural shapes produced by `arb_ground_expr`.
    ///
    /// The generator only uses standard DataCons (`Just`/`Nothing`/`(,)`/`[]`/`:`/`True`/`False`
    /// and boxing constructors `I#`/`W#`/`D#`/`F#`/`C#`). Critically, `gen_con` only
    /// wraps with `Just`/`Nothing`/`(,)`/Bool — it never produces `Con(I#, [Lit])`
    /// shapes. So none of the normalization rules can fire on generator output:
    ///
    /// - Rule 1 (flatten_box) needs `Con(BOX, [Con(BOX, [..])])`.
    /// - Rule 2 (canonicalize_effect_tag) needs a `Union` DataCon (absent here).
    /// - Rule 3 (unbox_prim_args) needs `PrimOp` with `Con(BOX, [Lit])` args,
    ///   but PrimOp args of primitive type fall through `gen_con` → `gen_leaf`
    ///   and produce raw `Lit`.
    ///
    /// We therefore assert both structural identity (`normalize(expr) == expr`)
    /// and semantic identity. The hand-built tests in `tidepool-repr::normalize`
    /// cover the actual rule transformations on synthesized shapes.
    ///
    /// History: the prior version of this test renamed `Just` → `W#` to force
    /// Rule 1 to fire on generated `Maybe (Maybe a)` shapes. That made it
    /// flaky: Rule 1 only preserves semantics for true boxing wrappers around
    /// primitives (a precondition GHC enforces but the rename violated), so
    /// when the generator hit a `Just (Just x)` case the values diverged. #311.
    #[test]
    fn prop_normalize_identity_on_user_data(expr in arb_ground_expr()) {
        let table = standard_datacon_table();
        let env = env_from_datacon_table(&table);

        let normalized_expr = normalize(&expr, &table);
        prop_assert_eq!(
            &normalized_expr, &expr,
            "normalize should be identity on user-data shapes from arb_ground_expr"
        );

        let mut heap1 = VecHeap::new();
        let original_res = eval(&expr, &env, &mut heap1).and_then(|v| deep_force(v, &mut heap1));

        let mut heap2 = VecHeap::new();
        let normalized_res =
            eval(&normalized_expr, &env, &mut heap2).and_then(|v| deep_force(v, &mut heap2));

        match (original_res, normalized_res) {
            (Ok(v1), Ok(v2)) => assert_values_eq(&v1, &v2),
            (Err(e1), Err(e2)) => prop_assert_eq!(
                format!("{:?}", e1),
                format!("{:?}", e2),
                "evaluation failed with different errors after normalization"
            ),
            (Ok(v1), Err(e2)) => prop_assert!(
                false,
                "normalized eval failed but original succeeded.\nError: {:?}\nOriginal result: {:?}\nExpr: {:#?}",
                e2, v1, expr
            ),
            (Err(e1), Ok(v2)) => prop_assert!(
                false,
                "original eval failed but normalized succeeded.\nError: {:?}\nNormalized result: {:?}\nExpr: {:#?}",
                e1, v2, expr
            ),
        }
    }
}
