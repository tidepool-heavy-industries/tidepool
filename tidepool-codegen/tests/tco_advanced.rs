use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::value::Value;
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::*;
use tidepool_repr::{Literal, TreeBuilder};

fn assert_lit_int(val: &Value, expected: i64) {
    let Value::Lit(Literal::LitInt(n)) = val else {
        panic!("expected Lit(Int({})), got {:?}", expected, val);
    };
    assert_eq!(*n, expected);
}

fn empty_table() -> DataConTable {
    let mut table = DataConTable::new();
    // Add required freer-simple tags for JitEffectMachine::compile
    use tidepool_codegen::effect_machine::EffContKind;
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

#[test]
fn test_mutual_tco_even() {
    let is_even = VarId(1);
    let is_odd = VarId(2);
    let n = VarId(3);
    let binder = VarId(4);

    let mut bld = TreeBuilder::new();

    // isEven body: case n == 0 of { 1 -> 1; _ -> isOdd(n-1) }
    let vn1 = bld.push(CoreFrame::Var(n));
    let lit0 = bld.push(CoreFrame::Lit(Literal::LitInt(0)));
    let cmp1 = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntEq,
        args: vec![vn1, lit0],
    });
    let lit1 = bld.push(CoreFrame::Lit(Literal::LitInt(1)));
    let vn2 = bld.push(CoreFrame::Var(n));
    let sub1 = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntSub,
        args: vec![vn2, lit1],
    });
    let v_is_odd = bld.push(CoreFrame::Var(is_odd));
    let call_odd = bld.push(CoreFrame::App {
        fun: v_is_odd,
        arg: sub1,
    });
    let case_even = bld.push(CoreFrame::Case {
        scrutinee: cmp1,
        binder,
        alts: vec![
            Alt {
                con: AltCon::LitAlt(Literal::LitInt(1)),
                binders: vec![],
                body: lit1,
            },
            Alt {
                con: AltCon::Default,
                binders: vec![],
                body: call_odd,
            },
        ],
    });
    let lam_even = bld.push(CoreFrame::Lam {
        binder: n,
        body: case_even,
    });

    // isOdd body: case n == 0 of { 1 -> 0; _ -> isEven(n-1) }
    let vn3 = bld.push(CoreFrame::Var(n));
    let lit0_2 = bld.push(CoreFrame::Lit(Literal::LitInt(0)));
    let cmp2 = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntEq,
        args: vec![vn3, lit0_2],
    });
    let lit0_res = bld.push(CoreFrame::Lit(Literal::LitInt(0)));
    let vn4 = bld.push(CoreFrame::Var(n));
    let sub2 = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntSub,
        args: vec![vn4, lit1],
    });
    let v_is_even = bld.push(CoreFrame::Var(is_even));
    let call_even = bld.push(CoreFrame::App {
        fun: v_is_even,
        arg: sub2,
    });
    let case_odd = bld.push(CoreFrame::Case {
        scrutinee: cmp2,
        binder,
        alts: vec![
            Alt {
                con: AltCon::LitAlt(Literal::LitInt(1)),
                binders: vec![],
                body: lit0_res,
            },
            Alt {
                con: AltCon::Default,
                binders: vec![],
                body: call_even,
            },
        ],
    });
    let lam_odd = bld.push(CoreFrame::Lam {
        binder: n,
        body: case_odd,
    });

    // isEven(100)
    let lit100 = bld.push(CoreFrame::Lit(Literal::LitInt(100)));
    let v_is_even_main = bld.push(CoreFrame::Var(is_even));
    let app = bld.push(CoreFrame::App {
        fun: v_is_even_main,
        arg: lit100,
    });

    bld.push(CoreFrame::LetRec {
        bindings: vec![(is_even, lam_even), (is_odd, lam_odd)],
        body: app,
    });

    let expr = bld.build();
    let table = empty_table();
    let mut machine = JitEffectMachine::compile(&expr, &table, 1 << 20).unwrap();
    let result = machine.run_pure().unwrap();
    assert_lit_int(&result, 1); // 100 is even
}

#[test]
fn test_mutual_tco_odd() {
    let is_even = VarId(1);
    let is_odd = VarId(2);
    let n = VarId(3);
    let binder = VarId(4);

    let mut bld = TreeBuilder::new();

    // isEven body
    let vn1 = bld.push(CoreFrame::Var(n));
    let lit0 = bld.push(CoreFrame::Lit(Literal::LitInt(0)));
    let cmp1 = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntEq,
        args: vec![vn1, lit0],
    });
    let lit1 = bld.push(CoreFrame::Lit(Literal::LitInt(1)));
    let vn2 = bld.push(CoreFrame::Var(n));
    let sub1 = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntSub,
        args: vec![vn2, lit1],
    });
    let v_is_odd = bld.push(CoreFrame::Var(is_odd));
    let call_odd = bld.push(CoreFrame::App {
        fun: v_is_odd,
        arg: sub1,
    });
    let case_even = bld.push(CoreFrame::Case {
        scrutinee: cmp1,
        binder,
        alts: vec![
            Alt {
                con: AltCon::LitAlt(Literal::LitInt(1)),
                binders: vec![],
                body: lit1,
            },
            Alt {
                con: AltCon::Default,
                binders: vec![],
                body: call_odd,
            },
        ],
    });
    let lam_even = bld.push(CoreFrame::Lam {
        binder: n,
        body: case_even,
    });

    // isOdd body
    let vn3 = bld.push(CoreFrame::Var(n));
    let lit0_2 = bld.push(CoreFrame::Lit(Literal::LitInt(0)));
    let cmp2 = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntEq,
        args: vec![vn3, lit0_2],
    });
    let lit0_res = bld.push(CoreFrame::Lit(Literal::LitInt(0)));
    let vn4 = bld.push(CoreFrame::Var(n));
    let sub2 = bld.push(CoreFrame::PrimOp {
        op: PrimOpKind::IntSub,
        args: vec![vn4, lit1],
    });
    let v_is_even = bld.push(CoreFrame::Var(is_even));
    let call_even = bld.push(CoreFrame::App {
        fun: v_is_even,
        arg: sub2,
    });
    let case_odd = bld.push(CoreFrame::Case {
        scrutinee: cmp2,
        binder,
        alts: vec![
            Alt {
                con: AltCon::LitAlt(Literal::LitInt(1)),
                binders: vec![],
                body: lit0_res,
            },
            Alt {
                con: AltCon::Default,
                binders: vec![],
                body: call_even,
            },
        ],
    });
    let lam_odd = bld.push(CoreFrame::Lam {
        binder: n,
        body: case_odd,
    });

    // isEven(101)
    let lit101 = bld.push(CoreFrame::Lit(Literal::LitInt(101)));
    let v_is_even_main = bld.push(CoreFrame::Var(is_even));
    let app = bld.push(CoreFrame::App {
        fun: v_is_even_main,
        arg: lit101,
    });

    bld.push(CoreFrame::LetRec {
        bindings: vec![(is_even, lam_even), (is_odd, lam_odd)],
        body: app,
    });

    let expr = bld.build();
    let table = empty_table();
    let mut machine = JitEffectMachine::compile(&expr, &table, 1 << 20).unwrap();
    let result = machine.run_pure().unwrap();
    assert_lit_int(&result, 0); // 101 is NOT even
}
