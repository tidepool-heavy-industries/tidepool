//! Partial evaluation pass for Core expressions.

use rustc_hash::FxHashMap;
use tidepool_eval::{Changed, Pass};
use tidepool_repr::{Alt, AltCon, CoreExpr, CoreFrame, DataConId, Literal, PrimOpKind, VarId};

/// A value that might be known during partial evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PartialValue {
    /// The value is statically known.
    Known(KnownValue),
    /// The value is only known at runtime.
    Unknown,
}

/// A statically known value.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KnownValue {
    /// A literal value.
    Lit(Literal),
    /// A data constructor with known fields.
    Con(DataConId, Vec<KnownValue>),
}

/// Environment mapping variables to their partial values.
type PartialEnv = FxHashMap<VarId, PartialValue>;

/// First-order partial evaluation pass.
pub struct PartialEval;

impl Pass for PartialEval {
    fn run(&self, expr: &mut CoreExpr) -> Changed {
        if expr.nodes.is_empty() {
            return false;
        }
        let mut new_nodes = Vec::new();
        let (root_idx, _) = partial_eval_at(
            expr,
            expr.nodes.len() - 1,
            &PartialEnv::default(),
            &mut new_nodes,
        );
        let new_expr = CoreExpr { nodes: new_nodes }.extract_subtree(root_idx);
        if new_expr != *expr {
            *expr = new_expr;
            true
        } else {
            false
        }
    }
    fn name(&self) -> &str {
        "PartialEval"
    }
}

