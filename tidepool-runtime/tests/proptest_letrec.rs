use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use tidepool_codegen::jit_machine::JitEffectMachine;
use tidepool_eval::{env_from_datacon_table, eval, Value, VecHeap};
use tidepool_repr::datacon_table::DataConTable;
use tidepool_repr::frame::CoreFrame;
use tidepool_repr::types::{Alt, AltCon, DataConId, Literal, PrimOpKind, VarId};
use tidepool_repr::{CoreExpr, TreeBuilder};
use tidepool_testing::gen::{arb_core_expr, standard_datacon_table};

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Lit(l1), Value::Lit(l2)) => l1 == l2,
        (Value::Con(tag1, fields1), Value::Con(tag2, fields2)) => {
            tag1 == tag2
                && fields1.len() == fields2.len()
                && fields1
                    .iter()
                    .zip(fields2.iter())
                    .all(|(f1, f2)| values_equal(f1, f2))
        }
        _ => true,
    }
}

fn build_table_for_expr(expr: &CoreExpr) -> DataConTable {
    let mut table = standard_datacon_table();
    let mut seen = std::collections::HashMap::new();
    for node in &expr.nodes {
        match node {
            CoreFrame::Con { tag, fields } => {
                let arity = fields.len() as u32;
                let entry = seen.entry(*tag).or_insert(0);
                if arity > *entry {
                    *entry = arity;
                }
            }
            CoreFrame::Case { alts, .. } => {
                for alt in alts {
                    if let AltCon::DataAlt(tag) = alt.con {
                        let arity = alt.binders.len() as u32;
                        let entry = seen.entry(tag).or_insert(0);
                        if arity > *entry {
                            *entry = arity;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for (id, arity) in seen {
        if table.get(id).is_none() {
            table.insert(tidepool_repr::datacon::DataCon {
                id,
                name: format!("C{}", id.0),
                tag: (id.0 % 100) as u32 + 1,
                rep_arity: arity,
                field_bangs: vec![],
                qualified_name: None,
            });
        }
    }
    table
}

fn check_jit_vs_eval(expr: CoreExpr, nursery_size: usize) -> Result<(), TestCaseError> {
    let table = build_table_for_expr(&expr);
    let mut heap_eval = VecHeap::new();
    let env_eval = env_from_datacon_table(&table);
    let res_eval = eval(&expr, &env_eval, &mut heap_eval);
    let res_jit = match JitEffectMachine::compile(&expr, &table, nursery_size) {
        Ok(mut machine) => machine.run_pure(),
        Err(e) => Err(e),
    };
    if let (Ok(v1), Ok(v2)) = (res_eval, res_jit) {
        prop_assert!(
            values_equal(&v1, &v2),
            "JIT and Eval results differ.
    Eval: {:?}
    JIT:  {:?}
    Expr: {:#?}",
            v1,
            v2,
            expr
        );
    }
    Ok(())
}

#[test]
fn letrec_mutual_recursion() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 50,
                ..Config::default()
            });
            runner
                .run(&(1..10i64), |n| {
                    let mut b = TreeBuilder::new();
                    // f = \x -> case (x ==# 0#) of { 1# -> 42#; DEFAULT -> g (x -# 1#) }
                    // g = \y -> case (y ==# 0#) of { 1# -> 99#; DEFAULT -> f (y -# 1#) }
                    // VarId(100)=f, VarId(101)=g, VarId(102)=x, VarId(103)=y
                    // VarId(200..201) for case binders

                    // f rhs
                    let x_var = b.push(CoreFrame::Var(VarId(102)));
                    let zero = b.push(CoreFrame::Lit(Literal::LitInt(0)));
                    let x_eq_zero = b.push(CoreFrame::PrimOp {
                        op: PrimOpKind::IntEq,
                        args: vec![x_var, zero],
                    });
                    let forty_two = b.push(CoreFrame::Lit(Literal::LitInt(42)));
                    let g_var = b.push(CoreFrame::Var(VarId(101)));
                    let one = b.push(CoreFrame::Lit(Literal::LitInt(1)));
                    let x_minus_one = b.push(CoreFrame::PrimOp {
                        op: PrimOpKind::IntSub,
                        args: vec![x_var, one],
                    });
                    let g_app = b.push(CoreFrame::App {
                        fun: g_var,
                        arg: x_minus_one,
                    });
                    let f_case = b.push(CoreFrame::Case {
                        scrutinee: x_eq_zero,
                        binder: VarId(200),
                        alts: vec![
                            Alt {
                                con: AltCon::LitAlt(Literal::LitInt(1)),
                                binders: vec![],
                                body: forty_two,
                            },
                            Alt {
                                con: AltCon::Default,
                                binders: vec![],
                                body: g_app,
                            },
                        ],
                    });
                    let f_lam = b.push(CoreFrame::Lam {
                        binder: VarId(102),
                        body: f_case,
                    });

                    // g rhs
                    let y_var = b.push(CoreFrame::Var(VarId(103)));
                    let y_eq_zero = b.push(CoreFrame::PrimOp {
                        op: PrimOpKind::IntEq,
                        args: vec![y_var, zero],
                    });
                    let ninety_nine = b.push(CoreFrame::Lit(Literal::LitInt(99)));
                    let f_var_rhs = b.push(CoreFrame::Var(VarId(100)));
                    let y_minus_one = b.push(CoreFrame::PrimOp {
                        op: PrimOpKind::IntSub,
                        args: vec![y_var, one],
                    });
                    let f_app = b.push(CoreFrame::App {
                        fun: f_var_rhs,
                        arg: y_minus_one,
                    });
                    let g_case = b.push(CoreFrame::Case {
                        scrutinee: y_eq_zero,
                        binder: VarId(201),
                        alts: vec![
                            Alt {
                                con: AltCon::LitAlt(Literal::LitInt(1)),
                                binders: vec![],
                                body: ninety_nine,
                            },
                            Alt {
                                con: AltCon::Default,
                                binders: vec![],
                                body: f_app,
                            },
                        ],
                    });
                    let g_lam = b.push(CoreFrame::Lam {
                        binder: VarId(103),
                        body: g_case,
                    });

                    // body: f n
                    let f_var_body = b.push(CoreFrame::Var(VarId(100)));
                    let n_lit = b.push(CoreFrame::Lit(Literal::LitInt(n)));
                    let body = b.push(CoreFrame::App {
                        fun: f_var_body,
                        arg: n_lit,
                    });

                    let _letrec = b.push(CoreFrame::LetRec {
                        bindings: vec![(VarId(100), f_lam), (VarId(101), g_lam)],
                        body,
                    });

                    check_jit_vs_eval(b.build(), 64 * 1024)
                })
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn letrec_lam_captures_con_binder() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 50,
                ..Config::default()
            });
            runner
                .run(
                    &(any::<i64>(), any::<i64>(), any::<i64>()),
                    |(a, b, x_val)| {
                        let mut b_tree = TreeBuilder::new();

                        // pair = Con(DataConId(4), [Lit(a), Lit(b)])
                        let lit_a = b_tree.push(CoreFrame::Lit(Literal::LitInt(a)));
                        let lit_b = b_tree.push(CoreFrame::Lit(Literal::LitInt(b)));
                        let pair_rhs = b_tree.push(CoreFrame::Con {
                            tag: DataConId(4),
                            fields: vec![lit_a, lit_b],
                        });

                        // f = \x -> case pair of { DataAlt(4) [a_bind, b_bind] -> a_bind +# b_bind +# x }
                        let _x_bind_var = b_tree.push(CoreFrame::Var(VarId(10))); // binder for \x
                        let pair_var = b_tree.push(CoreFrame::Var(VarId(11))); // reference to pair

                        let a_bind = b_tree.push(CoreFrame::Var(VarId(12)));
                        let b_bind = b_tree.push(CoreFrame::Var(VarId(13)));
                        let x_ref = b_tree.push(CoreFrame::Var(VarId(10)));

                        let add1 = b_tree.push(CoreFrame::PrimOp {
                            op: PrimOpKind::IntAdd,
                            args: vec![a_bind, b_bind],
                        });
                        let add2 = b_tree.push(CoreFrame::PrimOp {
                            op: PrimOpKind::IntAdd,
                            args: vec![add1, x_ref],
                        });

                        let case_f = b_tree.push(CoreFrame::Case {
                            scrutinee: pair_var,
                            binder: VarId(14),
                            alts: vec![Alt {
                                con: AltCon::DataAlt(DataConId(4)),
                                binders: vec![VarId(12), VarId(13)],
                                body: add2,
                            }],
                        });

                        let f_rhs = b_tree.push(CoreFrame::Lam {
                            binder: VarId(10),
                            body: case_f,
                        });

                        // in f x_val
                        let f_var = b_tree.push(CoreFrame::Var(VarId(15)));
                        let lit_x = b_tree.push(CoreFrame::Lit(Literal::LitInt(x_val)));
                        let body = b_tree.push(CoreFrame::App {
                            fun: f_var,
                            arg: lit_x,
                        });

                        let _letrec = b_tree.push(CoreFrame::LetRec {
                            bindings: vec![(VarId(11), pair_rhs), (VarId(15), f_rhs)],
                            body,
                        });

                        check_jit_vs_eval(b_tree.build(), 64 * 1024)
                    },
                )
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn letrec_multiple_deferred_simple() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 50,
                ..Config::default()
            });
            runner
                .run(&(any::<i64>(), any::<i64>()), |(v1, v2)| {
                    let mut b = TreeBuilder::new();

                    // f = \x -> x
                    let x_bind = b.push(CoreFrame::Var(VarId(1)));
                    let f_rhs = b.push(CoreFrame::Lam {
                        binder: VarId(1),
                        body: x_bind,
                    });

                    // a = f v1
                    let f_ref1 = b.push(CoreFrame::Var(VarId(10)));
                    let lit1 = b.push(CoreFrame::Lit(Literal::LitInt(v1)));
                    let a_rhs = b.push(CoreFrame::App {
                        fun: f_ref1,
                        arg: lit1,
                    });

                    // b = f v2
                    let f_ref2 = b.push(CoreFrame::Var(VarId(10)));
                    let lit2 = b.push(CoreFrame::Lit(Literal::LitInt(v2)));
                    let b_rhs = b.push(CoreFrame::App {
                        fun: f_ref2,
                        arg: lit2,
                    });

                    // c = a +# b
                    let a_ref = b.push(CoreFrame::Var(VarId(11)));
                    let b_ref = b.push(CoreFrame::Var(VarId(12)));
                    let c_rhs = b.push(CoreFrame::PrimOp {
                        op: PrimOpKind::IntAdd,
                        args: vec![a_ref, b_ref],
                    });

                    // in c
                    let c_ref = b.push(CoreFrame::Var(VarId(13)));

                    let _letrec = b.push(CoreFrame::LetRec {
                        bindings: vec![
                            (VarId(10), f_rhs),
                            (VarId(11), a_rhs),
                            (VarId(12), b_rhs),
                            (VarId(13), c_rhs),
                        ],
                        body: c_ref,
                    });

                    check_jit_vs_eval(b.build(), 64 * 1024)
                })
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn letrec_five_bindings() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 50,
                ..Config::default()
            });
            runner
                .run(&(any::<i64>(), any::<i64>()), |(x, y)| {
                    let mut b = TreeBuilder::new();

                    // id = \p -> p
                    let p_bind = b.push(CoreFrame::Var(VarId(1)));
                    let id_rhs = b.push(CoreFrame::Lam {
                        binder: VarId(1),
                        body: p_bind,
                    });

                    // add = \a -> \b -> a +# b
                    let a_bind = b.push(CoreFrame::Var(VarId(2)));
                    let b_bind = b.push(CoreFrame::Var(VarId(3)));
                    let add_inner = b.push(CoreFrame::PrimOp {
                        op: PrimOpKind::IntAdd,
                        args: vec![a_bind, b_bind],
                    });
                    let add_lam_b = b.push(CoreFrame::Lam {
                        binder: VarId(3),
                        body: add_inner,
                    });
                    let add_rhs = b.push(CoreFrame::Lam {
                        binder: VarId(2),
                        body: add_lam_b,
                    });

                    // v1 = id x
                    let id_ref1 = b.push(CoreFrame::Var(VarId(10)));
                    let lit_x = b.push(CoreFrame::Lit(Literal::LitInt(x)));
                    let v1_rhs = b.push(CoreFrame::App {
                        fun: id_ref1,
                        arg: lit_x,
                    });

                    // v2 = id y
                    let id_ref2 = b.push(CoreFrame::Var(VarId(10)));
                    let lit_y = b.push(CoreFrame::Lit(Literal::LitInt(y)));
                    let v2_rhs = b.push(CoreFrame::App {
                        fun: id_ref2,
                        arg: lit_y,
                    });

                    // v3 = add v1 v2
                    let add_ref = b.push(CoreFrame::Var(VarId(11)));
                    let v1_ref = b.push(CoreFrame::Var(VarId(12)));
                    let v2_ref = b.push(CoreFrame::Var(VarId(13)));
                    let add_v1 = b.push(CoreFrame::App {
                        fun: add_ref,
                        arg: v1_ref,
                    });
                    let v3_rhs = b.push(CoreFrame::App {
                        fun: add_v1,
                        arg: v2_ref,
                    });

                    // in v3
                    let v3_ref = b.push(CoreFrame::Var(VarId(14)));

                    let _letrec = b.push(CoreFrame::LetRec {
                        bindings: vec![
                            (VarId(10), id_rhs),
                            (VarId(11), add_rhs),
                            (VarId(12), v1_rhs),
                            (VarId(13), v2_rhs),
                            (VarId(14), v3_rhs),
                        ],
                        body: v3_ref,
                    });

                    check_jit_vs_eval(b.build(), 64 * 1024)
                })
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn letrec_random_wrapped() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut runner = TestRunner::new(Config {
                cases: 50,
                ..Config::default()
            });
            runner
                .run(&arb_core_expr(), |expr| {
                    let mut b = TreeBuilder::new();
                    let expr_nodes_len = expr.nodes.len();

                    // Push the random expr
                    let mut expr_builder = TreeBuilder::new();
                    for node in expr.nodes {
                        expr_builder.push(node);
                    }
                    let expr_off = b.push_tree(expr_builder);
                    let expr_root = expr_off + expr_nodes_len - 1;

                    // v = expr
                    let v_id = VarId(0xFFFF_0000);
                    let v_ref = b.push(CoreFrame::Var(v_id));

                    let _letrec = b.push(CoreFrame::LetRec {
                        bindings: vec![(v_id, expr_root)],
                        body: v_ref,
                    });

                    let wrapped = b.build();
                    check_jit_vs_eval(wrapped.clone(), 64 * 1024)?;
                    check_jit_vs_eval(wrapped, 4 * 1024)
                })
                .unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}
