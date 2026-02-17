use core_repr::{CoreFrame, RecursiveTree};
use core_eval::{eval::eval, env::Env, value::Value, heap::VecHeap, error::EvalError};
use std::path::PathBuf;
use std::error::Error;

/// Helper function to evaluate a CoreExpr tree.
pub fn eval_expr(nodes: Vec<CoreFrame<usize>>) -> Result<Value, EvalError> {
    let expr = RecursiveTree { nodes };
    let mut heap = VecHeap::new();
    eval(&expr, &Env::new(), &mut heap)
}

/// Helper function to evaluate a CBOR file.
pub fn eval_cbor(path: &str) -> Result<Value, Box<dyn Error>> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(path);
    let bytes = std::fs::read(&p)?;
    let expr = core_repr::serial::read::read_cbor(&bytes)?;
    let mut heap = VecHeap::new();
    eval(&expr, &Env::new(), &mut heap).map_err(|e| e.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_repr::{Literal, VarId, DataConId, JoinId, PrimOpKind, Alt, AltCon};

    // --- 1. Arithmetic ---

    #[test]
    fn test_arith_add() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(10)),
            CoreFrame::Lit(Literal::LitInt(20)),
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(30))));
    }

    #[test]
    fn test_arith_sub() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(50)),
            CoreFrame::Lit(Literal::LitInt(15)),
            CoreFrame::PrimOp { op: PrimOpKind::IntSub, args: vec![0, 1] },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(35))));
    }

    #[test]
    fn test_arith_mul() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(6)),
            CoreFrame::Lit(Literal::LitInt(7)),
            CoreFrame::PrimOp { op: PrimOpKind::IntMul, args: vec![0, 1] },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(42))));
    }

    #[test]
    fn test_arith_negate() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::PrimOp { op: PrimOpKind::IntNegate, args: vec![0] },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(-42))));
    }

    #[test]
    fn test_arith_nested() {
        // (10 + 20) * (50 - 15) = 30 * 35 = 1050
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::Lit(Literal::LitInt(20)), // 1
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] }, // 2: 30
            CoreFrame::Lit(Literal::LitInt(50)), // 3
            CoreFrame::Lit(Literal::LitInt(15)), // 4
            CoreFrame::PrimOp { op: PrimOpKind::IntSub, args: vec![3, 4] }, // 5: 35
            CoreFrame::PrimOp { op: PrimOpKind::IntMul, args: vec![2, 5] }, // 6: 1050
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(1050))));
    }

    #[test]
    fn test_arith_overflow() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(i64::MAX)),
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(i64::MIN))));
    }

    // --- 2. Comparisons ---

    #[test]
    fn test_cmp_eq() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::PrimOp { op: PrimOpKind::IntEq, args: vec![0, 1] },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(1))));
    }

    #[test]
    fn test_cmp_lt() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(10)),
            CoreFrame::Lit(Literal::LitInt(20)),
            CoreFrame::PrimOp { op: PrimOpKind::IntLt, args: vec![0, 1] },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(1))));
    }

    #[test]
    fn test_cmp_ge() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(20)),
            CoreFrame::Lit(Literal::LitInt(10)),
            CoreFrame::PrimOp { op: PrimOpKind::IntGe, args: vec![0, 1] },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(1))));
    }

    #[test]
    fn test_cmp_mixed_char() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitChar('a')),
            CoreFrame::Lit(Literal::LitChar('b')),
            CoreFrame::PrimOp { op: PrimOpKind::CharLt, args: vec![0, 1] },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(1))));
    }

    #[test]
    fn test_cmp_double_eq() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitDouble(1.5f64.to_bits())),
            CoreFrame::Lit(Literal::LitDouble(1.5f64.to_bits())),
            CoreFrame::PrimOp { op: PrimOpKind::DoubleEq, args: vec![0, 1] },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(1))));
    }

    // --- 3. Lambda/App ---

    #[test]
    fn test_lam_identity() {
        let nodes = vec![
            CoreFrame::Var(VarId(1)),
            CoreFrame::Lam { binder: VarId(1), body: 0 }, // lambda x. x
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::App { fun: 1, arg: 2 }, // (lambda x. x) 42
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(42))));
    }

    #[test]
    fn test_lam_const() {
        let nodes = vec![
            CoreFrame::Var(VarId(1)), // 0: x
            CoreFrame::Var(VarId(2)), // 1: y
            CoreFrame::Lam { binder: VarId(1), body: 1 }, // 2: lambda x. y
            CoreFrame::Lam { binder: VarId(2), body: 2 }, // 3: lambda y. lambda x. y
            CoreFrame::Lit(Literal::LitInt(42)), // 4
            CoreFrame::App { fun: 3, arg: 4 }, // 5: (lambda y. lambda x. y) 42
            CoreFrame::Lit(Literal::LitInt(99)), // 6
            CoreFrame::App { fun: 5, arg: 6 }, // 7: ((lambda y. lambda x. y) 42) 99
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(42))));
    }

    #[test]
    fn test_lam_compose() {
        // compose = lambda f. lambda g. lambda x. f (g x)
        let nodes = vec![
            CoreFrame::Var(VarId(3)), // 0: x
            CoreFrame::Var(VarId(2)), // 1: g
            CoreFrame::App { fun: 1, arg: 0 }, // 2: g x
            CoreFrame::Var(VarId(1)), // 3: f
            CoreFrame::App { fun: 3, arg: 2 }, // 4: f (g x)
            CoreFrame::Lam { binder: VarId(3), body: 4 }, // 5: lambda x. f (g x)
            CoreFrame::Lam { binder: VarId(2), body: 5 }, // 6: lambda g. lambda x. f (g x)
            CoreFrame::Lam { binder: VarId(1), body: 6 }, // 7: lambda f. lambda g. lambda x. f (g x)
            
            // f = lambda n. n + 1
            CoreFrame::Var(VarId(10)), // 8: n
            CoreFrame::Lit(Literal::LitInt(1)), // 9
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![8, 9] }, // 10: n + 1
            CoreFrame::Lam { binder: VarId(10), body: 10 }, // 11: f
            
            // g = lambda m. m * 2
            CoreFrame::Var(VarId(11)), // 12: m
            CoreFrame::Lit(Literal::LitInt(2)), // 13
            CoreFrame::PrimOp { op: PrimOpKind::IntMul, args: vec![12, 13] }, // 14: m * 2
            CoreFrame::Lam { binder: VarId(11), body: 14 }, // 15: g
            
            CoreFrame::App { fun: 7, arg: 11 }, // 16: compose f
            CoreFrame::App { fun: 16, arg: 15 }, // 17: compose f g
            CoreFrame::Lit(Literal::LitInt(10)), // 18
            CoreFrame::App { fun: 17, arg: 18 }, // 19: (compose f g) 10 = (10 * 2) + 1 = 21
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(21))));
    }

    #[test]
    fn test_lam_currying() {
        // add = lambda x. lambda y. x + y
        let nodes = vec![
            CoreFrame::Var(VarId(1)), // 0: x
            CoreFrame::Var(VarId(2)), // 1: y
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] }, // 2: x + y
            CoreFrame::Lam { binder: VarId(2), body: 2 }, // 3: lambda y. x + y
            CoreFrame::Lam { binder: VarId(1), body: 3 }, // 4: lambda x. lambda y. x + y
            CoreFrame::Lit(Literal::LitInt(10)), // 5
            CoreFrame::App { fun: 4, arg: 5 }, // 6: (lambda x. lambda y. x + y) 10
            CoreFrame::Lit(Literal::LitInt(20)), // 7
            CoreFrame::App { fun: 6, arg: 7 }, // 8: ((lambda x. lambda y. x + y) 10) 20
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(30))));
    }

    // --- 4. Let bindings ---

    #[test]
    fn test_let_nonrec_simple() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::Var(VarId(1)),
            CoreFrame::LetNonRec { binder: VarId(1), rhs: 0, body: 1 },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(42))));
    }

    #[test]
    fn test_let_nonrec_nested() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::Lit(Literal::LitInt(20)), // 1
            CoreFrame::Var(VarId(1)), // 2
            CoreFrame::Var(VarId(2)), // 3
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![2, 3] }, // 4: x + y
            CoreFrame::LetNonRec { binder: VarId(2), rhs: 1, body: 4 }, // 5: let y = 20 in x + y
            CoreFrame::LetNonRec { binder: VarId(1), rhs: 0, body: 5 }, // 6: let x = 10 in let y = 20 in x + y
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(30))));
    }

    #[test]
    fn test_letrec_mutual() {
        // letrec f = lambda n. n + 1; g = lambda m. f m in g 10
        let nodes = vec![
            CoreFrame::Var(VarId(11)), // 0: n
            CoreFrame::Lit(Literal::LitInt(1)), // 1
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] }, // 2: n + 1
            CoreFrame::Lam { binder: VarId(11), body: 2 }, // 3: f = lambda n. n + 1
            
            CoreFrame::Var(VarId(12)), // 4: m
            CoreFrame::Var(VarId(1)), // 5: f
            CoreFrame::App { fun: 5, arg: 4 }, // 6: f m
            CoreFrame::Lam { binder: VarId(12), body: 6 }, // 7: g = lambda m. f m
            
            CoreFrame::Var(VarId(2)), // 8: g
            CoreFrame::Lit(Literal::LitInt(10)), // 9
            CoreFrame::App { fun: 8, arg: 9 }, // 10: g 10
            
            CoreFrame::LetRec {
                bindings: vec![(VarId(1), 3), (VarId(2), 7)],
                body: 10,
            }, // 11
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(11))));
    }

    #[test]
    fn test_letrec_factorial() {
        // letrec fact = lambda n. if n == 0 then 1 else n * fact (n - 1) in fact 5
        let nodes = vec![
            CoreFrame::Var(VarId(10)), // 0: n
            CoreFrame::Lit(Literal::LitInt(0)), // 1
            CoreFrame::PrimOp { op: PrimOpKind::IntEq, args: vec![0, 1] }, // 2: n == 0
            
            CoreFrame::Lit(Literal::LitInt(1)), // 3
            
            CoreFrame::Var(VarId(10)), // 4: n
            CoreFrame::Var(VarId(1)), // 5: fact
            CoreFrame::Var(VarId(10)), // 6: n
            CoreFrame::Lit(Literal::LitInt(1)), // 7
            CoreFrame::PrimOp { op: PrimOpKind::IntSub, args: vec![6, 7] }, // 8: n - 1
            CoreFrame::App { fun: 5, arg: 8 }, // 9: fact (n - 1)
            CoreFrame::PrimOp { op: PrimOpKind::IntMul, args: vec![4, 9] }, // 10: n * fact (n - 1)
            
            CoreFrame::Case {
                scrutinee: 2,
                binder: VarId(11),
                alts: vec![
                    Alt { con: AltCon::LitAlt(Literal::LitInt(1)), binders: vec![], body: 3 },
                    Alt { con: AltCon::Default, binders: vec![], body: 10 },
                ],
            }, // 11: if n == 0 then 1 else ...
            
            CoreFrame::Lam { binder: VarId(10), body: 11 }, // 12: lambda n. ...
            
            CoreFrame::Var(VarId(1)), // 13: fact
            CoreFrame::Lit(Literal::LitInt(5)), // 14
            CoreFrame::App { fun: 13, arg: 14 }, // 15: fact 5
            
            CoreFrame::LetRec {
                bindings: vec![(VarId(1), 12)],
                body: 15,
            }, // 16
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(120))));
    }

    // --- 5. Case/Pattern matching ---

    #[test]
    fn test_case_lit_alt() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Lit(Literal::LitInt(100)), // 1
            CoreFrame::Lit(Literal::LitInt(200)), // 2
            CoreFrame::Case {
                scrutinee: 0,
                binder: VarId(10),
                alts: vec![
                    Alt { con: AltCon::LitAlt(Literal::LitInt(1)), binders: vec![], body: 1 },
                    Alt { con: AltCon::Default, binders: vec![], body: 2 },
                ],
            },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(100))));
    }

    #[test]
    fn test_case_default() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(3)), // 0
            CoreFrame::Lit(Literal::LitInt(100)), // 1
            CoreFrame::Lit(Literal::LitInt(200)), // 2
            CoreFrame::Case {
                scrutinee: 0,
                binder: VarId(10),
                alts: vec![
                    Alt { con: AltCon::LitAlt(Literal::LitInt(1)), binders: vec![], body: 1 },
                    Alt { con: AltCon::Default, binders: vec![], body: 2 },
                ],
            },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(200))));
    }

    #[test]
    fn test_case_data_alt() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Con { tag: DataConId(1), fields: vec![0] }, // 1: Just 42
            CoreFrame::Var(VarId(20)), // 2: x
            CoreFrame::Case {
                scrutinee: 1,
                binder: VarId(10),
                alts: vec![
                    Alt {
                        con: AltCon::DataAlt(DataConId(1)),
                        binders: vec![VarId(20)],
                        body: 2,
                    },
                ],
            },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(42))));
    }

    #[test]
    fn test_case_nested() {
        // case Just (Just 42) of Just x -> case x of Just y -> y
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Con { tag: DataConId(1), fields: vec![0] }, // 1: Just 42
            CoreFrame::Con { tag: DataConId(1), fields: vec![1] }, // 2: Just (Just 42)
            
            CoreFrame::Var(VarId(21)), // 3: y
            CoreFrame::Var(VarId(20)), // 4: x
            CoreFrame::Case {
                scrutinee: 4,
                binder: VarId(11),
                alts: vec![
                    Alt {
                        con: AltCon::DataAlt(DataConId(1)),
                        binders: vec![VarId(21)],
                        body: 3,
                    },
                ],
            }, // 5: case x of Just y -> y
            
            CoreFrame::Case {
                scrutinee: 2,
                binder: VarId(10),
                alts: vec![
                    Alt {
                        con: AltCon::DataAlt(DataConId(1)),
                        binders: vec![VarId(20)],
                        body: 5,
                    },
                ],
            }, // 6
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(42))));
    }

    #[test]
    fn test_case_binder_usage() {
        // case 42 of x -> x + 1
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Var(VarId(10)), // 1: x
            CoreFrame::Lit(Literal::LitInt(1)), // 2
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![1, 2] }, // 3: x + 1
            CoreFrame::Case {
                scrutinee: 0,
                binder: VarId(10),
                alts: vec![
                    Alt { con: AltCon::Default, binders: vec![], body: 3 },
                ],
            },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(43))));
    }

    // --- 6. Con ---

    #[test]
    fn test_con_nullary() {
        let nodes = vec![
            CoreFrame::Con { tag: DataConId(100), fields: vec![] },
        ];
        let res = eval_expr(nodes).unwrap();
        if let Value::Con(tag, fields) = res {
            assert_eq!(tag.0, 100);
            assert_eq!(fields.len(), 0);
        } else {
            panic!("Expected Con");
        }
    }

    #[test]
    fn test_con_multi_field() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::Lit(Literal::LitInt(3)),
            CoreFrame::Con { tag: DataConId(1), fields: vec![0, 1, 2] },
        ];
        let res = eval_expr(nodes).unwrap();
        if let Value::Con(tag, fields) = res {
            assert_eq!(tag.0, 1);
            assert_eq!(fields.len(), 3);
            assert!(matches!(fields[0], Value::Lit(Literal::LitInt(1))));
            assert!(matches!(fields[1], Value::Lit(Literal::LitInt(2))));
            assert!(matches!(fields[2], Value::Lit(Literal::LitInt(3))));
        } else {
            panic!("Expected Con");
        }
    }

    #[test]
    fn test_con_nested() {
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)),
            CoreFrame::Con { tag: DataConId(1), fields: vec![0] },
            CoreFrame::Con { tag: DataConId(2), fields: vec![1] },
        ];
        let res = eval_expr(nodes).unwrap();
        if let Value::Con(tag2, fields2) = res {
            assert_eq!(tag2.0, 2);
            if let Value::Con(tag1, fields1) = &fields2[0] {
                assert_eq!(tag1.0, 1);
                assert!(matches!(fields1[0], Value::Lit(Literal::LitInt(42))));
            } else {
                panic!("Expected nested Con");
            }
        } else {
            panic!("Expected Con");
        }
    }

    // --- 7. Join/Jump ---

    #[test]
    fn test_join_simple() {
        let nodes = vec![
            CoreFrame::Var(VarId(10)), // 0: x
            CoreFrame::Lit(Literal::LitInt(42)), // 1
            CoreFrame::Jump { label: JoinId(1), args: vec![1] }, // 2
            CoreFrame::Join {
                label: JoinId(1),
                params: vec![VarId(10)],
                rhs: 0,
                body: 2,
            },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(42))));
    }

    #[test]
    fn test_join_multi_param() {
        let nodes = vec![
            CoreFrame::Var(VarId(10)), // 0: x
            CoreFrame::Var(VarId(11)), // 1: y
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 1] }, // 2: x + y
            CoreFrame::Lit(Literal::LitInt(10)), // 3
            CoreFrame::Lit(Literal::LitInt(20)), // 4
            CoreFrame::Jump { label: JoinId(1), args: vec![3, 4] }, // 5
            CoreFrame::Join {
                label: JoinId(1),
                params: vec![VarId(10), VarId(11)],
                rhs: 2,
                body: 5,
            },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(30))));
    }

    #[test]
    fn test_join_scoping() {
        // let y = 10 in join j(x) = x + y in let y = 20 in jump j(5)
        // result should be 15, not 25.
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::Var(VarId(20)), // 1: y (captured)
            CoreFrame::Var(VarId(10)), // 2: x
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![1, 2] }, // 3: x + y
            
            CoreFrame::Lit(Literal::LitInt(20)), // 4
            CoreFrame::Lit(Literal::LitInt(5)), // 5
            CoreFrame::Jump { label: JoinId(1), args: vec![5] }, // 6
            CoreFrame::LetNonRec { binder: VarId(20), rhs: 4, body: 6 }, // 7: let y = 20 in jump j(5)
            
            CoreFrame::Join {
                label: JoinId(1),
                params: vec![VarId(10)],
                rhs: 3,
                body: 7,
            }, // 8: join j(x) = x+y in ...
            
            CoreFrame::LetNonRec { binder: VarId(20), rhs: 0, body: 8 }, // 9: let y = 10 in ...
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(15))));
    }

    // --- 8. Thunks ---

    #[test]
    fn test_thunk_laziness() {
        // let x = <latent error> in 42
        // We use an unbound variable (VarId(999)) to simulate an error that would occur if forced.
        let nodes = vec![
            CoreFrame::Var(VarId(999)), // 0: unbound
            CoreFrame::Lit(Literal::LitInt(42)), // 1
            CoreFrame::LetNonRec { binder: VarId(1), rhs: 0, body: 1 },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(42))));
    }

    #[test]
    fn test_thunk_caching() {
        // let x = 1 + 1 in x + x
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![0, 0] }, // 1: 1 + 1
            CoreFrame::Var(VarId(1)), // 2: x
            CoreFrame::PrimOp { op: PrimOpKind::IntAdd, args: vec![2, 2] }, // 3: x + x
            CoreFrame::LetNonRec { binder: VarId(1), rhs: 1, body: 3 },
        ];
        let res = eval_expr(nodes).unwrap();
        assert!(matches!(res, Value::Lit(Literal::LitInt(4))));
    }

    #[test]
    fn test_thunk_blackhole() {
        // letrec x = x in x
        let nodes = vec![
            CoreFrame::Var(VarId(1)), // 0: x
            CoreFrame::LetRec {
                bindings: vec![(VarId(1), 0)],
                body: 0,
            },
        ];
        let res = eval_expr(nodes);
        assert!(matches!(res, Err(EvalError::InfiniteLoop(_))));
    }

    // --- 9. CBOR roundtrip & Differential ---

    #[test]
    fn test_differential_identity() {
        // Rust-constructed identity
        let nodes = vec![
            CoreFrame::Var(VarId(1)),
            CoreFrame::Lam { binder: VarId(1), body: 0 },
        ];
        let rust_res = eval_expr(nodes).unwrap();

        // Haskell-compiled identity from CBOR
        let cbor_res = eval_cbor("../haskell/test/Identity_cbor/identity.cbor").unwrap();

        // Both should be Closures
        assert!(matches!(rust_res, Value::Closure(_, _, _)));
        assert!(matches!(cbor_res, Value::Closure(_, _, _)));
    }

    #[test]
    fn test_cbor_identity() {
        let res = eval_cbor("../haskell/test/Identity_cbor/identity.cbor").unwrap();
        match res {
            Value::Closure(_, _, _) => (),
            _ => panic!("Expected Closure, got {:?}", res),
        }
    }

    #[test]
    fn test_cbor_apply() {
        let res = eval_cbor("../haskell/test/Identity_cbor/apply.cbor").unwrap();
        match res {
            Value::Closure(_, _, _) => (),
            _ => panic!("Expected Closure, got {:?}", res),
        }
    }

    #[test]
    fn test_cbor_const() {
        let res = eval_cbor("../haskell/test/Identity_cbor/const'.cbor").unwrap();
        match res {
            Value::Closure(_, _, _) => (),
            _ => panic!("Expected Closure, got {:?}", res),
        }
    }
}
