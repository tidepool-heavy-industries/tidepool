#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
use tidepool_extract_cmd::{with_compiler_transaction, CompilerTransactionOutcome, ExtractCmd};

fn retained_scope<T>(action: impl FnOnce() -> T) -> CompilerTransactionOutcome<T> {
    let retained = std::cell::RefCell::new(None);
    let outcome = with_compiler_transaction(|close| *retained.borrow_mut() = Some(close), action);
    assert_eq!(retained.into_inner(), Some(outcome.close.clone()));
    outcome
}

fn clean_action<T>(outcome: CompilerTransactionOutcome<T>) -> T {
    assert!(
        outcome.close.is_clean(),
        "compiler close: {:?}",
        outcome.close
    );
    outcome.action
}
#[path = "support/compiler_inputs.rs"]
mod compiler_inputs;

struct TestEnvironment {
    dir: std::path::PathBuf,
    socket: Option<std::ffi::OsString>,
    timing: Option<std::ffi::OsString>,
    memo_trace: Option<std::ffi::OsString>,
}

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        match self.socket.take() {
            Some(value) => std::env::set_var("TIDEPOOL_EXTRACT_DAEMON_SOCKET", value),
            None => std::env::remove_var("TIDEPOOL_EXTRACT_DAEMON_SOCKET"),
        }
        match self.timing.take() {
            Some(value) => std::env::set_var("TIDEPOOL_TIMING", value),
            None => std::env::remove_var("TIDEPOOL_TIMING"),
        }
        match self.memo_trace.take() {
            Some(value) => std::env::set_var("TIDEPOOL_MEMO_TRACE", value),
            None => std::env::remove_var("TIDEPOOL_MEMO_TRACE"),
        }
        // best-effort: test cleanup of a temp path.
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