/// Recursively partially evaluate an expression at a given index.
fn partial_eval_at(
    expr: &CoreExpr,
    idx: usize,
    env: &PartialEnv,
    new_nodes: &mut Vec<CoreFrame<usize>>,
) -> (usize, PartialValue) {
    match &expr.nodes[idx] {
        CoreFrame::Var(v) => match env.get(v) {
            Some(PartialValue::Known(kv)) => {
                let ni = emit_known(kv, new_nodes);
                (ni, PartialValue::Known(kv.clone()))
            }
            _ => {
                let ni = new_nodes.len();
                new_nodes.push(CoreFrame::Var(*v));
                (ni, PartialValue::Unknown)
            }
        },
        CoreFrame::Lit(lit) => {
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::Lit(lit.clone()));
            (ni, PartialValue::Known(KnownValue::Lit(lit.clone())))
        }
        CoreFrame::Con { tag, fields } => {
            let (fi, fv): (Vec<_>, Vec<_>) = fields
                .iter()
                .map(|&f| partial_eval_at(expr, f, env, new_nodes))
                .unzip();
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::Con {
                tag: *tag,
                fields: fi,
            });
            let known_fields = fv
                .into_iter()
                .map(|v| match v {
                    PartialValue::Known(k) => Some(k),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>();

            if let Some(kf) = known_fields {
                (ni, PartialValue::Known(KnownValue::Con(*tag, kf)))
            } else {
                (ni, PartialValue::Unknown)
            }
        }
        CoreFrame::LetNonRec { binder, rhs, body } => {
            let (rhs_i, rhs_v) = partial_eval_at(expr, *rhs, env, new_nodes);
            let mut new_env = env.clone();
            new_env.insert(*binder, rhs_v.clone());
            if matches!(rhs_v, PartialValue::Known(_)) {
                // Known RHS: evaluate body with known binder, skip the let
                partial_eval_at(expr, *body, &new_env, new_nodes)
            } else {
                let (body_i, body_v) = partial_eval_at(expr, *body, &new_env, new_nodes);
                let ni = new_nodes.len();
                new_nodes.push(CoreFrame::LetNonRec {
                    binder: *binder,
                    rhs: rhs_i,
                    body: body_i,
                });
                (ni, body_v)
            }
        }
        CoreFrame::LetRec { bindings, body } => {
            let mut new_env = env.clone();
            for (b, _) in bindings {
                new_env.insert(*b, PartialValue::Unknown);
            }
            let nb: Vec<_> = bindings
                .iter()
                .map(|(b, r)| {
                    let (ri, _) = partial_eval_at(expr, *r, &new_env, new_nodes);
                    (*b, ri)
                })
                .collect();
            let (bi, bv) = partial_eval_at(expr, *body, &new_env, new_nodes);
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::LetRec {
                bindings: nb,
                body: bi,
            });
            (ni, bv)
        }
        CoreFrame::Case {
            scrutinee,
            binder,
            alts,
        } => {
            let (si, sv) = partial_eval_at(expr, *scrutinee, env, new_nodes);
            match &sv {
                PartialValue::Known(KnownValue::Con(tag, field_vals)) => {
                    let matched = alts
                        .iter()
                        .find(|a| matches!(&a.con, AltCon::DataAlt(t) if t == tag))
                        .or_else(|| alts.iter().find(|a| matches!(&a.con, AltCon::Default)));
                    if let Some(alt) = matched {
                        let mut new_env = env.clone();
                        new_env.insert(*binder, sv.clone());
                        if let AltCon::DataAlt(_) = &alt.con {
                            for (b, fv) in alt.binders.iter().zip(field_vals.iter()) {
                                new_env.insert(*b, PartialValue::Known(fv.clone()));
                            }
                        }
                        partial_eval_at(expr, alt.body, &new_env, new_nodes)
                    } else {
                        emit_residual_case(expr, si, binder, alts, env, new_nodes)
                    }
                }
                PartialValue::Known(KnownValue::Lit(lit)) => {
                    let matched = alts
                        .iter()
                        .find(|a| matches!(&a.con, AltCon::LitAlt(l) if l == lit))
                        .or_else(|| alts.iter().find(|a| matches!(&a.con, AltCon::Default)));
                    if let Some(alt) = matched {
                        let mut new_env = env.clone();
                        new_env.insert(*binder, sv.clone());
                        partial_eval_at(expr, alt.body, &new_env, new_nodes)
                    } else {
                        emit_residual_case(expr, si, binder, alts, env, new_nodes)
                    }
                }
                PartialValue::Unknown => emit_residual_case(expr, si, binder, alts, env, new_nodes),
            }
        }
        CoreFrame::PrimOp { op, args } => {
            let (ai, av): (Vec<_>, Vec<_>) = args
                .iter()
                .map(|&a| partial_eval_at(expr, a, env, new_nodes))
                .unzip();
            if let Some(result) = try_eval_primop(*op, &av) {
                let ni = new_nodes.len();
                new_nodes.push(CoreFrame::Lit(result.clone()));
                (ni, PartialValue::Known(KnownValue::Lit(result)))
            } else {
                let ni = new_nodes.len();
                new_nodes.push(CoreFrame::PrimOp { op: *op, args: ai });
                (ni, PartialValue::Unknown)
            }
        }
        CoreFrame::App { fun, arg } => {
            let (fi, _) = partial_eval_at(expr, *fun, env, new_nodes);
            let (ai, _) = partial_eval_at(expr, *arg, env, new_nodes);
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::App { fun: fi, arg: ai });
            (ni, PartialValue::Unknown)
        }
        CoreFrame::Lam { binder, body } => {
            let (bi, _) = partial_eval_at(expr, *body, env, new_nodes);
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::Lam {
                binder: *binder,
                body: bi,
            });
            (ni, PartialValue::Unknown)
        }
        CoreFrame::Join {
            label,
            params,
            rhs,
            body,
        } => {
            let (ri, _) = partial_eval_at(expr, *rhs, env, new_nodes);
            let (bi, bv) = partial_eval_at(expr, *body, env, new_nodes);
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::Join {
                label: *label,
                params: params.clone(),
                rhs: ri,
                body: bi,
            });
            (ni, bv)
        }
        CoreFrame::Jump { label, args } => {
            let ai: Vec<_> = args
                .iter()
                .map(|&a| partial_eval_at(expr, a, env, new_nodes).0)
                .collect();
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::Jump {
                label: *label,
                args: ai,
            });
            (ni, PartialValue::Unknown)
        }
    }
}

/// Emit nodes for a known value into the new nodes vector.
fn emit_known(kv: &KnownValue, new_nodes: &mut Vec<CoreFrame<usize>>) -> usize {
    match kv {
        KnownValue::Lit(lit) => {
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::Lit(lit.clone()));
            ni
        }
        KnownValue::Con(tag, fields) => {
            let fi: Vec<usize> = fields.iter().map(|k| emit_known(k, new_nodes)).collect();
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::Con {
                tag: *tag,
                fields: fi,
            });
            ni
        }
    }
}

/// Emit a residual case expression when the scrutinee is unknown.
fn emit_residual_case(
    expr: &CoreExpr,
    scrut_idx: usize,
    binder: &VarId,
    alts: &[Alt<usize>],
    env: &PartialEnv,
    new_nodes: &mut Vec<CoreFrame<usize>>,
) -> (usize, PartialValue) {
    let mut new_env = env.clone();
    new_env.insert(*binder, PartialValue::Unknown);
    let new_alts: Vec<_> = alts
        .iter()
        .map(|alt| {
            let mut alt_env = new_env.clone();
            for b in &alt.binders {
                alt_env.insert(*b, PartialValue::Unknown);
            }
            let (bi, _) = partial_eval_at(expr, alt.body, &alt_env, new_nodes);
            Alt {
                con: alt.con.clone(),
                binders: alt.binders.clone(),
                body: bi,
            }
        })
        .collect();
    let ni = new_nodes.len();
    new_nodes.push(CoreFrame::Case {
        scrutinee: scrut_idx,
        binder: *binder,
        alts: new_alts,
    });
    (ni, PartialValue::Unknown)
}

