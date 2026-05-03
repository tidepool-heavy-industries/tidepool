use proptest::prelude::*;
use tidepool_codegen::effect_machine::EffContKind;
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::value::Value;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{CoreExpr, TreeBuilder};
use tidepool_testing::compare::assert_values_eq;

fn test_table() -> DataConTable {
    let mut table = DataConTable::new();
    // I#, W#, C#, F#, D#
    let boxes = [
        (100, "I#", 1),
        (101, "W#", 2),
        (102, "C#", 3),
        (103, "F#", 4),
        (104, "D#", 5),
    ];
    for (id, name, tag) in boxes {
        table.insert(tidepool_repr::datacon::DataCon {
            id: DataConId(id),
            name: name.to_string(),
            tag,
            rep_arity: 1,
            field_bangs: vec![],
            qualified_name: None,
        });
    }

    for (i, kind) in EffContKind::ALL.iter().enumerate() {
        table.insert(tidepool_repr::datacon::DataCon {
            id: DataConId(1000 + i as u64),
            name: kind.name().to_string(),
            tag: (1000 + i) as u32,
            rep_arity: if matches!(kind, EffContKind::Node | EffContKind::Union) {
                2
            } else {
                1
            },
            field_bangs: vec![],
            qualified_name: None,
        });
    }
    table
}

fn run_pure_fixture(expr: &CoreExpr, table: &DataConTable) -> Value {
    let mut machine = JitEffectMachine::compile(expr, table, 1 << 20).expect("Compilation failed");
    machine.run_pure().expect("Run failed")
}

#[test]
fn differential_flatten_nested_int_boxes() {
    let table = test_table();
    let i_hash_id = table.get_by_name("I#").unwrap();

    // canonical: Con(I#, [Lit(Int, 42)])
    let mut bld_can = TreeBuilder::new();
    let lit_42 = bld_can.push(CoreFrame::Lit(Literal::LitInt(42)));
    bld_can.push(CoreFrame::Con {
        tag: i_hash_id,
        fields: vec![lit_42],
    });
    let canonical = bld_can.build();

    // pre_canonical: Con(I#, [Con(I#, [Lit(Int, 42)])])
    let mut bld_pre = TreeBuilder::new();
    let lit_42_pre = bld_pre.push(CoreFrame::Lit(Literal::LitInt(42)));
    let inner_box = bld_pre.push(CoreFrame::Con {
        tag: i_hash_id,
        fields: vec![lit_42_pre],
    });
    bld_pre.push(CoreFrame::Con {
        tag: i_hash_id,
        fields: vec![inner_box],
    });
    let pre_canonical = bld_pre.build();

    let val_can = run_pure_fixture(&canonical, &table);
    let val_pre = run_pure_fixture(&pre_canonical, &table);

    assert_values_eq(&val_can, &val_pre);
}

#[test]
fn differential_unbox_primop_args() {
    let table = test_table();
    let i_hash_id = table.get_by_name("I#").unwrap();

    // canonical: PrimOp { IntAdd, [Lit(Int, 1), Lit(Int, 2)] }
    let mut bld_can = TreeBuilder::new();
    let lit_1 = bld_can.push(CoreFrame::Lit(Literal::LitInt(1)));
    let lit_2 = bld_can.push(CoreFrame::Lit(Literal::LitInt(2)));
    bld_can.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![lit_1, lit_2],
    });
    let canonical = bld_can.build();

    // pre_canonical: PrimOp { IntAdd, [Con(I#, [Lit(Int, 1)]), Con(I#, [Lit(Int, 2)])] }
    let mut bld_pre = TreeBuilder::new();
    let lit_1_pre = bld_pre.push(CoreFrame::Lit(Literal::LitInt(1)));
    let box_1 = bld_pre.push(CoreFrame::Con {
        tag: i_hash_id,
        fields: vec![lit_1_pre],
    });
    let lit_2_pre = bld_pre.push(CoreFrame::Lit(Literal::LitInt(2)));
    let box_2 = bld_pre.push(CoreFrame::Con {
        tag: i_hash_id,
        fields: vec![lit_2_pre],
    });
    bld_pre.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntAdd,
        args: vec![box_1, box_2],
    });
    let pre_canonical = bld_pre.build();

    let val_can = run_pure_fixture(&canonical, &table);
    let val_pre = run_pure_fixture(&pre_canonical, &table);

    assert_values_eq(&val_can, &val_pre);
    // Sanity check for the specific value
    assert!(
        matches!(val_can, Value::Lit(Literal::LitInt(3))),
        "Expected LitInt(3), got {:?}",
        val_can
    );
}

