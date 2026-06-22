//! Optimization passes for Tidepool Core expressions.
//!
//! Includes beta reduction, case reduction, dead code elimination, inlining,
//! occurrence analysis, and partial evaluation.

pub mod beta;
pub mod case_reduce;
pub mod dce;
pub mod inline;
pub mod occ;
pub mod partial;
pub mod pipeline;
mod rewrite;

pub use pipeline::{default_passes, optimize, run_pipeline, PipelineStats};

/// Shared body of every `Pass::run`: skip an empty tree, run the pass's rewrite,
/// and if it produced a new expression install it and report `Changed`. Each
/// pass (beta/case_reduce/dce/inline) supplies only its `try_*` rewrite closure.
pub(crate) fn apply_rewrite(
    expr: &mut tidepool_repr::CoreExpr,
    rewrite: impl FnOnce(&tidepool_repr::CoreExpr) -> Option<tidepool_repr::CoreExpr>,
) -> tidepool_eval::Changed {
    if expr.nodes.is_empty() {
        return false;
    }
    match rewrite(expr) {
        Some(new_expr) => {
            *expr = new_expr;
            true
        }
        None => false,
    }
}