#[test]
fn direct_transaction_executes_multiple_compiler_requests() {
    compiler_inputs::require_compiler_executables();
    let dir = std::env::temp_dir().join(format!(
        "tidepool-compiler-transaction-{}",
        std::process::id()
    ));
    // best-effort: test cleanup of a temp path from a prior run.
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("Expr.hs");
    std::fs::write(
        &source,
        "module Expr where\nimport Dep\nresult :: Int\nresult = dep + 2\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Dep.hs"),
        "module Dep where\ndep :: Int\ndep = 40\n",
    )
    .unwrap();

    let _environment = TestEnvironment {
        dir: dir.clone(),
        socket: std::env::var_os("TIDEPOOL_EXTRACT_DAEMON_SOCKET"),
        timing: std::env::var_os("TIDEPOOL_TIMING"),
        memo_trace: std::env::var_os("TIDEPOOL_MEMO_TRACE"),
    };
    std::env::remove_var("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
    std::env::set_var("TIDEPOOL_TIMING", "1");
    let diagnostics = clean_action(retained_scope(|| {
        let mut diagnostics = Vec::new();
        for index in 0..3 {
            if index == 2 {
                std::fs::write(
                    dir.join("Dep.hs"),
                    "module Dep where\ndep :: Int\ndep = 41\n",
                )
                .unwrap();
            }
            let mut command = ExtractCmd::new().unwrap();
            command
                .input(&source)
                .output_dir(dir.join(format!("out-{index}")))
                .target("result")
                .include(&dir);
            let endpoint = command.bind().unwrap();
            let run = endpoint.execute(&command).unwrap();
            assert!(run.success(), "request {index}: {}", run.stderr_lossy());
            diagnostics.push(run.stderr_lossy().into_owned());
        }
        diagnostics
    }));
    // Loading can capture finalized Core before the later memo lookup, so
    // its prose miss message does not establish whether any work was reused.
    // Assert the actual dependency decisions and work, with source versions
    // linking the cold, unchanged and changed requests in this transaction.
    let reuse_events: Vec<Vec<serde_json::Value>> = diagnostics
        .iter()
        .map(|diagnostic| {
            diagnostic
                .lines()
                .filter_map(|line| line.trim_start().strip_prefix("tidepool-reuse "))
                .map(|json| {
                    serde_json::from_str::<serde_json::Value>(json)
                        .expect("compiler reuse observations must be valid JSON")
                })
                .collect()
        })
        .collect();
    let mut source_versions = Vec::new();
    let mut cycles = std::collections::BTreeSet::new();
    for (index, observed) in reuse_events.iter().enumerate() {
        let events: Vec<_> = observed
            .iter()
            .filter(|event| event["module"] == "Dep")
            .collect();
        let decisions: Vec<_> = events
            .iter()
            .filter(|event| event["stage"] == "source_frontend" && event["decision"] != "work")
            .collect();
        assert_eq!(decisions.len(), 1, "request {index}: {events:?}");
        let decision = decisions[0];
        assert_eq!(decision["schema"], 1);
        assert_eq!(decision["unit"], "main");
        assert_eq!(decision["purpose"], "general");
        assert_eq!(decision["items"], 1);
        assert_eq!(decision["version_kind"], "source_fingerprint");
        let cycle = decision["cycle"].as_u64().expect("actual compiler cycle");
        assert!(cycles.insert(cycle), "requests must have distinct cycles");
        assert!(events.iter().all(|event| event["cycle"] == cycle));
        let version = decision["version"]
            .as_str()
            .filter(|version| !version.is_empty())
            .expect("dependency source fingerprint");
        source_versions.push(version.to_owned());
        let warm = index == 1;
        assert_eq!(decision["decision"], if warm { "hit" } else { "miss" });
        assert_eq!(
            decision["reason"],
            match index {
                0 => "absent",
                1 => "matched",
                2 => "changed_source",
                _ => unreachable!(),
            }
        );
        for stage in ["source_frontend", "finalized_core"] {
            let complete = observed
                .iter()
                .filter(|event| {
                    event["cycle"] == cycle
                        && event["stage"] == stage
                        && event["decision"] == "complete"
                        && event["reason"] == "stage_complete"
                        && event["module"].is_null()
                        && event["items"] == 0
                })
                .count();
            assert_eq!(complete, 1, "request {index}, missing {stage} completion");
            let work = events
                .iter()
                .filter(|event| event["stage"] == stage && event["decision"] == "work")
                .count();
            assert_eq!(work == 0, warm, "request {index}, {stage}: {events:?}");
        }
        if warm {
            assert!(events.iter().any(|event| {
                event["stage"] == "finalized_core"
                    && event["decision"] == "hit"
                    && event["reason"] == "matched"
            }));
        }
    }
    assert_eq!(source_versions[0], source_versions[1]);
    assert_ne!(source_versions[1], source_versions[2]);

    std::fs::write(&source, "module Expr where\nresult =\n").unwrap();
    clean_action(retained_scope(|| {
        let mut command = ExtractCmd::new().unwrap();
        command
            .input(&source)
            .output_dir(dir.join("failed"))
            .target("result")
            .include(&dir);
        let run = command.bind().unwrap().execute(&command).unwrap();
        assert!(!run.success(), "invalid source must be rejected");
    }));

    std::fs::write(
        &source,
        "module Expr where\nimport Dep\nresult :: Int\nresult = dep + 2\n",
    )
    .unwrap();
    clean_action(retained_scope(|| {
        let mut command = ExtractCmd::new().unwrap();
        command
            .input(&source)
            .output_dir(dir.join("after-failure"))
            .target("result")
            .include(&dir);
        let run = command.bind().unwrap().execute(&command).unwrap();
        assert!(
            run.success(),
            "a new transaction is admitted after rejection: {}",
            run.stderr_lossy()
        );
    }));
}

