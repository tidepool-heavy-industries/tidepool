//! Partial evaluation pass for Core expressions.

use crate::{Changed, Pass};
use rustc_hash::FxHashMap;
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

/// The result of partially evaluating one subtree: the index of the emitted
/// node in the caller's rebuilt `new_nodes` buffer, paired with the abstract
/// value that flows upward. A named struct (not a bare `(usize, PartialValue)`
/// tuple) so call sites read `.idx` / `.value` instead of `.0` / `.1`.
struct Residual {
    /// Index of the emitted node in the caller's `new_nodes` buffer.
    idx: usize,
    /// The abstract (Known/Unknown) value produced by the subtree.
    value: PartialValue,
}

/// First-order partial evaluation pass.
pub struct PartialEval;

impl Pass for PartialEval {
    fn run(&self, expr: &mut CoreExpr) -> Changed {
        crate::apply_rewrite(expr, |e| {
            let mut new_nodes = Vec::new();
            let root =
                partial_eval_at(e, e.nodes.len() - 1, &PartialEnv::default(), &mut new_nodes);
            let new_expr = CoreExpr { nodes: new_nodes }.extract_subtree(root.idx);
            // Unlike the redex-finding passes, PartialEval always produces a
            // rebuilt tree; signal Changed only when it actually differs.
            (new_expr != *e).then_some(new_expr)
        })
    }
    fn name(&self) -> &str {
        "PartialEval"
    }
}