/// Try to evaluate a primitive operation on partially known arguments.
fn try_eval_primop(op: PrimOpKind, args: &[PartialValue]) -> Option<Literal> {
    let lits: Vec<&Literal> = args
        .iter()
        .filter_map(|a| match a {
            PartialValue::Known(KnownValue::Lit(l)) => Some(l),
            _ => None,
        })
        .collect();
    if lits.len() != args.len() {
        return None;
    }
    match op {
        PrimOpKind::IntAdd => {
            if let [Literal::LitInt(a), Literal::LitInt(b)] = &lits[..] {
                Some(Literal::LitInt(a.wrapping_add(*b)))
            } else {
                None
            }
        }
        PrimOpKind::IntSub => {
            if let [Literal::LitInt(a), Literal::LitInt(b)] = &lits[..] {
                Some(Literal::LitInt(a.wrapping_sub(*b)))
            } else {
                None
            }
        }
        PrimOpKind::IntMul => {
            if let [Literal::LitInt(a), Literal::LitInt(b)] = &lits[..] {
                Some(Literal::LitInt(a.wrapping_mul(*b)))
            } else {
                None
            }
        }
        PrimOpKind::IntNegate => {
            if let [Literal::LitInt(a)] = &lits[..] {
                Some(Literal::LitInt(a.wrapping_neg()))
            } else {
                None
            }
        }
        PrimOpKind::IntEq => int_cmp(&lits, |a, b| a == b),
        PrimOpKind::IntNe => int_cmp(&lits, |a, b| a != b),
        PrimOpKind::IntLt => int_cmp(&lits, |a, b| a < b),
        PrimOpKind::IntLe => int_cmp(&lits, |a, b| a <= b),
        PrimOpKind::IntGt => int_cmp(&lits, |a, b| a > b),
        PrimOpKind::IntGe => int_cmp(&lits, |a, b| a >= b),
        _ => None,
    }
}

