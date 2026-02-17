use core_eval::pass::{Pass, Changed};
use core_repr::CoreExpr;
use crate::beta::BetaReduce;
use crate::inline::Inline;
use crate::case_reduce::CaseReduce;
use crate::dce::Dce;
use crate::partial::PartialEval;

/// Maximum number of iterations for the pipeline to avoid infinite loops.
pub const MAX_PIPELINE_ITERATIONS: usize = 1000;

/// Statistics from a pipeline run.
#[derive(Debug, Clone, Default)]
pub struct PipelineStats {
    /// Total number of pipeline iterations.
    /// Includes the final iteration where no changes were reported.
    pub iterations: usize,
    /// Total number of times each pass was invoked.
    pub pass_invocations: Vec<(String, usize)>,
}

/// Run a sequence of passes to fixed point.
/// Keeps iterating until no pass reports a change or MAX_PIPELINE_ITERATIONS is reached.
/// Returns stats about how many iterations and per-pass invocations.
///
/// # Panics
/// Panics if the number of iterations exceeds MAX_PIPELINE_ITERATIONS.
pub fn run_pipeline(passes: &[Box<dyn Pass>], expr: &mut CoreExpr) -> PipelineStats {
    let mut stats = PipelineStats {
        iterations: 0,
        pass_invocations: passes.iter().map(|p| (p.name().to_string(), 0)).collect(),
    };

    if passes.is_empty() {
        return stats;
    }

    loop {
        stats.iterations += 1;
        if stats.iterations > MAX_PIPELINE_ITERATIONS {
            panic!(
                "Optimization pipeline exceeded maximum iterations ({}). Potential infinite loop in passes: {:?}",
                MAX_PIPELINE_ITERATIONS,
                passes.iter().map(|p| p.name()).collect::<Vec<_>>()
            );
        }

        let mut changed: Changed = false;
        for (i, pass) in passes.iter().enumerate() {
            if pass.run(expr) {
                changed = true;
            }
            stats.pass_invocations[i].1 += 1;
        }

        if !changed {
            break;
        }
    }

    stats
}

/// Returns the default optimization pass sequence.
/// Order: BetaReduce → Inline → CaseReduce → Dce → PartialEval.
pub fn default_passes() -> Vec<Box<dyn Pass>> {
    vec![
        Box::new(BetaReduce),
        Box::new(Inline),
        Box::new(CaseReduce),
        Box::new(Dce),
        Box::new(PartialEval),
    ]
}

/// Run the default optimization pipeline to fixed point.
pub fn optimize(expr: &mut CoreExpr) -> PipelineStats {
    run_pipeline(&default_passes(), expr)
}

/// Run a single pass to fixed point (convenience).
/// Returns the number of times the pass reported a change.
pub fn run_pass_to_fixpoint(pass: &dyn Pass, expr: &mut CoreExpr) -> usize {
    let mut changes = 0;
    loop {
        if !pass.run(expr) {
            break;
        }
        changes += 1;
        if changes >= MAX_PIPELINE_ITERATIONS {
            panic!(
                "Pass '{}' exceeded maximum iterations ({}) in run_pass_to_fixpoint.",
                pass.name(),
                MAX_PIPELINE_ITERATIONS
            );
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_repr::{CoreFrame, RecursiveTree, VarId};
    use std::cell::Cell;

    struct TestPass {
        name: String,
        changes_remaining: Cell<usize>,
    }

    impl Pass for TestPass {
        fn run(&self, _expr: &mut CoreExpr) -> Changed {
            let rem = self.changes_remaining.get();
            if rem > 0 {
                self.changes_remaining.set(rem - 1);
                true
            } else {
                false
            }
        }

        fn name(&self) -> &str {
            &self.name
        }
    }

    fn dummy_expr() -> CoreExpr {
        RecursiveTree {
            nodes: vec![CoreFrame::Var(VarId(0))],
        }
    }

    #[test]
    fn test_empty_pipeline() {
        let mut expr = dummy_expr();
        let stats = run_pipeline(&[], &mut expr);
        assert_eq!(stats.iterations, 0);
        assert!(stats.pass_invocations.is_empty());
    }

    #[test]
    fn test_single_noop_pass() {
        let mut expr = dummy_expr();
        let pass = Box::new(TestPass {
            name: "NoOp".to_string(),
            changes_remaining: Cell::new(0),
        });
        let stats = run_pipeline(&[pass], &mut expr);
        assert_eq!(stats.iterations, 1);
        assert_eq!(stats.pass_invocations[0], ("NoOp".to_string(), 1));
    }

    #[test]
    fn test_single_changing_pass() {
        let mut expr = dummy_expr();
        let pass = Box::new(TestPass {
            name: "Changing".to_string(),
            changes_remaining: Cell::new(1),
        });
        let stats = run_pipeline(&[pass], &mut expr);
        assert_eq!(stats.iterations, 2);
        assert_eq!(stats.pass_invocations[0], ("Changing".to_string(), 2));
    }

    #[test]
    fn test_fixed_point_terminates() {
        let mut expr = dummy_expr();
        let n = 5;
        let pass = Box::new(TestPass {
            name: "N-Times".to_string(),
            changes_remaining: Cell::new(n),
        });
        let stats = run_pipeline(&[pass], &mut expr);
        assert_eq!(stats.iterations, n + 1);
        assert_eq!(stats.pass_invocations[0], ("N-Times".to_string(), n + 1));
    }

    #[test]
    fn test_pipeline_stats() {
        let mut expr = dummy_expr();
        let pass1 = Box::new(TestPass {
            name: "P1".to_string(),
            changes_remaining: Cell::new(2),
        });
        let pass2 = Box::new(TestPass {
            name: "P2".to_string(),
            changes_remaining: Cell::new(1),
        });
        let stats = run_pipeline(&[pass1, pass2], &mut expr);
        // Iteration 1: P1 changes (2->1), P2 changes (1->0). Changed = true.
        // Iteration 2: P1 changes (1->0), P2 no change. Changed = true.
        // Iteration 3: P1 no change, P2 no change. Changed = false. Break.
        assert_eq!(stats.iterations, 3);
        assert_eq!(stats.pass_invocations[0], ("P1".to_string(), 3));
        assert_eq!(stats.pass_invocations[1], ("P2".to_string(), 3));
    }

    #[test]
    fn test_run_pass_to_fixpoint() {
        let mut expr = dummy_expr();
        let n = 3;
        let pass = TestPass {
            name: "N-Times".to_string(),
            changes_remaining: Cell::new(n),
        };
        let changes = run_pass_to_fixpoint(&pass, &mut expr);
        assert_eq!(changes, n);
    }

    #[test]
    #[should_panic(expected = "Optimization pipeline exceeded maximum iterations")]
    fn test_infinite_loop_panic() {
        struct InfinitePass;
        impl Pass for InfinitePass {
            fn run(&self, _expr: &mut CoreExpr) -> Changed { true }
            fn name(&self) -> &str { "Infinite" }
        }
        let mut expr = dummy_expr();
        run_pipeline(&[Box::new(InfinitePass)], &mut expr);
    }
}