/// `TIDEPOOL_MEMO_TRACE=1` is a diagnostic-only stderr emitter: it must
/// forward the two new `tidepool-memo-*` lines and change nothing about the
/// compiled artifact. Compile identical source with the flag off and on and
/// assert byte-identical stdout (the CBOR/artifact payload).
#[test]
fn memo_trace_flag_adds_diagnostics_without_changing_compiled_output() {
    compiler_inputs::require_compiler_executables();
    let dir = std::env::temp_dir().join(format!("tidepool-memo-trace-{}", std::process::id()));
    // best-effort: test cleanup of a temp path from a prior run.
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("Expr.hs");
    std::fs::write(
        &source,
        "module Expr where\nimport Dep\nresult :: Int\nresult = dep + 2\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Dep.hs"),
        "module Dep where\ndep :: Int\ndep = 40\n",
    )
    .unwrap();

    let _environment = TestEnvironment {
        dir: dir.clone(),
        socket: std::env::var_os("TIDEPOOL_EXTRACT_DAEMON_SOCKET"),
        timing: std::env::var_os("TIDEPOOL_TIMING"),
        memo_trace: std::env::var_os("TIDEPOOL_MEMO_TRACE"),
    };
    std::env::remove_var("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
    std::env::remove_var("TIDEPOOL_TIMING");

    let run_once = |output_dir: &str| {
        let mut command = ExtractCmd::new().unwrap();
        command
            .input(&source)
            .output_dir(dir.join(output_dir))
            .target("result")
            .include(&dir);
        let endpoint = command.bind().unwrap();
        let run = endpoint.execute(&command).unwrap();
        assert!(run.success(), "{output_dir}: {}", run.stderr_lossy());
        run
    };

    std::env::remove_var("TIDEPOOL_MEMO_TRACE");
    let without_trace = clean_action(retained_scope(|| run_once("without-trace")));

    std::env::set_var("TIDEPOOL_MEMO_TRACE", "1");
    let with_trace = clean_action(retained_scope(|| run_once("with-trace")));

    assert_eq!(
        without_trace.output.stdout, with_trace.output.stdout,
        "TIDEPOOL_MEMO_TRACE must not change the compiled artifact"
    );
    assert!(
        with_trace
            .stderr_lossy()
            .contains("tidepool-memo-cycle-graph"),
        "expected a tidepool-memo-cycle-graph line with TIDEPOOL_MEMO_TRACE=1: {}",
        with_trace.stderr_lossy()
    );
    assert!(
        !without_trace
            .stderr_lossy()
            .contains("tidepool-memo-cycle-graph"),
        "must not emit tidepool-memo-cycle-graph without TIDEPOOL_MEMO_TRACE=1: {}",
        without_trace.stderr_lossy()
    );
}