/// Recursively partially evaluate an expression at a given index.
///
/// NOTE (stack-safety): left native-recursive, like `tidepool_repr::subst`. It
/// threads a `PartialEnv` that differs per path (bindings accumulate as the walk
/// descends, and the Known/Unknown abstract value flows down), so post-order
/// index memoization — the basis of the converted `extract_subtree`/`free_vars`
/// walks — is unsound here. It also fuses control flow (a Known let/case
/// short-circuits straight to a sub-result, never emitting the binder). A
/// faithful explicit-stack conversion would need to re-pair per-frame env
/// state and these short-circuits by construction; a broken conversion would
/// fail SILENTLY (miswritten residual code), not crash. PartialEval is
/// optimize-only (off the JIT compile path), so its residual recursion depth
/// is unreachable from production eval.
fn partial_eval_at(
    expr: &CoreExpr,
    idx: usize,
    env: &PartialEnv,
    new_nodes: &mut Vec<CoreFrame<usize>>,
) -> Residual {
    match &expr.nodes[idx] {
        CoreFrame::Var(v) => match env.get(v) {
            Some(PartialValue::Known(kv)) => {
                let ni = emit_known(kv, new_nodes);
                Residual {
                    idx: ni,
                    value: PartialValue::Known(kv.clone()),
                }
            }
            _ => {
                let ni = new_nodes.len();
                new_nodes.push(CoreFrame::Var(*v));
                Residual {
                    idx: ni,
                    value: PartialValue::Unknown,
                }
            }
        },
        CoreFrame::Lit(lit) => {
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::Lit(lit.clone()));
            Residual {
                idx: ni,
                value: PartialValue::Known(KnownValue::Lit(lit.clone())),
            }
        }
        CoreFrame::Con { tag, fields } => {
            let (fi, fv): (Vec<_>, Vec<_>) = fields
                .iter()
                .map(|&f| {
                    let r = partial_eval_at(expr, f, env, new_nodes);
                    (r.idx, r.value)
                })
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

            let value = if let Some(kf) = known_fields {
                PartialValue::Known(KnownValue::Con(*tag, kf))
            } else {
                PartialValue::Unknown
            };
            Residual { idx: ni, value }
        }
        CoreFrame::LetNonRec { binder, rhs, body } => {
            let rhs_r = partial_eval_at(expr, *rhs, env, new_nodes);
            let mut new_env = env.clone();
            new_env.insert(*binder, rhs_r.value.clone());
            if matches!(rhs_r.value, PartialValue::Known(_)) {
                // Known RHS: evaluate body with known binder, skip the let
                partial_eval_at(expr, *body, &new_env, new_nodes)
            } else {
                let body_r = partial_eval_at(expr, *body, &new_env, new_nodes);
                let ni = new_nodes.len();
                new_nodes.push(CoreFrame::LetNonRec {
                    binder: *binder,
                    rhs: rhs_r.idx,
                    body: body_r.idx,
                });
                Residual {
                    idx: ni,
                    value: body_r.value,
                }
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
                    let ri = partial_eval_at(expr, *r, &new_env, new_nodes).idx;
                    (*b, ri)
                })
                .collect();
            let body_r = partial_eval_at(expr, *body, &new_env, new_nodes);
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::LetRec {
                bindings: nb,
                body: body_r.idx,
            });
            Residual {
                idx: ni,
                value: body_r.value,
            }
        }
        CoreFrame::Case {
            scrutinee,
            binder,
            alts,
        } => {
            let scrut = partial_eval_at(expr, *scrutinee, env, new_nodes);
            match &scrut.value {
                PartialValue::Known(KnownValue::Con(tag, field_vals)) => {
                    let matched = alts
                        .iter()
                        .find(|a| matches!(&a.con, AltCon::DataAlt(t) if t == tag))
                        .or_else(|| alts.iter().find(|a| matches!(&a.con, AltCon::Default)));
                    if let Some(alt) = matched {
                        let mut new_env = env.clone();
                        new_env.insert(*binder, scrut.value.clone());
                        if let AltCon::DataAlt(_) = &alt.con {
                            for (b, fv) in alt.binders.iter().zip(field_vals.iter()) {
                                new_env.insert(*b, PartialValue::Known(fv.clone()));
                            }
                        }
                        partial_eval_at(expr, alt.body, &new_env, new_nodes)
                    } else {
                        emit_residual_case(expr, scrut.idx, binder, alts, env, new_nodes)
                    }
                }
                PartialValue::Known(KnownValue::Lit(lit)) => {
                    let matched = alts
                        .iter()
                        .find(|a| matches!(&a.con, AltCon::LitAlt(l) if l == lit))
                        .or_else(|| alts.iter().find(|a| matches!(&a.con, AltCon::Default)));
                    if let Some(alt) = matched {
                        let mut new_env = env.clone();
                        new_env.insert(*binder, scrut.value.clone());
                        partial_eval_at(expr, alt.body, &new_env, new_nodes)
                    } else {
                        emit_residual_case(expr, scrut.idx, binder, alts, env, new_nodes)
                    }
                }
                PartialValue::Unknown => {
                    emit_residual_case(expr, scrut.idx, binder, alts, env, new_nodes)
                }
            }
        }
        CoreFrame::PrimOp { op, args } => {
            let (ai, av): (Vec<_>, Vec<_>) = args
                .iter()
                .map(|&a| {
                    let r = partial_eval_at(expr, a, env, new_nodes);
                    (r.idx, r.value)
                })
                .unzip();
            if let Some(result) = try_eval_primop(*op, &av) {
                let ni = new_nodes.len();
                new_nodes.push(CoreFrame::Lit(result.clone()));
                Residual {
                    idx: ni,
                    value: PartialValue::Known(KnownValue::Lit(result)),
                }
            } else {
                let ni = new_nodes.len();
                new_nodes.push(CoreFrame::PrimOp { op: *op, args: ai });
                Residual {
                    idx: ni,
                    value: PartialValue::Unknown,
                }
            }
        }
        CoreFrame::App { fun, arg } => {
            let fi = partial_eval_at(expr, *fun, env, new_nodes).idx;
            let ai = partial_eval_at(expr, *arg, env, new_nodes).idx;
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::App { fun: fi, arg: ai });
            Residual {
                idx: ni,
                value: PartialValue::Unknown,
            }
        }
        CoreFrame::Lam { binder, body } => {
            let mut new_env = env.clone();
            new_env.insert(*binder, PartialValue::Unknown);
            let bi = partial_eval_at(expr, *body, &new_env, new_nodes).idx;
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::Lam {
                binder: *binder,
                body: bi,
            });
            Residual {
                idx: ni,
                value: PartialValue::Unknown,
            }
        }
        CoreFrame::Join {
            label,
            params,
            rhs,
            body,
        } => {
            let mut rhs_env = env.clone();
            for p in params {
                rhs_env.insert(*p, PartialValue::Unknown);
            }
            let ri = partial_eval_at(expr, *rhs, &rhs_env, new_nodes).idx;
            let body_r = partial_eval_at(expr, *body, env, new_nodes);
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::Join {
                label: *label,
                params: params.clone(),
                rhs: ri,
                body: body_r.idx,
            });
            Residual {
                idx: ni,
                value: body_r.value,
            }
        }
        CoreFrame::Jump { label, args } => {
            let ai: Vec<_> = args
                .iter()
                .map(|&a| partial_eval_at(expr, a, env, new_nodes).idx)
                .collect();
            let ni = new_nodes.len();
            new_nodes.push(CoreFrame::Jump {
                label: *label,
                args: ai,
            });
            Residual {
                idx: ni,
                value: PartialValue::Unknown,
            }
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
) -> Residual {
    let mut new_env = env.clone();
    new_env.insert(*binder, PartialValue::Unknown);
    let new_alts: Vec<_> = alts
        .iter()
        .map(|alt| {
            let mut alt_env = new_env.clone();
            for b in &alt.binders {
                alt_env.insert(*b, PartialValue::Unknown);
            }
            let bi = partial_eval_at(expr, alt.body, &alt_env, new_nodes).idx;
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
    Residual {
        idx: ni,
        value: PartialValue::Unknown,
    }
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
