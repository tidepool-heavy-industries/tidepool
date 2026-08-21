//! Occurrence analysis for Core expressions.

use rustc_hash::FxHashMap;
use tidepool_repr::{get_children, CoreExpr, CoreFrame, VarId};

/// Occurrence count for a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occ {
    /// Variable is not used.
    Dead,
    /// Variable is used exactly once.
    Once,
    /// Variable is used more than once.
    Many,
}

impl Occ {
    /// Combine two occurrence counts using saturated addition.
    ///
    /// This is a domain-specific, capped addition for occurrence analysis:
    /// - `Dead` is the identity element (`Dead + x = x`).
    /// - `Once + Once = Many`.
    /// - `Many` is the saturation point (`x + Many = Many` and `Many + x = Many`).
    ///
    /// We suppress `clippy::should_implement_trait` because this is a
    /// domain-specific saturated addition for occurrence analysis, not
    /// general-purpose numeric addition.
    #[allow(clippy::should_implement_trait)]
    pub fn add(self, other: Occ) -> Occ {
        match (self, other) {
            (Occ::Dead, o) | (o, Occ::Dead) => o,
            _ => Occ::Many,
        }
    }
}

/// Map from variable to occurrence count.
pub type OccMap = FxHashMap<VarId, Occ>;

/// Count occurrences of all variables in the expression.
/// Binding sites (in Lam, Let, Case, Join) are NOT counted as occurrences.
/// Only Var(v) nodes (variable use sites) are counted.
///
/// `CoreExpr` is a DAG, not a tree: a single `Var` node can be reachable via
/// more than one parent edge (or twice from the same parent, e.g. `x + x`
/// sharing one `Var` index in both `PrimOp` args). Counting distinct `Var`
/// frames in the flat node vector — one scan, no notion of "reached from
/// where" — undercounts any such shared node to `Once`. We instead count
/// incoming edges: for every node, walk its children (`get_children`, the
/// same child set the rewrite passes descend through) and charge an
/// occurrence for each child that is a `Var`. A node with two parent edges is
/// then correctly `Many`, matching how many times a rewrite would actually
/// splice a substituted subtree at it.
pub fn occ_analysis(expr: &CoreExpr) -> OccMap {
    let mut map = OccMap::default();
    for node in &expr.nodes {
        for child in get_children(node) {
            if let CoreFrame::Var(v) = &expr.nodes[child] {
                let entry = map.entry(*v).or_insert(Occ::Dead);
                *entry = entry.add(Occ::Once);
            }
        }
    }
    map
}