/// Exercise the declared production frontend and Haskell worker through the
/// same lazy transaction scope and affine close sink as native callers.
fn genuine_frontend_close_fixture(obstruct: bool) {
    use std::cell::RefCell;
    use tidepool_extract_cmd::{
        CompilerScratchRetirement, CompilerTermination, CompilerTransactionClose,
        CompilerTransactionCloseReason, CompilerTransactionRetirement, CompilerWorkerRetirement,
    };

    compiler_inputs::require_compiler_executables();
    assert!(
        std::env::var_os(tidepool_extract_cmd::REQUIRED_DAEMON_ENDPOINT_ENV).is_none()
            && std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV).is_none(),
        "this producer join requires the runner's explicit --compiler-mode direct"
    );
    let scratch = match std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT") {
        Some(root) => {
            let root = std::path::PathBuf::from(root).join("genuine-frontend-close");
            std::fs::create_dir(&root).unwrap();
            root
        }
        None => {
            let scratch = tempfile::tempdir().unwrap();
            // Keep actual compiler inputs and products if an assertion fails.
            scratch.keep()
        }
    };
    let source = scratch.join("CloseEvidence.hs");
    std::fs::write(
        &source,
        "module CloseEvidence where\nresult :: Int\nresult = 42\n",
    )
    .unwrap();
    let logical = scratch.join("logical-products");
    std::fs::create_dir(&logical).unwrap();
    let unrelated = logical.join("caller-owned");
    std::fs::write(&unrelated, b"retain caller ownership").unwrap();

    let retained_close = RefCell::new(None);
    let completed_body = RefCell::new(None);
    let physical = RefCell::new(None);
    let expected_os_cause = RefCell::new(None);
    let outcome = with_compiler_transaction(
        |close| *retained_close.borrow_mut() = Some(close),
        || {
            let mut command = ExtractCmd::new().unwrap();
            command
                .input(&source)
                .output_dir(scratch.join("artifacts"))
                .target("result")
                .include(&scratch)
                .build_products_dir(&logical);
            let run = command.bind().unwrap().execute(&command).unwrap();
            assert!(
                run.success(),
                "actual compiler request: {}",
                run.stderr_lossy()
            );
            assert!(
                !run.output.stdout.is_empty(),
                "actual producer must complete its body"
            );
            *completed_body.borrow_mut() = Some(run.output.stdout.clone());

            // Discover the actual interface in this exclusive logical root.
            // No private namespace grammar or rendered stderr drives ownership.
            let mut pending = vec![logical.clone()];
            let mut interface_directories = Vec::new();
            let mut visited = 0;
            while let Some(directory) = pending.pop() {
                visited += 1;
                assert!(visited <= 32, "one-module products exceeded fixture bound");
                for entry in std::fs::read_dir(directory).unwrap() {
                    let entry = entry.unwrap();
                    let kind = entry.file_type().unwrap();
                    assert!(
                        !kind.is_symlink(),
                        "fixture products must remain under their owner"
                    );
                    if kind.is_dir() {
                        pending.push(entry.path());
                    } else if entry.file_name() == "CloseEvidence.hi" {
                        interface_directories.push(entry.path().parent().unwrap().to_owned());
                    }
                }
            }
            assert_eq!(
                interface_directories.len(),
                1,
                "actual GHC interface must identify one placed owner"
            );
            let owned = interface_directories.pop().unwrap();
            assert!(owned.starts_with(&logical));
            if obstruct {
                // The request is complete. Replace its actual placed directory
                // with a file so retirement fails independently of privileges.
                std::fs::remove_dir_all(&owned).unwrap();
                std::fs::write(&owned, b"owned scratch obstruction").unwrap();
                let independent = std::fs::read_dir(&owned).unwrap_err();
                assert_eq!(independent.kind(), std::io::ErrorKind::NotADirectory);
                *expected_os_cause.borrow_mut() =
                    Some((independent.raw_os_error(), independent.to_string()));
            }
            *physical.borrow_mut() = Some(owned);
            run.output.stdout
        },
    );
    std::fs::write(
        scratch.join("compiler-close-evidence.txt"),
        format!("{:#?}\n", retained_close.borrow()),
    )
    .unwrap();
    assert_eq!(
        Some(outcome.action),
        completed_body.into_inner(),
        "close must not replace completed compiler work"
    );
    assert_eq!(
        retained_close.into_inner(),
        Some(outcome.close.clone()),
        "the actual close sink must retain the same lifecycle evidence"
    );
    let owned = physical.into_inner().unwrap();
    if obstruct {
        let CompilerTransactionClose::Unconfirmed(evidence) = outcome.close else {
            panic!("actual filesystem close failure cannot confirm cleanup");
        };
        assert_eq!(
            evidence.reason,
            CompilerTransactionCloseReason::FrontendReportedFailure
        );
        let CompilerTransactionRetirement::Direct(retirement) = evidence.retirement else {
            panic!("actual frontend retirement must remain separate");
        };
        assert_eq!(retirement.exit.unwrap().code(), Some(2));
        assert_eq!(retirement.termination, CompilerTermination::NotRequested);
        let report = retirement.worker_report.unwrap();
        let CompilerWorkerRetirement::Reaped(status) = report.worker else {
            panic!("filesystem obstruction must follow successful actual worker reap");
        };
        assert!(status.success());
        let CompilerScratchRetirement::Unconfirmed(failures) = report.scratch else {
            panic!("exact filesystem cause must cross the frontend frame");
        };
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].path, owned);
        assert_eq!(
            failures[0].phase,
            tidepool_extract_cmd::frontend::ScratchCleanupPhase::Products
        );
        assert_eq!(failures[0].cause.kind, std::io::ErrorKind::NotADirectory);
        let (expected_os_error, expected_message) = expected_os_cause.into_inner().unwrap();
        assert_eq!(failures[0].cause.raw_os_error, expected_os_error);
        assert_eq!(failures[0].cause.message, expected_message);
        assert!(
            owned.is_file(),
            "unconfirmed scratch remains retained for diagnostics"
        );
        // The worker and frontend are actually reaped; only this test's exact
        // filesystem obstruction remains, and the test owner can now remove it.
        std::fs::remove_file(&owned).unwrap();
    } else {
        assert_eq!(outcome.close, CompilerTransactionClose::Clean);
        assert!(
            !owned.exists(),
            "successful normal END must retire its actual products"
        );
    }
    assert!(logical.is_dir());
    assert_eq!(
        std::fs::read(unrelated).unwrap(),
        b"retain caller ownership"
    );
}

#[test]
fn genuine_frontend_scratch_failure_preserves_completed_body_and_retained_close_receipt() {
    genuine_frontend_close_fixture(true);
}

#[test]
fn genuine_frontend_clean_end_retires_products_and_preserves_completed_body() {
    genuine_frontend_close_fixture(false);
}
