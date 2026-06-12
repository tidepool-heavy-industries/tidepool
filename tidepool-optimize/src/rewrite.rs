//! Shared stack-safe redex search for the rewrite passes.
//!
//! Every pass (`beta`, `dce`, `inline`, `case_reduce`) performs a single
//! pre-order, left-to-right search for the first applicable redex, rewrites it,
//! and returns (the pipeline re-runs to fixpoint). The search formerly recursed
//! by tree depth (`try_*_at`); [`find_redex`] replaces that with an explicit
//! heap stack, so the passes walk the same deep trees that bit the emitter
//! without consuming call stack.
//!
//! The per-node predicate (`try_at`) inspects ONE node and returns the rewritten
//! whole-tree iff that node is a redex; non-redex nodes fall through to a
//! children-in-order descent — exactly the order the recursive `try_*_at`
//! walked, so the first redex selected is unchanged.

use tidepool_repr::{get_children, CoreExpr};

/// Pre-order, left-to-right search for the first redex; returns the rewritten
/// tree, or `None` if no node is a redex.
///
/// `try_at(expr, idx)` must be side-effect free and return `Some(new_tree)` only
/// when `idx` is itself a redex (it must NOT recurse into children — the descent
/// is this driver's job).
pub(crate) fn find_redex<F>(expr: &CoreExpr, mut try_at: F) -> Option<CoreExpr>
where
    F: FnMut(&CoreExpr, usize) -> Option<CoreExpr>,
{
    if expr.nodes.is_empty() {
        return None;
    }
    // `seen` keeps shared (DAG) subtrees from being re-walked; since the search
    // returns on the first redex and a non-redex node stays a non-redex, this
    // never changes which redex is found — it only avoids the recursive walk's
    // latent exponential re-descent on shared nodes.
    let mut seen: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
    let mut stack = vec![expr.nodes.len() - 1];
    while let Some(idx) = stack.pop() {
        if !seen.insert(idx) {
            continue;
        }
        if let Some(new_expr) = try_at(expr, idx) {
            return Some(new_expr);
        }
        // Reversed so children pop left-to-right (matches `try_*_at` order).
        for &c in get_children(&expr.nodes[idx]).iter().rev() {
            if !seen.contains(&c) {
                stack.push(c);
            }
        }
    }
    None
}
