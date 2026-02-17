use core_repr::{AltCon, CoreExpr, CoreFrame, MapLayer};
use core_eval::{Changed, Pass};
use std::collections::HashMap;

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

                    if let Some(alt) = alt {
                        let mut body = extract_subtree(expr, alt.body);
                        // Bind fields to alt binders
                        if let AltCon::DataAlt(_) = &alt.con {
                            for (alt_binder, field_idx) in alt.binders.iter().zip(fields.iter()) {
                                let field_tree = extract_subtree(expr, *field_idx);
                                body = core_repr::subst::subst(&body, *alt_binder, &field_tree);
                            }
                        }
                        // Substitute case binder with scrutinee
                        let scrut_tree = extract_subtree(expr, *scrutinee);
                        body = core_repr::subst::subst(&body, *binder, &scrut_tree);
                        Some(replace_subtree(expr, idx, &body))
                    } else {
                        // No matching alt — try children
                        try_children(expr, idx)
                    }
                }
                CoreFrame::Lit(lit) => {
                    let alt = alts
                        .iter()
                        .find(|a| matches!(&a.con, AltCon::LitAlt(l) if l == lit))
                        .or_else(|| alts.iter().find(|a| matches!(&a.con, AltCon::Default)));

                    if let Some(alt) = alt {
                        let mut body = extract_subtree(expr, alt.body);
                        // Substitute case binder with scrutinee literal
                        let scrut_tree = extract_subtree(expr, *scrutinee);
                        body = core_repr::subst::subst(&body, *binder, &scrut_tree);
                        Some(replace_subtree(expr, idx, &body))
                    } else {
                        try_children(expr, idx)
                    }
                }
                _ => try_children(expr, idx),
            }
        }
        _ => try_children(expr, idx),
    }
}

fn try_children(expr: &CoreExpr, idx: usize) -> Option<CoreExpr> {
    let children = get_children(&expr.nodes[idx]);
    for child in children {
        if let Some(result) = try_case_reduce_at(expr, child) {
            return Some(result);
        }
    }
    None
}

fn get_children(frame: &CoreFrame<usize>) -> Vec<usize> {
    match frame {
        CoreFrame::Var(_) | CoreFrame::Lit(_) => vec![],
        CoreFrame::App { fun, arg } => vec![*fun, *arg],
        CoreFrame::Lam { body, .. } => vec![*body],
        CoreFrame::LetNonRec { rhs, body, .. } => vec![*rhs, *body],
        CoreFrame::LetRec { bindings, body, .. } => {
            let mut c: Vec<usize> = bindings.iter().map(|(_, r)| *r).collect();
            c.push(*body);
            c
        }
        CoreFrame::Case {
            scrutinee, alts, ..
        } => {
            let mut c = vec![*scrutinee];
            for alt in alts {
                c.push(alt.body);
            }
            c
        }
        CoreFrame::Con { fields, .. } => fields.clone(),
        CoreFrame::Join { rhs, body, .. } => vec![*rhs, *body],
        CoreFrame::Jump { args, .. } => args.clone(),
        CoreFrame::PrimOp { args, .. } => args.clone(),
    }
}

fn extract_subtree(expr: &CoreExpr, root_idx: usize) -> CoreExpr {
    let mut new_nodes = Vec::new();
    let mut old_to_new = HashMap::new();
    collect(root_idx, expr, &mut new_nodes, &mut old_to_new);
    CoreExpr { nodes: new_nodes }
}

fn collect(
    idx: usize,
    expr: &CoreExpr,
    new_nodes: &mut Vec<CoreFrame<usize>>,
    old_to_new: &mut HashMap<usize, usize>,
) -> usize {
    if let Some(&new_idx) = old_to_new.get(&idx) {
        return new_idx;
    }
    let mapped = expr.nodes[idx]
        .clone()
        .map_layer(|child| collect(child, expr, new_nodes, old_to_new));
    let new_idx = new_nodes.len();
    new_nodes.push(mapped);
    old_to_new.insert(idx, new_idx);
    new_idx
}

