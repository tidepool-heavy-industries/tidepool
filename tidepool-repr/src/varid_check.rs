//! Load-time duplicate-VarId detection for the top-level Let spine.
//!
//! Defends against the #313 bug class: two DISTINCT top-level bindings
//! (e.g. simplifier floats from different modules) receiving the same VarId
//! from the Haskell serializer's hash-based scheme. The deserialized program
//! then silently shadows one binding with the other, and references resolve
//! to the wrong RHS — in #313, `run`'s continuation was resumed through the
//! wrong `k_X1`, casing a raw `(Int,Text,Text)` tuple as a list (CASE TRAP).
//!
//! The serializer (`Translate.wrapAllBinds`) emits top-level bindings as a
//! Let-nest: `LetNonRec`/`LetRec` frames chained through their `body` edge,
//! terminating in the target expression. Every binder on that spine is a
//! distinct GHC top-level binding, so a duplicate VarId there is ALWAYS an
//! identifier collision, never legitimate shadowing. Nested binders
//! (lambdas, local lets inside RHSs) are never visited — shadowing is legal
//! there and this check ignores it by construction.

use crate::frame::CoreFrame;
use crate::types::VarId;
use crate::CoreExpr;
use std::collections::HashMap;

/// A binding site on the top-level Let spine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingSite {
    /// Index of the Let frame in the tree's node vector.
    pub node: usize,
    /// Position of the binder within the frame: always 0 for `LetNonRec`,
    /// the index into `bindings` for `LetRec`.
    pub position: usize,
}

impl std::fmt::Display for BindingSite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "node {} binder #{}", self.node, self.position)
    }
}

/// A duplicate VarId across two distinct top-level binding sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "duplicate top-level VarId {:#018x} bound at {first} and at {second}: \
     two distinct top-level bindings share one identifier, so one silently \
     shadows the other and references resolve to the wrong RHS (the #313 \
     bug class — see GhcPipeline.externalizeInternalTops)",
    .var_id.0
)]
pub struct VarIdCollision {
    /// The colliding identifier.
    pub var_id: VarId,
    /// The binding site encountered first (closer to the root).
    pub first: BindingSite,
    /// The binding site encountered second.
    pub second: BindingSite,
}

/// Walk the top-level Let spine of a deserialized program and error on the
/// first VarId bound at two distinct binding sites.
///
/// Returns the number of top-level binders inspected. O(spine length) with
/// a single hash map. Does not descend into RHSs, so nested binders may
/// legally shadow top-level ones without triggering.
pub fn check_toplevel_varids(expr: &CoreExpr) -> Result<usize, VarIdCollision> {
    let mut seen: HashMap<VarId, BindingSite> = HashMap::new();
    let mut count = 0usize;
    for (node, position, var_id) in toplevel_binders(expr) {
        count += 1;
        let site = BindingSite { node, position };
        if let Some(&first) = seen.get(&var_id) {
            // Identical site can only recur on a malformed (cyclic) spine,
            // which the step guard in `toplevel_binders` truncates; a binder
            // appearing once is never a collision.
            if first != site {
                return Err(VarIdCollision {
                    var_id,
                    first,
                    second: site,
                });
            }
        } else {
            seen.insert(var_id, site);
        }
    }
    Ok(count)
}