/// Get the occurrence count for a specific variable, defaulting to Dead.
pub fn get_occ(map: &OccMap, var: VarId) -> Occ {
    map.get(&var).copied().unwrap_or(Occ::Dead)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::{Alt, AltCon, DataConId, Literal, PrimOpKind};

    // Test helpers
    fn tree(nodes: Vec<CoreFrame<usize>>) -> CoreExpr {
        CoreExpr { nodes }
    }

    // 1. let x = 1 in 2 -> x Dead
    #[test]
    fn test_dead_var() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::Lit(Literal::LitInt(2)),
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 1,
            },
        ]);
        let map = occ_analysis(&expr);
        assert_eq!(get_occ(&map, x), Occ::Dead);
    }

    // 2. let x = 1 in x -> x Once
    #[test]
    fn test_once_var() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::Var(x),
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 1,
            },
        ]);
        let map = occ_analysis(&expr);
        assert_eq!(get_occ(&map, x), Occ::Once);
    }

    // 3. let x = 1 in x + x -> x Many
    #[test]
    fn test_many_var() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Lit(Literal::LitInt(1)),
            CoreFrame::Var(x),
            CoreFrame::Var(x),
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![1, 2],
            },
            CoreFrame::LetNonRec {
                binder: x,
                rhs: 0,
                body: 3,
            },
        ]);
        let map = occ_analysis(&expr);
        assert_eq!(get_occ(&map, x), Occ::Many);
    }

    // 4. λx. x -> x Once
    #[test]
    fn test_lam_binder_excluded() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Var(x),
            CoreFrame::Lam { binder: x, body: 0 },
        ]);
        let map = occ_analysis(&expr);
        assert_eq!(get_occ(&map, x), Occ::Once);
    }

    // 5. letrec { f = g; g = f } in 0 -> both Once
    #[test]
    fn test_letrec_sibling_refs() {
        let f = VarId(1);
        let g = VarId(2);
        let expr = tree(vec![
            CoreFrame::Var(g),                  // 0
            CoreFrame::Var(f),                  // 1
            CoreFrame::Lit(Literal::LitInt(0)), // 2
            CoreFrame::LetRec {
                bindings: vec![(f, 0), (g, 1)],
                body: 2,
            },
        ]);
        let map = occ_analysis(&expr);
        assert_eq!(get_occ(&map, f), Occ::Once);
        assert_eq!(get_occ(&map, g), Occ::Once);
    }

    // 6. case x of w { Just y → y } -> x Once, w Dead, y Once
    #[test]
    fn test_case_binders() {
        let x = VarId(1);
        let w = VarId(2);
        let y = VarId(3);
        let expr = tree(vec![
            CoreFrame::Var(x), // 0
            CoreFrame::Var(y), // 1
            CoreFrame::Case {
                scrutinee: 0,
                binder: w,
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(1)),
                    binders: vec![y],
                    body: 1,
                }],
            },
        ]);
        let map = occ_analysis(&expr);
        assert_eq!(get_occ(&map, x), Occ::Once);
        assert_eq!(get_occ(&map, w), Occ::Dead);
        assert_eq!(get_occ(&map, y), Occ::Once);
    }

    // 7. case x of w { Just y → w } -> x Once, w Once, y Dead
    #[test]
    fn test_case_binder_used() {
        let x = VarId(1);
        let w = VarId(2);
        let y = VarId(3);
        let expr = tree(vec![
            CoreFrame::Var(x), // 0
            CoreFrame::Var(w), // 1
            CoreFrame::Case {
                scrutinee: 0,
                binder: w,
                alts: vec![Alt {
                    con: AltCon::DataAlt(DataConId(1)),
                    binders: vec![y],
                    body: 1,
                }],
            },
        ]);
        let map = occ_analysis(&expr);
        assert_eq!(get_occ(&map, x), Occ::Once);
        assert_eq!(get_occ(&map, w), Occ::Once);
        assert_eq!(get_occ(&map, y), Occ::Dead);
    }

    // 8. A single Var node shared by two parent edges: x + x where both PrimOp
    // args point at the SAME Var(x) index (index 0 appears once in `nodes`,
    // but is reachable via two distinct edges). Regression for the DAG-sharing
    // undercount: a flat scan over `nodes` sees one `Var` frame and reports
    // `Once`, but the node is used twice.
    #[test]
    fn test_shared_var_node_two_parent_edges() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Var(x), // 0: the single shared Var(x) node
            CoreFrame::PrimOp {
                op: PrimOpKind::IntAdd,
                args: vec![0, 0], // both operands reference index 0
            }, // 1: x + x, via one shared node
        ]);
        let map = occ_analysis(&expr);
        assert_eq!(get_occ(&map, x), Occ::Many);
    }

    // 9. The same shared-node shape as (8), but the two edges come from
    // different parents rather than the same PrimOp's args list — the "DAG
    // node referenced by multiple parent edges" shape the sharing gates must
    // see as Many.
    #[test]
    fn test_shared_var_node_distinct_parents() {
        let x = VarId(1);
        let expr = tree(vec![
            CoreFrame::Var(x),                     // 0: the single shared Var(x) node
            CoreFrame::Lam { binder: x, body: 0 }, // 1: unused wrapper, just another parent of 0
            CoreFrame::App { fun: 1, arg: 0 },     // 2: also refers to node 0 directly
        ]);
        let map = occ_analysis(&expr);
        assert_eq!(get_occ(&map, x), Occ::Many);
    }
}