fn replace_subtree(expr: &CoreExpr, target_idx: usize, replacement: &CoreExpr) -> CoreExpr {
    let mut new_nodes = Vec::new();
    let mut old_to_new = HashMap::new();
    rebuild(
        expr,
        expr.nodes.len() - 1,
        target_idx,
        replacement,
        &mut new_nodes,
        &mut old_to_new,
    );
    CoreExpr { nodes: new_nodes }
}

fn rebuild(
    expr: &CoreExpr,
    idx: usize,
    target: usize,
    replacement: &CoreExpr,
    new_nodes: &mut Vec<CoreFrame<usize>>,
    old_to_new: &mut HashMap<usize, usize>,
) -> usize {
    if let Some(&ni) = old_to_new.get(&idx) {
        return ni;
    }
    if idx == target {
        let offset = new_nodes.len();
        for node in &replacement.nodes {
            new_nodes.push(node.clone().map_layer(|i| i + offset));
        }
        let root = new_nodes.len() - 1;
        old_to_new.insert(idx, root);
        return root;
    }
    let mapped = expr.nodes[idx]
        .clone()
        .map_layer(|child| rebuild(expr, child, target, replacement, new_nodes, old_to_new));
    let new_idx = new_nodes.len();
    new_nodes.push(mapped);
    old_to_new.insert(idx, new_idx);
    new_idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_eval::env::Env;
    use core_eval::heap::VecHeap;
    use core_eval::value::Value;
    use core_repr::{Alt, DataConId, Literal, PrimOpKind, VarId};

    #[test]
    fn test_case_known_con() {
        // case Con(tag=1, [42]) of w { DataAlt(1) [y] -> y }
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0],
            }, // 1
            CoreFrame::Var(VarId(3)), // 2: y
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
            CoreFrame::Var(VarId(10)),                                     // 3: a
            CoreFrame::Var(VarId(11)),                                     // 4: b
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
        let val_before = core_eval::eval(&expr, &Env::new(), &mut heap).unwrap();

        let changed = pass.run(&mut expr);
        assert!(changed);

        let mut heap2 = VecHeap::new();
        let val_after = core_eval::eval(&expr, &Env::new(), &mut heap2).unwrap();

        match (val_before, val_after) {
            (Value::Lit(l1), Value::Lit(l2)) => {
                assert_eq!(l1, l2);
                if let Literal::LitInt(3) = l1 {
                    // OK
                } else {
                    panic!("Expected 3, got {:?}", l1);
                }
            }
            (v1, v2) => panic!("Value mismatch or not Lit: {:?}, {:?}", v1, v2),
        }
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
            CoreFrame::Var(VarId(2)), // 2: w
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
        if let CoreFrame::Con { tag, fields } = &expr.nodes[expr.nodes.len() - 1] {
            assert_eq!(tag.0, 1);
            assert_eq!(fields.len(), 1);
            if let CoreFrame::Lit(Literal::LitInt(42)) = &expr.nodes[fields[0]] {
                // OK
            } else {
                panic!("Expected field to be 42");
            }
        } else {
            panic!("Expected Con, got {:?}", expr.nodes[expr.nodes.len() - 1]);
        }
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
            CoreFrame::Var(VarId(10)),                                     // 3: a
            CoreFrame::Var(VarId(11)),                                     // 4: b
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![3, 4],
            }, // 5
            CoreFrame::Lit(Literal::LitInt(0)),                             // 6
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
        let val_before = core_eval::eval(&expr, &Env::new(), &mut heap).unwrap();

        pass.run(&mut expr);

        let mut heap2 = VecHeap::new();
        let val_after = core_eval::eval(&expr, &Env::new(), &mut heap2).unwrap();

        match (val_before, val_after) {
            (Value::Lit(l1), Value::Lit(l2)) => assert_eq!(l1, l2),
            (Value::Con(t1, f1), Value::Con(t2, f2)) => {
                assert_eq!(t1, t2);
                assert_eq!(f1.len(), f2.len());
                // Simple check for literals in fields
                for (v1, v2) in f1.iter().zip(f2.iter()) {
                    if let (Value::Lit(ll1), Value::Lit(ll2)) = (v1, v2) {
                        assert_eq!(ll1, ll2);
                    }
                }
            }
            (v1, v2) => panic!("Value mismatch or unsupported for eval check: {:?}, {:?}", v1, v2),
        }
    }
}
