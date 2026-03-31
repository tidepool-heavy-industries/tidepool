//! Case reduction pass for Core expressions.

use tidepool_eval::{Changed, Pass};
use tidepool_repr::{get_children, replace_subtree, AltCon, CoreExpr, CoreFrame};

/// A pass that performs case-of-known-constructor and case-of-known-literal reductions.
pub struct CaseReduce;

impl Pass for CaseReduce {
    fn run(&self, expr: &mut CoreExpr) -> Changed {
        if expr.nodes.is_empty() {
            return false;
        }
        match try_case_reduce(expr) {
            Some(new_expr) => {
                *expr = new_expr;
                true
            }
            None => false,
        }
    }

    fn name(&self) -> &str {
        "CaseReduce"
    }
}

fn try_case_reduce(expr: &CoreExpr) -> Option<CoreExpr> {
    try_case_reduce_at(expr, expr.nodes.len() - 1)
}

fn try_case_reduce_at(expr: &CoreExpr, idx: usize) -> Option<CoreExpr> {
    match &expr.nodes[idx] {
        CoreFrame::Case {
            scrutinee,
            binder,
            alts,
        } => {
            match &expr.nodes[*scrutinee] {
                CoreFrame::Con { tag, fields } => {
                    // Find matching DataAlt or Default
                    let alt = alts
                        .iter()
                        .find(|a| matches!(&a.con, AltCon::DataAlt(t) if t == tag))
                        .or_else(|| alts.iter().find(|a| matches!(&a.con, AltCon::Default)));

                    let Some(alt) = alt else {
                        // No matching alt — try children
                        return try_children(expr, idx);
                    };

                    // Arity check for DataAlt: binders must match fields.
                    // If mismatch, skip this reduction (malformed IR).
                    if let AltCon::DataAlt(_) = &alt.con {
                        if alt.binders.len() != fields.len() {
                            return try_children(expr, idx);
                        }
                    }

                    let mut body = expr.extract_subtree(alt.body);
                    // Bind fields to alt binders
                    if let AltCon::DataAlt(_) = &alt.con {
                        for (alt_binder, field_idx) in alt.binders.iter().zip(fields.iter()) {
                            let field_tree = expr.extract_subtree(*field_idx);
                            body = tidepool_repr::subst::subst(&body, *alt_binder, &field_tree);
                        }
                    }
                    // Substitute case binder with scrutinee
                    let scrut_tree = expr.extract_subtree(*scrutinee);
                    body = tidepool_repr::subst::subst(&body, *binder, &scrut_tree);
                    Some(replace_subtree(expr, idx, &body))
                }
                CoreFrame::Lit(lit) => {
                    let alt = alts
                        .iter()
                        .find(|a| matches!(&a.con, AltCon::LitAlt(l) if l == lit))
                        .or_else(|| alts.iter().find(|a| matches!(&a.con, AltCon::Default)));

                    let Some(alt) = alt else {
                        return try_children(expr, idx);
                    };

                    let mut body = expr.extract_subtree(alt.body);
                    // Substitute case binder with scrutinee literal
                    let scrut_tree = expr.extract_subtree(*scrutinee);
                    body = tidepool_repr::subst::subst(&body, *binder, &scrut_tree);
                    Some(replace_subtree(expr, idx, &body))
                }
                _ => try_children(expr, idx),
            }
        }
        _ => try_children(expr, idx),
    }
}

