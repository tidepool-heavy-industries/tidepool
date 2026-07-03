//! Optimization pass trait and associated types.

use tidepool_repr::CoreExpr;

/// Whether a pass changed the expression.
pub type Changed = bool;

/// An optimization pass over CoreExpr.
pub trait Pass {
    /// Run the pass, mutating the expression in place. Returns true if anything changed.
    fn run(&self, expr: &mut CoreExpr) -> Changed;

    /// Human-readable name for diagnostics.
    fn name(&self) -> &str;
}
