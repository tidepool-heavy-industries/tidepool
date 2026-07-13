//! Case reduction pass for Core expressions.

use crate::occ::{get_occ, occ_analysis, Occ, OccMap};
use crate::{Changed, Pass};
use tidepool_repr::{replace_subtree, AltCon, CoreExpr, CoreFrame};

/// A pass that performs case-of-known-constructor and case-of-known-literal
/// reductions — gated against work duplication the same way [`BetaReduce`] and
/// [`Inline`] are (see `try_case_reduce_at`).
///
/// [`BetaReduce`]: crate::beta::BetaReduce
/// [`Inline`]: crate::inline::Inline
pub struct CaseReduce;

impl Pass for CaseReduce {
    fn run(&self, expr: &mut CoreExpr) -> Changed {
        crate::apply_rewrite(expr, |e| try_case_reduce(e, &occ_analysis(e)))
    }

    fn name(&self) -> &str {
        "CaseReduce"
    }
}

fn try_case_reduce(expr: &CoreExpr, occ_map: &OccMap) -> Option<CoreExpr> {
    crate::rewrite::find_redex(expr, |expr, idx| try_case_reduce_at(expr, idx, occ_map))
}

/// Case-of-known-constructor / case-of-known-literal test for a single node.
/// Returns `Some` only when the `Case`'s scrutinee is a manifest `Con`/`Lit`
/// with a matching (and, for `DataAlt`, correctly-arity'd) alternative; every
/// other shape — including a `Case` with no matching alt or an arity mismatch —
/// returns `None` so the search driver descends into children.
///
/// Substitution splices a full copy of the replacement at every binder
/// occurrence, and a `Con`'s fields are arbitrary sub-expressions (construction
/// never forces them), so reducing `case Con [e] of w { C [y] -> y + y }` with
/// a non-trivial `e` would turn one shared evaluation into two. The same
/// work-preservation gate as [`BetaReduce`](crate::beta::BetaReduce) therefore
/// applies to each substitution: fire only when the binder is used at most
/// once, or the spliced tree is free to copy (a `Var`/`Lit`; for the case
/// binder, a `Con` whose fields are all `Var`/`Lit`). Any gated substitution
/// aborts the whole reduction — the redex stays, sharing is preserved.
fn try_case_reduce_at(expr: &CoreExpr, idx: usize, occ_map: &OccMap) -> Option<CoreExpr> {
    let CoreFrame::Case {
        scrutinee,
        binder,
        alts,
    } = &expr.nodes[idx]
    else {
        return None;
    };
    match &expr.nodes[*scrutinee] {
        CoreFrame::Con { tag, fields } => {
            // Find matching DataAlt or Default
            let alt = alts
                .iter()
                .find(|a| matches!(&a.con, AltCon::DataAlt(t) if t == tag))
                .or_else(|| alts.iter().find(|a| matches!(&a.con, AltCon::Default)))?;

            // Arity check for DataAlt: binders must match fields.
            // If mismatch, skip this reduction (malformed IR).
            if let AltCon::DataAlt(_) = &alt.con {
                if alt.binders.len() != fields.len() {
                    return None;
                }
            }

            // Work-preservation gate (see doc comment): every substitution must
            // be single-use or splice a trivially-copyable tree.
            let trivial =
                |i: usize| matches!(&expr.nodes[i], CoreFrame::Var(_) | CoreFrame::Lit(_));
            if let AltCon::DataAlt(_) = &alt.con {
                for (alt_binder, field_idx) in alt.binders.iter().zip(fields.iter()) {
                    if get_occ(occ_map, *alt_binder) == Occ::Many && !trivial(*field_idx) {
                        return None;
                    }
                }
            }
            // Copying the scrutinee `Con` for the case binder duplicates its
            // field subtrees, so it is only free when every field is trivial.
            if get_occ(occ_map, *binder) == Occ::Many && !fields.iter().copied().all(trivial) {
                return None;
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
                .or_else(|| alts.iter().find(|a| matches!(&a.con, AltCon::Default)))?;

            let mut body = expr.extract_subtree(alt.body);
            // Substitute case binder with scrutinee literal
            let scrut_tree = expr.extract_subtree(*scrutinee);
            body = tidepool_repr::subst::subst(&body, *binder, &scrut_tree);
            Some(replace_subtree(expr, idx, &body))
        }
        _ => None,
    }
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

        let (Value::Lit(ref l1), Value::Lit(ref l2)) = (val_before, val_after) else {
            panic!("Value mismatch or not Lit");
        };
        assert_eq!(l1, l2);
        let Literal::LitInt(3) = *l1 else {
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
    fn test_case_multi_use_nontrivial_field_not_reduced() {
        // case Con(tag=1, [1 + 2]) of w { DataAlt(1) [y] -> y + y }
        // y occurs twice and the field is a non-trivial PrimOp: reducing would
        // splice `1 + 2` at both occurrences, duplicating the work.
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Lit(Literal::LitInt(2)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2: 1 + 2 (non-trivial field)
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![2],
            }, // 3
            CoreFrame::Var(VarId(10)),          // 4: y
            CoreFrame::Var(VarId(10)),          // 5: y (distinct occurrence)
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![4, 5],
            }, // 6: y + y
            CoreFrame::Case {
                scrutinee: 3,
                binder: VarId(2),
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(1)),
                    binders: vec![VarId(10)],
                    body: 6,
                }],
            }, // 7
        ];
        let mut expr = CoreExpr { nodes };
        let changed = CaseReduce.run(&mut expr);
        assert!(
            !changed,
            "multi-use alt binder + non-trivial field must not reduce"
        );
    }

    #[test]
    fn test_case_multi_use_trivial_field_reduced() {
        // case Con(tag=1, [42]) of w { DataAlt(1) [y] -> y + y }
        // y occurs twice but the field is a Lit — free to copy, so reduce.
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0],
            }, // 1
            CoreFrame::Var(VarId(10)),           // 2: y
            CoreFrame::Var(VarId(10)),           // 3: y
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![2, 3],
            }, // 4: y + y
            CoreFrame::Case {
                scrutinee: 1,
                binder: VarId(2),
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(1)),
                    binders: vec![VarId(10)],
                    body: 4,
                }],
            }, // 5
        ];
        let mut expr = CoreExpr { nodes };
        let changed = CaseReduce.run(&mut expr);
        assert!(changed, "trivial fields are free to copy");
        let mut heap = VecHeap::new();
        let val = tidepool_eval::eval(&expr, &Env::new(), &mut heap).unwrap();
        let Value::Lit(Literal::LitInt(84)) = val else {
            panic!("Expected 84, got {val:?}");
        };
    }

    #[test]
    fn test_case_multi_use_case_binder_nontrivial_con_not_reduced() {
        // case Con(tag=1, [1 + 2]) of w { DataAlt(1) [] -> ... } is malformed
        // (arity), so use a Default alt: case Con(tag=1, [1 + 2]) of w
        // { Default -> Con(tag=2, [w, w]) }. w occurs twice and copying the
        // scrutinee Con duplicates its non-trivial field.
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Lit(Literal::LitInt(2)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2: 1 + 2
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![2],
            }, // 3: scrutinee
            CoreFrame::Var(VarId(2)),           // 4: w
            CoreFrame::Var(VarId(2)),           // 5: w
            CoreFrame::Con {
                tag: DataConId(2),
                fields: vec![4, 5],
            }, // 6: Con(2, [w, w])
            CoreFrame::Case {
                scrutinee: 3,
                binder: VarId(2),
                alts: vec![Alt {
                    con: AltCon::Default,
                    binders: vec![],
                    body: 6,
                }],
            }, // 7
        ];
        let mut expr = CoreExpr { nodes };
        let changed = CaseReduce.run(&mut expr);
        assert!(
            !changed,
            "multi-use case binder + non-trivial Con fields must not reduce"
        );
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

        match (&val_before, &val_after) {
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
