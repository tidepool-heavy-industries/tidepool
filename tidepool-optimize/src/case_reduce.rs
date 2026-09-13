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