/// Collect `(node index, binder position, VarId)` for every binder on the
/// top-level Let spine: follow `body` edges from the root through
/// `LetNonRec`/`LetRec` frames, stopping at the first frame of any other
/// kind (the `wrapAllBinds` target expression).
pub fn toplevel_binders(expr: &CoreExpr) -> Vec<(usize, usize, VarId)> {
    let mut out = Vec::new();
    if expr.nodes.is_empty() {
        return out;
    }
    // `read_cbor` guarantees the root is the last node.
    let mut idx = expr.nodes.len() - 1;
    // A well-formed spine visits each frame at most once; the step guard
    // terminates on malformed (cyclic) `body` edges.
    let mut steps = 0usize;
    loop {
        steps += 1;
        if steps > expr.nodes.len() {
            break;
        }
        match &expr.nodes[idx] {
            CoreFrame::LetNonRec { binder, body, .. } => {
                out.push((idx, 0, *binder));
                idx = *body;
            }
            CoreFrame::LetRec { bindings, body } => {
                for (pos, (binder, _)) in bindings.iter().enumerate() {
                    out.push((idx, pos, *binder));
                }
                idx = *body;
            }
            _ => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::RecursiveTree;
    use crate::types::Literal;

    /// let v1 = 1 in let v2 = 2 in v1  — clean two-binder spine.
    fn clean_spine() -> CoreExpr {
        RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(1)), // 0
                CoreFrame::Lit(Literal::LitInt(2)), // 1
                CoreFrame::Var(VarId(1)),           // 2
                CoreFrame::LetNonRec {
                    binder: VarId(2),
                    rhs: 1,
                    body: 2,
                }, // 3
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 0,
                    body: 3,
                }, // 4 (root)
            ],
        }
    }

    #[test]
    fn clean_spine_passes() {
        assert_eq!(check_toplevel_varids(&clean_spine()), Ok(2));
    }

    #[test]
    fn single_binder_passes() {
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(1)),
                CoreFrame::Var(VarId(7)),
                CoreFrame::LetNonRec {
                    binder: VarId(7),
                    rhs: 0,
                    body: 1,
                },
            ],
        };
        assert_eq!(check_toplevel_varids(&expr), Ok(1));
    }

    #[test]
    fn non_let_root_is_empty_spine() {
        let expr = RecursiveTree {
            nodes: vec![CoreFrame::Lit(Literal::LitInt(42))],
        };
        assert_eq!(check_toplevel_varids(&expr), Ok(0));
    }

    #[test]
    fn duplicate_across_letnonrec_sites_errors() {
        let mut expr = clean_spine();
        // Clone the outer binder's VarId onto the inner site.
        if let CoreFrame::LetNonRec { binder, .. } = &mut expr.nodes[3] {
            *binder = VarId(1);
        }
        let err = check_toplevel_varids(&expr).unwrap_err();
        assert_eq!(err.var_id, VarId(1));
        assert_eq!(
            err.first,
            BindingSite {
                node: 4,
                position: 0
            }
        );
        assert_eq!(
            err.second,
            BindingSite {
                node: 3,
                position: 0
            }
        );
    }

    #[test]
    fn duplicate_within_letrec_group_errors() {
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(1)), // 0
                CoreFrame::Lit(Literal::LitInt(2)), // 1
                CoreFrame::Var(VarId(5)),           // 2
                CoreFrame::LetRec {
                    bindings: vec![(VarId(5), 0), (VarId(5), 1)],
                    body: 2,
                }, // 3 (root)
            ],
        };
        let err = check_toplevel_varids(&expr).unwrap_err();
        assert_eq!(err.var_id, VarId(5));
        assert_eq!(
            err.first,
            BindingSite {
                node: 3,
                position: 0
            }
        );
        assert_eq!(
            err.second,
            BindingSite {
                node: 3,
                position: 1
            }
        );
    }

    #[test]
    fn duplicate_across_letrec_and_letnonrec_errors() {
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(1)), // 0
                CoreFrame::Lit(Literal::LitInt(2)), // 1
                CoreFrame::Var(VarId(9)),           // 2
                CoreFrame::LetRec {
                    bindings: vec![(VarId(9), 1)],
                    body: 2,
                }, // 3
                CoreFrame::LetNonRec {
                    binder: VarId(9),
                    rhs: 0,
                    body: 3,
                }, // 4 (root)
            ],
        };
        let err = check_toplevel_varids(&expr).unwrap_err();
        assert_eq!(err.var_id, VarId(9));
        assert_eq!(
            err.first,
            BindingSite {
                node: 4,
                position: 0
            }
        );
        assert_eq!(
            err.second,
            BindingSite {
                node: 3,
                position: 0
            }
        );
    }

    #[test]
    fn nested_shadowing_is_ignored() {
        // let v1 = 1 in (\v1 -> let v1 = 2 in v1) — the lambda binder and the
        // let UNDER the lambda both reuse VarId(1); neither is on the spine.
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(1)), // 0
                CoreFrame::Lit(Literal::LitInt(2)), // 1
                CoreFrame::Var(VarId(1)),           // 2
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 1,
                    body: 2,
                }, // 3 (nested let, under the lambda)
                CoreFrame::Lam {
                    binder: VarId(1),
                    body: 3,
                }, // 4
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 0,
                    body: 4,
                }, // 5 (root): body is a Lam → spine stops there
            ],
        };
        assert_eq!(check_toplevel_varids(&expr), Ok(1));
    }

    #[test]
    fn shadowing_in_rhs_is_ignored() {
        // let v1 = (let v1 = 2 in v1) in let v2 = 3 in v1 — the inner let
        // lives in an RHS, not on the spine.
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(2)), // 0
                CoreFrame::Var(VarId(1)),           // 1
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 0,
                    body: 1,
                }, // 2 (RHS-internal let)
                CoreFrame::Lit(Literal::LitInt(3)), // 3
                CoreFrame::Var(VarId(1)),           // 4
                CoreFrame::LetNonRec {
                    binder: VarId(2),
                    rhs: 3,
                    body: 4,
                }, // 5
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 2,
                    body: 5,
                }, // 6 (root)
            ],
        };
        assert_eq!(check_toplevel_varids(&expr), Ok(2));
    }

    #[test]
    fn cyclic_spine_terminates() {
        // Malformed: root's body edge points back at itself. The step guard
        // must terminate, and the identical site must not count as collision.
        let expr = RecursiveTree {
            nodes: vec![
                CoreFrame::Lit(Literal::LitInt(1)), // 0
                CoreFrame::LetNonRec {
                    binder: VarId(1),
                    rhs: 0,
                    body: 1,
                }, // 1 (root, cycles to itself)
            ],
        };
        // Must not hang; same-site repeats are not collisions.
        assert!(check_toplevel_varids(&expr).is_ok());
    }

    #[test]
    fn error_message_names_both_sites() {
        let mut expr = clean_spine();
        if let CoreFrame::LetNonRec { binder, .. } = &mut expr.nodes[3] {
            *binder = VarId(1);
        }
        let msg = check_toplevel_varids(&expr).unwrap_err().to_string();
        assert!(msg.contains("0x0000000000000001"), "msg: {msg}");
        assert!(msg.contains("node 4 binder #0"), "msg: {msg}");
        assert!(msg.contains("node 3 binder #0"), "msg: {msg}");
        assert!(msg.contains("#313"), "msg: {msg}");
    }
}
