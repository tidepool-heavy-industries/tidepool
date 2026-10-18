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
    with_settlement_report(action, std::io::stderr)
}

fn with_settlement_report<T, W: std::io::Write>(
    action: impl FnOnce(&mut dyn FnMut(tidepool_runtime::CompilerTransactionClose)) -> T,
    report: impl FnOnce() -> W,
) -> T {
    use tidepool_runtime::CompilerTransactionClose;

    let mut closes = Vec::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        action(&mut |close| closes.push(close))
    }));
    let confirmed = closes.iter().all(|close| {
        matches!(
            close,
            CompilerTransactionClose::Clean | CompilerTransactionClose::NotStarted
        )
    });
    match result {
        Ok(result) => {
            assert!(confirmed, "compiler close is unconfirmed: {closes:?}");
            result
        }
        Err(primary) => {
            if !confirmed {
                // Reporting is secondary to the retained operation panic, even
                // when a writer or diagnostic formatter itself panics.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut report = report();
                    let _ = writeln!(report, "compiler close is unconfirmed: {closes:?}");
                }));
            }
            std::panic::resume_unwind(primary)
        }
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

    fn unconfirmed() -> CompilerTransactionClose {
        use tidepool_extract_cmd::{
            CompilerTransactionCloseEvidence, CompilerTransactionCloseReason,
            CompilerTransactionRetirement,
        };
        CompilerTransactionClose::Unconfirmed(CompilerTransactionCloseEvidence::new(
            CompilerTransactionCloseReason::Cancelled,
            CompilerTransactionRetirement::DaemonUnobserved { disconnect: None },
        ))
    }

    #[test]
    fn uncertain_drop_retains_exact_original_panic_and_reports_closes() {
        struct CloseOnDrop<'a>(&'a mut dyn FnMut(CompilerTransactionClose));
        impl Drop for CloseOnDrop<'_> {
            fn drop(&mut self) {
                (self.0)(unconfirmed());
                (self.0)(CompilerTransactionClose::Clean);
            }
        }
        let payload = std::sync::Arc::new("original owned panic");
        let expected = payload.clone();
        let mut report = Vec::new();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::with_settlement_report(
                |recipient| {
                    recipient(CompilerTransactionClose::NotStarted);
                    let _close = CloseOnDrop(recipient);
                    std::panic::panic_any(payload);
                },
                || &mut report,
            );
        }))
        .unwrap_err();
        assert!(std::sync::Arc::ptr_eq(
            panic.downcast_ref::<std::sync::Arc<&str>>().unwrap(),
            &expected
        ));
        let report = String::from_utf8(report).unwrap();
        assert!(report.contains("Unconfirmed"));
        assert!(report.contains("Cancelled"));
        assert!(report.contains("Clean"));
    }

    #[test]
    fn reporting_failure_cannot_replace_original_panic() {
        struct BrokenReport {
            panics: bool,
        }
        impl std::io::Write for BrokenReport {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                if self.panics {
                    panic!("report writer panic");
                }
                Err(std::io::Error::other("report writer failed"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for panics in [false, true] {
            let panic = std::panic::catch_unwind(|| {
                super::with_settlement_report(
                    |recipient| {
                        recipient(unconfirmed());
                        panic!("original operation failure");
                    },
                    || BrokenReport { panics },
                );
            })
            .unwrap_err();
            assert_eq!(
                panic.downcast_ref::<&str>(),
                Some(&"original operation failure")
            );
        }
    }

    #[test]
    fn normally_returned_value_with_uncertain_close_still_refuses() {
        let panic = std::panic::catch_unwind(|| {
            with_settlement(|recipient| {
                recipient(unconfirmed());
                42
            });
        });
        assert!(panic.is_err());
    }

    #[test]
    fn diagnostic_sink_is_acquired_only_after_operation_unwinds() {
        let body_finished = std::cell::Cell::new(false);
        let sink_acquired = std::cell::Cell::new(false);
        struct Finished<'a>(&'a std::cell::Cell<bool>);
        impl Drop for Finished<'_> {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::with_settlement_report(
                |recipient| {
                    assert!(!sink_acquired.get());
                    let _finished = Finished(&body_finished);
                    recipient(unconfirmed());
                    panic!("original operation failure");
                },
                || {
                    assert!(body_finished.get());
                    sink_acquired.set(true);
                    Vec::new()
                },
            );
        }))
        .unwrap_err();
        assert!(sink_acquired.get());
        assert_eq!(
            panic.downcast_ref::<&str>(),
            Some(&"original operation failure")
        );
    }
}