#[test]
fn differential_canonicalize_effect_tags() {
    let table = test_table();
    let union_id = table.get_by_name("Union").unwrap();
    let w_hash_id = table.get_by_name("W#").unwrap();

    // canonical: Con(Union, [Lit(Word, 7), Var(10)])
    let mut bld_can_run = TreeBuilder::new();
    let lit_100 = bld_can_run.push(CoreFrame::Lit(Literal::LitInt(100)));
    let lit_7_r = bld_can_run.push(CoreFrame::Lit(Literal::LitWord(7)));
    let var_10_r = bld_can_run.push(CoreFrame::Var(VarId(10)));
    let union_can = bld_can_run.push(CoreFrame::Con {
        tag: union_id,
        fields: vec![lit_7_r, var_10_r],
    });
    bld_can_run.push(CoreFrame::LetNonRec {
        binder: VarId(10),
        rhs: lit_100,
        body: union_can,
    });
    let canonical_run = bld_can_run.build();

    // pre_canonical: Con(Union, [Con(W#, [Lit(Word, 7)]), Var(10)])
    let mut bld_pre_run = TreeBuilder::new();
    let lit_100_pre = bld_pre_run.push(CoreFrame::Lit(Literal::LitInt(100)));
    let lit_7_p = bld_pre_run.push(CoreFrame::Lit(Literal::LitWord(7)));
    let box_7_p = bld_pre_run.push(CoreFrame::Con {
        tag: w_hash_id,
        fields: vec![lit_7_p],
    });
    let var_10_p = bld_pre_run.push(CoreFrame::Var(VarId(10)));
    let union_pre = bld_pre_run.push(CoreFrame::Con {
        tag: union_id,
        fields: vec![box_7_p, var_10_p],
    });
    bld_pre_run.push(CoreFrame::LetNonRec {
        binder: VarId(10),
        rhs: lit_100_pre,
        body: union_pre,
    });
    let pre_canonical_run = bld_pre_run.build();

    let val_can = run_pure_fixture(&canonical_run, &table);
    let val_pre = run_pure_fixture(&pre_canonical_run, &table);

    assert_values_eq(&val_can, &val_pre);
}

#[test]
fn differential_normalize_idempotent_at_runtime() {
    let table = test_table();
    let i_hash_id = table.get_by_name("I#").unwrap();

    // Con(I#, [Lit(Int, 42)])
    let mut bld = TreeBuilder::new();
    let lit_42 = bld.push(CoreFrame::Lit(Literal::LitInt(42)));
    bld.push(CoreFrame::Con {
        tag: i_hash_id,
        fields: vec![lit_42],
    });
    let expr = bld.build();

    let norm1 = tidepool_repr::normalize(&expr, &table);
    let norm2 = tidepool_repr::normalize(&norm1, &table);

    // Assert IR-level idempotence (addresses Copilot comment)
    assert_eq!(norm1, norm2, "normalize must be idempotent at the IR level");

    let val1 = run_pure_fixture(&norm1, &table);
    let val2 = run_pure_fixture(&norm2, &table);

    assert_values_eq(&val1, &val2);
}

#[test]
fn differential_var_set_preserved() {
    let table = test_table();
    let i_hash_id = table.get_by_name("I#").unwrap();

    // Con(I#, [Con(I#, [Var(1)])]) -> Con(I#, [Var(1)])

    let mut bld_pre2 = TreeBuilder::new();
    let lit_5_2 = bld_pre2.push(CoreFrame::Lit(Literal::LitInt(5)));
    let var_1_2 = bld_pre2.push(CoreFrame::Var(VarId(1)));
    let inner_box = bld_pre2.push(CoreFrame::Con {
        tag: i_hash_id,
        fields: vec![var_1_2],
    });
    let outer_box = bld_pre2.push(CoreFrame::Con {
        tag: i_hash_id,
        fields: vec![inner_box],
    });
    bld_pre2.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: lit_5_2,
        body: outer_box,
    });
    let pre_canonical2 = bld_pre2.build();

    let mut bld_can2 = TreeBuilder::new();
    let lit_5_c = bld_can2.push(CoreFrame::Lit(Literal::LitInt(5)));
    let var_1_c = bld_can2.push(CoreFrame::Var(VarId(1)));
    let box_c = bld_can2.push(CoreFrame::Con {
        tag: i_hash_id,
        fields: vec![var_1_c],
    });
    bld_can2.push(CoreFrame::LetNonRec {
        binder: VarId(1),
        rhs: lit_5_c,
        body: box_c,
    });
    let canonical2 = bld_can2.build();

    let val_pre = run_pure_fixture(&pre_canonical2, &table);
    let val_can = run_pure_fixture(&canonical2, &table);

    assert_values_eq(&val_pre, &val_can);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]
    #[test]
    fn prop_differential_boxing(n in any::<i64>(), layers in 0..5usize) {
        let table = test_table();
        let i_hash_id = table.get_by_name("I#").unwrap();

        // Build canonical: Con(I#, [Lit(n)])
        let mut bld_can = TreeBuilder::new();
        let lit_n = bld_can.push(CoreFrame::Lit(Literal::LitInt(n)));
        bld_can.push(CoreFrame::Con {
            tag: i_hash_id,
            fields: vec![lit_n],
        });
        let canonical = bld_can.build();

        // Build pre-canonical: Con(I#, [Con(I#, ... [Lit(n)])])
        let mut bld_pre = TreeBuilder::new();
        let mut current = bld_pre.push(CoreFrame::Lit(Literal::LitInt(n)));
        for _ in 0..=layers {
            current = bld_pre.push(CoreFrame::Con {
                tag: i_hash_id,
                fields: vec![current],
            });
        }
        let pre_canonical = bld_pre.build();

        let val_can = run_pure_fixture(&canonical, &table);
        let val_pre = run_pure_fixture(&pre_canonical, &table);

        assert_values_eq(&val_can, &val_pre);
    }
}
