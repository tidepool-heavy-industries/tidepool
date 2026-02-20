use crate::{CoreExpr, CoreFrame, VarId};
use std::collections::HashMap;

/// Check if two expressions are alpha-equivalent.
pub fn alpha_eq(lhs: &CoreExpr, rhs: &CoreExpr) -> bool {
    if lhs.nodes.is_empty() && rhs.nodes.is_empty() {
        return true;
    }
    if lhs.nodes.is_empty() || rhs.nodes.is_empty() {
        return false;
    }
    let mut next_canon = 0u64;
    alpha_eq_at(
        lhs,
        rhs,
        lhs.nodes.len() - 1,
        rhs.nodes.len() - 1,
        &mut HashMap::new(),
        &mut HashMap::new(),
        &mut next_canon,
    )
}

/// Recursive helper. `env_l` maps lhs bound vars to canonical ids, `env_r` maps rhs bound vars to canonical ids.
fn alpha_eq_at(
    lhs: &CoreExpr,
    rhs: &CoreExpr,
    li: usize,
    ri: usize,
    env_l: &mut HashMap<VarId, u64>,
    env_r: &mut HashMap<VarId, u64>,
    next_canon: &mut u64,
) -> bool {
    match (&lhs.nodes[li], &rhs.nodes[ri]) {
        (CoreFrame::Var(lv), CoreFrame::Var(rv)) => match (env_l.get(lv), env_r.get(rv)) {
            (Some(lc), Some(rc)) => lc == rc,
            (None, None) => lv == rv,
            _ => false,
        },
        (CoreFrame::Lit(ll), CoreFrame::Lit(rl)) => ll == rl,
        (CoreFrame::App { fun: lf, arg: la }, CoreFrame::App { fun: rf, arg: ra }) => {
            alpha_eq_at(lhs, rhs, *lf, *rf, env_l, env_r, next_canon)
                && alpha_eq_at(lhs, rhs, *la, *ra, env_l, env_r, next_canon)
        }
        (
            CoreFrame::Lam {
                binder: lb,
                body: lbody,
            },
            CoreFrame::Lam {
                binder: rb,
                body: rbody,
            },
        ) => {
            let canon = *next_canon;
            *next_canon += 1;
            let old_l = env_l.insert(*lb, canon);
            let old_r = env_r.insert(*rb, canon);
            let res = alpha_eq_at(lhs, rhs, *lbody, *rbody, env_l, env_r, next_canon);
            if let Some(o) = old_l {
                env_l.insert(*lb, o);
            } else {
                env_l.remove(lb);
            }
            if let Some(o) = old_r {
                env_r.insert(*rb, o);
            } else {
                env_r.remove(rb);
            }
            res
        }
        (
            CoreFrame::LetNonRec {
                binder: lb,
                rhs: lrhs,
                body: lbody,
            },
            CoreFrame::LetNonRec {
                binder: rb,
                rhs: rrhs,
                body: rbody,
            },
        ) => {
            if !alpha_eq_at(lhs, rhs, *lrhs, *rrhs, env_l, env_r, next_canon) {
                return false;
            }
            let canon = *next_canon;
            *next_canon += 1;
            let old_l = env_l.insert(*lb, canon);
            let old_r = env_r.insert(*rb, canon);
            let res = alpha_eq_at(lhs, rhs, *lbody, *rbody, env_l, env_r, next_canon);
            if let Some(o) = old_l {
                env_l.insert(*lb, o);
            } else {
                env_l.remove(lb);
            }
            if let Some(o) = old_r {
                env_r.insert(*rb, o);
            } else {
                env_r.remove(rb);
            }
            res
        }
        (
            CoreFrame::LetRec {
                bindings: lbs,
                body: lbody,
            },
            CoreFrame::LetRec {
                bindings: rbs,
                body: rbody,
            },
        ) => {
            if lbs.len() != rbs.len() {
                return false;
            }
            let mut old_ls = Vec::new();
            let mut old_rs = Vec::new();
            for ((lb, _), (rb, _)) in lbs.iter().zip(rbs.iter()) {
                let canon = *next_canon;
                *next_canon += 1;
                old_ls.push(env_l.insert(*lb, canon));
                old_rs.push(env_r.insert(*rb, canon));
            }

            let mut ok = true;
            for ((_, lr), (_, rr)) in lbs.iter().zip(rbs.iter()) {
                if !alpha_eq_at(lhs, rhs, *lr, *rr, env_l, env_r, next_canon) {
                    ok = false;
                    break;
                }
            }
            if ok {
                ok = alpha_eq_at(lhs, rhs, *lbody, *rbody, env_l, env_r, next_canon);
            }

            // Restore env
            for ((lb, _), old) in lbs.iter().zip(old_ls) {
                if let Some(o) = old {
                    env_l.insert(*lb, o);
                } else {
                    env_l.remove(lb);
                }
            }
            for ((rb, _), old) in rbs.iter().zip(old_rs) {
                if let Some(o) = old {
                    env_r.insert(*rb, o);
                } else {
                    env_r.remove(rb);
                }
            }
            ok
        }
        (
            CoreFrame::Case {
                scrutinee: ls,
                binder: lb,
                alts: lalts,
            },
            CoreFrame::Case {
                scrutinee: rs,
                binder: rb,
                alts: ralts,
            },
        ) => {
            if !alpha_eq_at(lhs, rhs, *ls, *rs, env_l, env_r, next_canon) {
                return false;
            }
            if lalts.len() != ralts.len() {
                return false;
            }

            let canon = *next_canon;
            *next_canon += 1;
            let old_lb = env_l.insert(*lb, canon);
            let old_rb = env_r.insert(*rb, canon);

            let mut ok = true;
            for (lalt, ralt) in lalts.iter().zip(ralts.iter()) {
                if lalt.con != ralt.con || lalt.binders.len() != ralt.binders.len() {
                    ok = false;
                    break;
                }
                let mut old_alt_ls = Vec::new();
                let mut old_alt_rs = Vec::new();
                for (lab, rab) in lalt.binders.iter().zip(ralt.binders.iter()) {
                    let c = *next_canon;
                    *next_canon += 1;
                    old_alt_ls.push(env_l.insert(*lab, c));
                    old_alt_rs.push(env_r.insert(*rab, c));
                }

                if !alpha_eq_at(lhs, rhs, lalt.body, ralt.body, env_l, env_r, next_canon) {
                    ok = false;
                }

                // Restore alt env
                for (lab, old) in lalt.binders.iter().zip(old_alt_ls) {
                    if let Some(o) = old {
                        env_l.insert(*lab, o);
                    } else {
                        env_l.remove(lab);
                    }
                }
                for (rab, old) in ralt.binders.iter().zip(old_alt_rs) {
                    if let Some(o) = old {
                        env_r.insert(*rab, o);
                    } else {
                        env_r.remove(rab);
                    }
                }

                if !ok {
                    break;
                }
            }

            // Restore case binder
            if let Some(o) = old_lb {
                env_l.insert(*lb, o);
            } else {
                env_l.remove(lb);
            }
            if let Some(o) = old_rb {
                env_r.insert(*rb, o);
            } else {
                env_r.remove(rb);
            }
            ok
        }
        (
            CoreFrame::Con {
                tag: lt,
                fields: lf,
            },
            CoreFrame::Con {
                tag: rt,
                fields: rf,
            },
        ) => {
            if lt != rt || lf.len() != rf.len() {
                return false;
            }
            for (l, r) in lf.iter().zip(rf.iter()) {
                if !alpha_eq_at(lhs, rhs, *l, *r, env_l, env_r, next_canon) {
                    return false;
                }
            }
            true
        }
        (
            CoreFrame::Join {
                label: ll,
                params: lp,
                rhs: lr,
                body: lbody,
            },
            CoreFrame::Join {
                label: rl,
                params: rp,
                rhs: rr,
                body: rbody,
            },
        ) => {
            // Join labels must match exactly if not tracked by env (spec says JoinId is not VarId)
            if ll != rl || lp.len() != rp.len() {
                return false;
            }

            let mut old_lp = Vec::new();
            let mut old_rp = Vec::new();
            for (p_l, p_r) in lp.iter().zip(rp.iter()) {
                let canon = *next_canon;
                *next_canon += 1;
                old_lp.push(env_l.insert(*p_l, canon));
                old_rp.push(env_r.insert(*p_r, canon));
            }

            let res_rhs = alpha_eq_at(lhs, rhs, *lr, *rr, env_l, env_r, next_canon);

            // Restore lp env
            for (p_l, old) in lp.iter().zip(old_lp) {
                if let Some(o) = old {
                    env_l.insert(*p_l, o);
                } else {
                    env_l.remove(p_l);
                }
            }
            for (p_r, old) in rp.iter().zip(old_rp) {
                if let Some(o) = old {
                    env_r.insert(*p_r, o);
                } else {
                    env_r.remove(p_r);
                }
            }

            if !res_rhs {
                return false;
            }

            alpha_eq_at(lhs, rhs, *lbody, *rbody, env_l, env_r, next_canon)
        }
        (
            CoreFrame::Jump {
                label: ll,
                args: la,
            },
            CoreFrame::Jump {
                label: rl,
                args: ra,
            },
        ) => {
            if ll != rl || la.len() != ra.len() {
                return false;
            }
            for (l, r) in la.iter().zip(ra.iter()) {
                if !alpha_eq_at(lhs, rhs, *l, *r, env_l, env_r, next_canon) {
                    return false;
                }
            }
            true
        }
        (CoreFrame::PrimOp { op: lo, args: la }, CoreFrame::PrimOp { op: ro, args: ra }) => {
            if lo != ro || la.len() != ra.len() {
                return false;
            }
            for (l, r) in la.iter().zip(ra.iter()) {
                if !alpha_eq_at(lhs, rhs, *l, *r, env_l, env_r, next_canon) {
                    return false;
                }
            }
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JoinId, RecursiveTree};

    #[test]
    fn test_alpha_eq_lam() {
        let x = VarId(1);
        let y = VarId(2);
        let lhs = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(x),                     // 0
                CoreFrame::Lam { binder: x, body: 0 }, // 1
            ],
        };
        let rhs = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(y),                     // 0
                CoreFrame::Lam { binder: y, body: 0 }, // 1
            ],
        };
        assert!(alpha_eq(&lhs, &rhs));
    }

    #[test]
    fn test_alpha_neq_lam() {
        let x = VarId(1);
        let y = VarId(2);
        let lhs = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(x),                     // 0
                CoreFrame::Lam { binder: x, body: 0 }, // 1
            ],
        };
        let rhs = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(y),                     // 0
                CoreFrame::Lam { binder: x, body: 0 }, // 1 (binder x, body uses y)
            ],
        };
        assert!(!alpha_eq(&lhs, &rhs));
    }

    #[test]
    fn test_alpha_neq_free() {
        let x = VarId(1);
        let y = VarId(2);
        let lhs = RecursiveTree {
            nodes: vec![CoreFrame::Var(x)],
        };
        let rhs = RecursiveTree {
            nodes: vec![CoreFrame::Var(y)],
        };
        assert!(!alpha_eq(&lhs, &rhs));
    }

    #[test]
    fn test_alpha_eq_reflexive() {
        let x = VarId(1);
        let lhs = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(x),                     // 0
                CoreFrame::Lam { binder: x, body: 0 }, // 1
            ],
        };
        assert!(alpha_eq(&lhs, &lhs));
    }

    #[test]
    fn test_alpha_eq_let_rec() {
        let x = VarId(1);
        let y = VarId(2);
        let a = VarId(3);
        let b = VarId(4);
        // LetRec [(x, x)] x
        let lhs = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(x),
                CoreFrame::LetRec {
                    bindings: vec![(x, 0)],
                    body: 0,
                },
            ],
        };
        // LetRec [(y, y)] y
        let rhs = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(y),
                CoreFrame::LetRec {
                    bindings: vec![(y, 0)],
                    body: 0,
                },
            ],
        };
        assert!(alpha_eq(&lhs, &rhs));

        // LetRec [(a, b)] a  != LetRec [(a, a)] a
        let lhs2 = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(b),
                CoreFrame::Var(a),
                CoreFrame::LetRec {
                    bindings: vec![(a, 0)],
                    body: 1,
                },
            ],
        };
        let rhs2 = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(a),
                CoreFrame::LetRec {
                    bindings: vec![(a, 0)],
                    body: 0,
                },
            ],
        };
        assert!(!alpha_eq(&lhs2, &rhs2));
    }

    #[test]
    fn test_alpha_eq_case() {
        let x = VarId(1);
        let y = VarId(2);
        let z = VarId(3);
        // Case(x, y, [Default => y])
        let lhs = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(x),
                CoreFrame::Var(y),
                CoreFrame::Case {
                    scrutinee: 0,
                    binder: y,
                    alts: vec![crate::types::Alt {
                        con: crate::types::AltCon::Default,
                        binders: vec![],
                        body: 1,
                    }],
                },
            ],
        };
        // Case(x, z, [Default => z])
        let rhs = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(x),
                CoreFrame::Var(z),
                CoreFrame::Case {
                    scrutinee: 0,
                    binder: z,
                    alts: vec![crate::types::Alt {
                        con: crate::types::AltCon::Default,
                        binders: vec![],
                        body: 1,
                    }],
                },
            ],
        };
        assert!(alpha_eq(&lhs, &rhs));
    }

    #[test]
    fn test_alpha_eq_join() {
        let x = VarId(1);
        let y = VarId(2);
        // Join(j, [x], x, y)
        let lhs = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(x),
                CoreFrame::Var(y),
                CoreFrame::Join {
                    label: JoinId(1),
                    params: vec![x],
                    rhs: 0,
                    body: 1,
                },
            ],
        };
        // Join(j, [y], y, y)
        let rhs = RecursiveTree {
            nodes: vec![
                CoreFrame::Var(y),
                CoreFrame::Join {
                    label: JoinId(1),
                    params: vec![y],
                    rhs: 0,
                    body: 0,
                },
            ],
        };
        assert!(alpha_eq(&lhs, &rhs));
    }
}