fn try_children(expr: &CoreExpr, idx: usize) -> Option<CoreExpr> {
    get_children(&expr.nodes[idx])
        .into_iter()
        .find_map(|child| try_case_reduce_at(expr, child))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_eval::env::Env;
    use tidepool_eval::heap::VecHeap;
    use tidepool_eval::value::Value;
    use tidepool_repr::{Alt, DataConId, Literal, PrimOpKind, VarId};

    #[test]
    fn test_case_known_con() {
        // case Con(tag=1, [42]) of w { DataAlt(1) [y] -> y }
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0],
            }, // 1
            CoreFrame::Var(VarId(3)),            // 2: y
            CoreFrame::Case {
                scrutinee: 1,
                binder: VarId(2), // w
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(1)),
                    binders: vec![VarId(3)],
                    body: 2,
                }],
            }, // 3
        ];
        let mut expr = CoreExpr { nodes };
        let pass = CaseReduce;
        let changed = pass.run(&mut expr);
        assert!(changed);
        // Result should be Lit(42)
        assert_eq!(expr.nodes.len(), 1);
        assert!(matches!(expr.nodes[0], CoreFrame::Lit(Literal::LitInt(42))));
    }

    #[test]
    fn test_case_known_con_pair() {
        // case Con(tag=1, [1, 2]) of w { DataAlt(1) [a, b] -> PrimOp(IntAdd, [a, b]) }
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Lit(Literal::LitInt(2)), // 1
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0, 1],
            }, // 2
            CoreFrame::Var(VarId(10)),          // 3: a
            CoreFrame::Var(VarId(11)),          // 4: b
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![3, 4],
            }, // 5
            CoreFrame::Case {
                scrutinee: 2,
                binder: VarId(12),
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(1)),
                    binders: vec![VarId(10), VarId(11)],
                    body: 5,
                }],
            }, // 6
        ];
        let mut expr = CoreExpr { nodes };
        let pass = CaseReduce;

        let mut heap = VecHeap::new();
        let val_before = tidepool_eval::eval(&expr, &Env::new(), &mut heap).unwrap();

        let changed = pass.run(&mut expr);
        assert!(changed);

        let mut heap2 = VecHeap::new();
        let val_after = tidepool_eval::eval(&expr, &Env::new(), &mut heap2).unwrap();

        let (Value::Lit(l1), Value::Lit(l2)) = (val_before, val_after) else {
            panic!("Value mismatch or not Lit");
        };
        assert_eq!(l1, l2);
        let Literal::LitInt(3) = l1 else {
            panic!("Expected 3, got {:?}", l1);
        };
    }

    #[test]
    fn test_case_known_lit() {
        // case 3 of w { LitAlt(1) -> 10; LitAlt(3) -> 30; Default -> 99 }
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(3)),  // 0
            CoreFrame::Lit(Literal::LitInt(10)), // 1
            CoreFrame::Lit(Literal::LitInt(30)), // 2
            CoreFrame::Lit(Literal::LitInt(99)), // 3
            CoreFrame::Case {
                scrutinee: 0,
                binder: VarId(10),
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(1)),
                        binders: vec![],
                        body: 1,
                    },
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(3)),
                        binders: vec![],
                        body: 2,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 3,
                    },
                ],
            }, // 4
        ];
        let mut expr = CoreExpr { nodes };
        let pass = CaseReduce;
        let changed = pass.run(&mut expr);
        assert!(changed);
        // Result should be 30
        assert!(matches!(
            expr.nodes[expr.nodes.len() - 1],
            CoreFrame::Lit(Literal::LitInt(30))
        ));
    }

    #[test]
    fn test_case_known_lit_default() {
        // case 3 of w { LitAlt(1) -> 10; Default -> 99 }
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(3)),  // 0
            CoreFrame::Lit(Literal::LitInt(10)), // 1
            CoreFrame::Lit(Literal::LitInt(99)), // 2
            CoreFrame::Case {
                scrutinee: 0,
                binder: VarId(10),
                alts: vec![
                    Alt {
                        con: AltCon::LitAlt(Literal::LitInt(1)),
                        binders: vec![],
                        body: 1,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 2,
                    },
                ],
            }, // 3
        ];
        let mut expr = CoreExpr { nodes };
        let pass = CaseReduce;
        let changed = pass.run(&mut expr);
        assert!(changed);
        // Result should be 99
        assert!(matches!(
            expr.nodes[expr.nodes.len() - 1],
            CoreFrame::Lit(Literal::LitInt(99))
        ));
    }

    #[test]
    fn test_case_unknown_untouched() {
        // case Var(x) of w { Default -> 42 }
        let nodes = vec![
            CoreFrame::Var(VarId(1)),            // 0: x
            CoreFrame::Lit(Literal::LitInt(42)), // 1
            CoreFrame::Case {
                scrutinee: 0,
                binder: VarId(2),
                alts: vec![Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: 1,
                }],
            }, // 2
        ];
        let mut expr = CoreExpr { nodes };
        let pass = CaseReduce;
        let changed = pass.run(&mut expr);
        assert!(!changed);
    }

    #[test]
    fn test_case_binder_substituted() {
        // case Con(tag=1, [42]) of w { DataAlt(1) [y] -> w }
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0],
            }, // 1
            CoreFrame::Var(VarId(2)),            // 2: w
            CoreFrame::Case {
                scrutinee: 1,
                binder: VarId(2), // w
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(1)),
                    binders: vec![VarId(3)],
                    body: 2,
                }],
            }, // 3
        ];
        let mut expr = CoreExpr { nodes };
        let pass = CaseReduce;
        let changed = pass.run(&mut expr);
        assert!(changed);
        // Result should be Con(tag=1, [42])
        let CoreFrame::Con { tag, fields } = &expr.nodes[expr.nodes.len() - 1] else {
            panic!("Expected Con, got {:?}", expr.nodes[expr.nodes.len() - 1]);
        };
        assert_eq!(tag.0, 1);
        assert_eq!(fields.len(), 1);
        let CoreFrame::Lit(Literal::LitInt(42)) = &expr.nodes[fields[0]] else {
            panic!("Expected field to be 42");
        };
    }

    #[test]
    fn test_case_reduce_preserves_eval() {
        // case Con(tag=1, [1, 2]) of w { DataAlt(1) [a, b] -> a + b; Default -> 0 }
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Lit(Literal::LitInt(2)), // 1
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0, 1],
            }, // 2
            CoreFrame::Var(VarId(10)),          // 3: a
            CoreFrame::Var(VarId(11)),          // 4: b
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![3, 4],
            }, // 5
            CoreFrame::Lit(Literal::LitInt(0)), // 6
            CoreFrame::Case {
                scrutinee: 2,
                binder: VarId(12),
                alts: vec![
                    Alt {
                        con: AltCon::DataAlt(DataConId(1)),
                        binders: vec![VarId(10), VarId(11)],
                        body: 5,
                    },
                    Alt {
                        con: AltCon::Default,
                        binders: vec![],
                        body: 6,
                    },
                ],
            }, // 7
        ];
        let mut expr = CoreExpr { nodes };
        let pass = CaseReduce;

        let mut heap = VecHeap::new();
        let val_before = tidepool_eval::eval(&expr, &Env::new(), &mut heap).unwrap();

        pass.run(&mut expr);

        let mut heap2 = VecHeap::new();
        let val_after = tidepool_eval::eval(&expr, &Env::new(), &mut heap2).unwrap();

        match (val_before, val_after) {
            (Value::Lit(l1), Value::Lit(l2)) => assert_eq!(l1, l2),
            (Value::Con(t1, f1), Value::Con(t2, f2)) => {
                assert_eq!(t1, t2);
                assert_eq!(f1.len(), f2.len());
                // Simple check for literals in fields
                for (v1, v2) in f1.iter().zip(f2.iter()) {
                    let (Value::Lit(ll1), Value::Lit(ll2)) = (v1, v2) else {
                        continue;
                    };
                    assert_eq!(ll1, ll2);
                }
            }
            (v1, v2) => panic!(
                "Value mismatch or unsupported for eval check: {:?}, {:?}",
                v1, v2
            ),
        }
    }
}
