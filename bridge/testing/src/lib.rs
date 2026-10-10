//! Shared test support for prepared-STG execution.
//!
//! # Integration-test harness
//!
//! New `tidepool-runtime` / `tidepool-repl` integration tests should build their
//! setup through [`eval_harness::EvalHarness`] rather than hand-rolling the
//! include path, the 8-256 MiB eval thread, the 13-effect GADT preamble, and the
//! mock handler stack. The harness wraps the real `tidepool_runtime` entry points
//! (`compile_and_run` / `compile_haskell`), so tests
//! keep driving the production compile→JIT→dispatch path. See that module's docs
//! for the effectful and compile-only recipes.

pub mod effect_surface;
pub mod eval_harness;
pub mod fixtures;

pub use fixtures::fixture_source;

/// Retain compiler closure observations through a test operation, including unwind.
///
/// Use this for operations whose contract permits only clean closure or no
/// submission. Tests exercising uncertain closure must retain and check their
/// own observations instead.
pub fn with_settlement<T>(
    action: impl FnOnce(&mut dyn FnMut(tidepool_runtime::CompilerTransactionClose)) -> T,
) -> T {
    use std::sync::{Arc, Mutex};
    use tidepool_runtime::CompilerTransactionClose;

    let observations = Arc::new(Mutex::new(Vec::new()));
    let recipient = Arc::clone(&observations);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        action(&mut |close| recipient.lock().unwrap().push(close))
    }));
    let closes = observations.lock().unwrap();
    assert!(
        closes.iter().all(|close| matches!(
            close,
            CompilerTransactionClose::Clean | CompilerTransactionClose::NotStarted
        )),
        "compiler close is unconfirmed: {closes:?}"
    );
    drop(closes);
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[cfg(test)]
mod compiler_settlement_tests {
    use super::with_settlement;
    use tidepool_runtime::CompilerTransactionClose;

    #[test]
    fn operation_result_survives_multiple_closed_transactions() {
        let result = with_settlement(|recipient| {
            recipient(CompilerTransactionClose::NotStarted);
            recipient(CompilerTransactionClose::Clean);
            42
        });
        assert_eq!(result, 42);
    }

    #[test]
    fn unwind_retains_drop_observation_and_resumes_original_panic() {
        struct CloseOnDrop<'a>(&'a mut dyn FnMut(CompilerTransactionClose));
        impl Drop for CloseOnDrop<'_> {
            fn drop(&mut self) {
                (self.0)(CompilerTransactionClose::Clean);
            }
        }

        let panic = std::panic::catch_unwind(|| {
            with_settlement(|recipient| {
                let _transaction = CloseOnDrop(recipient);
                panic!("operation unwind");
            });
        })
        .unwrap_err();
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"operation unwind"));
    }
}
