//! Optimization passes for Tidepool Core expressions.
//!
//! Includes beta reduction, case reduction, dead code elimination, inlining,
//! occurrence analysis, and partial evaluation.

/// Beta reduction: `(\x -> body) arg` → `body` with `x` substituted for `arg`.
pub mod beta;
/// Case-of-known-constructor and case-of-known-literal reduction.
pub mod case_reduce;
/// Dead code elimination: drop unreferenced `Let`/`LetRec` bindings.
pub mod dce;
/// Inlining: substitute a single-use `LetNonRec` binding at its use site.
pub mod inline;
/// Occurrence analysis: counts how many times each bound variable is used.
pub mod occ;
/// First-order partial evaluation over statically-known values.
pub mod partial;
/// The [`Pass`] trait and its `Changed` return type, shared by every pass.
pub mod pass;
/// Fixed-point pipeline orchestration: runs a sequence of passes to convergence.
pub mod pipeline;
mod rewrite;

pub use pass::{Changed, Pass};
pub use pipeline::{default_passes, optimize, run_pipeline, PipelineStats};

/// Shared body of every `Pass::run`: skip an empty tree, run the pass's rewrite,
/// and if it produced a new expression install it and report `Changed`. Each
/// pass (beta/case_reduce/dce/inline) supplies only its `try_*` rewrite closure.
pub(crate) fn apply_rewrite(
    expr: &mut tidepool_repr::CoreExpr,
    rewrite: impl FnOnce(&tidepool_repr::CoreExpr) -> Option<tidepool_repr::CoreExpr>,
) -> Changed {
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
