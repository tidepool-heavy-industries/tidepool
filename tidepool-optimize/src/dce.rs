//! Dead code elimination pass for Core expressions.

use crate::occ::{get_occ, occ_analysis, Occ};
use crate::{Changed, Pass};
use tidepool_repr::{replace_subtree, CoreExpr, CoreFrame};

/// Dead Code Elimination pass.
/// Removes `LetNonRec` bindings where the binder is unused.
/// Removes `LetRec` groups where all binders are unused.
pub struct Dce;

impl Pass for Dce {
    fn run(&self, expr: &mut CoreExpr) -> Changed {
        crate::apply_rewrite(expr, |e| try_dce(e, &occ_analysis(e)))
    }

    fn name(&self) -> &str {
        "Dce"
    }
}

fn try_dce(expr: &CoreExpr, occ_map: &crate::occ::OccMap) -> Option<CoreExpr> {
    crate::rewrite::find_redex(expr, |expr, idx| try_dce_at(expr, idx, occ_map))
}

/// Dead-binding test for a single node: a `LetNonRec` whose binder is dead, or a
/// `LetRec` whose binders are all dead. Non-redex nodes return `None`; the
/// search driver handles descent.
fn try_dce_at(expr: &CoreExpr, idx: usize, occ_map: &crate::occ::OccMap) -> Option<CoreExpr> {
    match &expr.nodes[idx] {
        CoreFrame::LetNonRec { binder, body, .. } => {
            if get_occ(occ_map, *binder) == Occ::Dead {
                // Drop the binding, keep just body
                let body_tree = expr.extract_subtree(*body);
                Some(replace_subtree(expr, idx, &body_tree))
            } else {
                None
            }
        }
        CoreFrame::LetRec { bindings, body } => {
            let all_dead = bindings
                .iter()
                .all(|(binder, _)| get_occ(occ_map, *binder) == Occ::Dead);
            if all_dead {
                // Drop the entire group, keep just body
                let body_tree = expr.extract_subtree(*body);
                Some(replace_subtree(expr, idx, &body_tree))
            } else {
                None
            }
        }
        _ => None,
    }
}
