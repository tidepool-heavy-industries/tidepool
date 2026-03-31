//! Occurrence analysis for Core expressions.

use std::collections::HashMap;
use tidepool_repr::{CoreExpr, CoreFrame, VarId};

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
    /// Add two occurrence counts.
    ///
    /// This is a lattice join where Dead is the identity element,
    /// and any combination of non-Dead values results in Many.
    #[allow(clippy::should_implement_trait)]
    pub fn add(self, other: Occ) -> Occ {
        match (self, other) {
            (Occ::Dead, o) | (o, Occ::Dead) => o,
            _ => Occ::Many,
        }
    }
}

/// Map from variable to occurrence count.
pub type OccMap = HashMap<VarId, Occ>;

/// Count occurrences of all variables in the expression.
/// Binding sites (in Lam, Let, Case, Join) are NOT counted as occurrences.
/// Only Var(v) nodes (variable use sites) are counted.
pub fn occ_analysis(expr: &CoreExpr) -> OccMap {
    let mut map = OccMap::new();
    for node in &expr.nodes {
        if let CoreFrame::Var(v) = node {
            let entry = map.entry(*v).or_insert(Occ::Dead);
            *entry = entry.add(Occ::Once);
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
}
