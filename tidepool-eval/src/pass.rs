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

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::{CoreFrame, RecursiveTree, VarId};

    struct NoOpPass;

    impl Pass for NoOpPass {
        fn run(&self, _expr: &mut CoreExpr) -> Changed {
            false
        }

        fn name(&self) -> &str {
            "NoOpPass"
        }
    }

    #[test]
    fn test_noop_pass() {
        let pass = NoOpPass;
        let mut expr = RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        };
        let changed = pass.run(&mut expr);
        assert!(!changed);
        assert_eq!(pass.name(), "NoOpPass");
    }
}