/// Helper for integer comparison primops.
fn int_cmp(lits: &[&Literal], f: impl Fn(i64, i64) -> bool) -> Option<Literal> {
    if let [Literal::LitInt(a), Literal::LitInt(b)] = lits {
        Some(Literal::LitInt(if f(*a, *b) { 1 } else { 0 }))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_eval::env::Env;
    use tidepool_eval::eval;
    use tidepool_eval::heap::VecHeap;
    use tidepool_eval::value::Value;
    use tidepool_repr::{Alt, AltCon, CoreFrame, DataConId, Literal, PrimOpKind, VarId};

    #[test]
    fn test_partial_all_known() {
        // let x = 1 in let y = 2 in PrimOp(IntAdd, [x, y])
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Lit(Literal::LitInt(2)), // 1
            CoreFrame::Var(VarId(1)),           // 2: x
            CoreFrame::Var(VarId(2)),           // 3: y
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![2, 3],
            }, // 4
            CoreFrame::LetNonRec {
                binder: VarId(2),
                rhs: 1,
                body: 4,
            }, // 5
            CoreFrame::LetNonRec {
                binder: VarId(1),
                rhs: 0,
                body: 5,
            }, // 6
        ];
        let mut expr = CoreExpr { nodes };
        let pass = PartialEval;
        pass.run(&mut expr);

        assert_eq!(expr.nodes.len(), 1);
        assert_eq!(expr.nodes[0], CoreFrame::Lit(Literal::LitInt(3)));
    }

    #[test]
    fn test_partial_all_unknown() {
        // Var(VarId(1))
        let nodes = vec![CoreFrame::Var(VarId(1))];
        let mut expr = CoreExpr { nodes };
        let pass = PartialEval;
        let changed = pass.run(&mut expr);

        assert!(!changed);
        assert_eq!(expr.nodes.len(), 1);
        assert_eq!(expr.nodes[0], CoreFrame::Var(VarId(1)));
    }

    #[test]
    fn test_partial_case_known_con() {
        // let x = Con(1, [Lit(42)]) in case x of w { DataAlt(1) [y] -> y }
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(42)), // 0
            CoreFrame::Con {
                tag: DataConId(1),
                fields: vec![0],
            }, // 1
            CoreFrame::Var(VarId(2)),            // 2: y
            CoreFrame::Case {
                scrutinee: 1,
                binder: VarId(3),
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(1)),
                    binders: vec![VarId(2)],
                    body: 2,
                }],
            }, // 3
            CoreFrame::LetNonRec {
                binder: VarId(1),
                rhs: 1,
                body: 3,
            }, // 4
        ];
        let mut expr = CoreExpr { nodes };
        let pass = PartialEval;
        pass.run(&mut expr);

        assert_eq!(expr.nodes.len(), 1);
        assert_eq!(expr.nodes[0], CoreFrame::Lit(Literal::LitInt(42)));
    }

    #[test]
    fn test_partial_unknown_scrutinee() {
        // case Var(x) of w { Default -> Lit(42) }
        let nodes = vec![
            CoreFrame::Var(VarId(1)),            // 0
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
        let pass = PartialEval;
        let changed = pass.run(&mut expr);

        // It might "change" by rebuilding the nodes but semantically it's residual
        // Actually our run implementation returns true if new_expr != *expr.
        // Let's check the structure.
        if changed {
            assert!(matches!(expr.nodes.last().unwrap(), CoreFrame::Case { .. }));
        }
    }

    #[test]
    fn test_partial_primop_fold() {
        // PrimOp(IntAdd, [Lit(1), Lit(2)])
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Lit(Literal::LitInt(2)), // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2
        ];
        let mut expr = CoreExpr { nodes };
        let pass = PartialEval;
        pass.run(&mut expr);

        assert_eq!(expr.nodes.len(), 1);
        assert_eq!(expr.nodes[0], CoreFrame::Lit(Literal::LitInt(3)));
    }

    #[test]
    fn test_partial_primop_unknown_arg() {
        // PrimOp(IntAdd, [Lit(1), Var(x)])
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Var(VarId(1)),           // 1
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 1],
            }, // 2
        ];
        let mut expr = CoreExpr { nodes };
        let pass = PartialEval;
        pass.run(&mut expr);

        assert!(matches!(
            expr.nodes.last().unwrap(),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                ..
            }
        ));
    }

    #[test]
    fn test_partial_preserves_eval() {
        // let x = 10 in let y = 20 in PrimOp(IntAdd, [x, y])
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(10)), // 0
            CoreFrame::Lit(Literal::LitInt(20)), // 1
            CoreFrame::Var(VarId(1)),            // 2
            CoreFrame::Var(VarId(2)),            // 3
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![2, 3],
            }, // 4
            CoreFrame::LetNonRec {
                binder: VarId(2),
                rhs: 1,
                body: 4,
            }, // 5
            CoreFrame::LetNonRec {
                binder: VarId(1),
                rhs: 0,
                body: 5,
            }, // 6
        ];
        let mut expr = CoreExpr { nodes };

        let mut heap_before = VecHeap::new();
        let val_before = eval(&expr, &Env::new(), &mut heap_before).unwrap();

        let pass = PartialEval;
        pass.run(&mut expr);

        let mut heap_after = VecHeap::new();
        let val_after = eval(&expr, &Env::new(), &mut heap_after).unwrap();

        let (Value::Lit(Literal::LitInt(n1)), Value::Lit(Literal::LitInt(n2))) =
            (val_before, val_after)
        else {
            panic!("Expected LitInt(30)");
        };
        assert_eq!(n1, 30);
        assert_eq!(n2, 30);
    }

    #[test]
    fn test_partial_nested_let() {
        // let x = 1 in let y = PrimOp(IntAdd, [x, Lit(2)]) in PrimOp(IntAdd, [y, Lit(3)])
        let nodes = vec![
            CoreFrame::Lit(Literal::LitInt(1)), // 0
            CoreFrame::Var(VarId(1)),           // 1: x
            CoreFrame::Lit(Literal::LitInt(2)), // 2
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![1, 2],
            }, // 3: x + 2
            CoreFrame::Var(VarId(2)),           // 4: y
            CoreFrame::Lit(Literal::LitInt(3)), // 5
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![4, 5],
            }, // 6: y + 3
            CoreFrame::LetNonRec {
                binder: VarId(2),
                rhs: 3,
                body: 6,
            }, // 7
            CoreFrame::LetNonRec {
                binder: VarId(1),
                rhs: 0,
                body: 7,
            }, // 8
        ];
        let mut expr = CoreExpr { nodes };
        let pass = PartialEval;
        pass.run(&mut expr);

        assert_eq!(expr.nodes.len(), 1);
        assert_eq!(expr.nodes[0], CoreFrame::Lit(Literal::LitInt(6)));
    }
}
