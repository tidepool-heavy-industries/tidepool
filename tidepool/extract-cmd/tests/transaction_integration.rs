#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert on known-good values; .clippy.toml allows this in test code"
)]
use tidepool_extract_cmd::{with_compiler_transaction, ExtractCmd};

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
    if std::env::var_os("TIDEPOOL_EXTRACT").is_none()
        || std::env::var_os("TIDEPOOL_EXTRACT_WORKER").is_none()
    {
        eprintln!("transaction_integration: SKIPPED (worktree frontend/worker not selected)");
        return;
    }
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
    let diagnostics = with_compiler_transaction(|| {
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
    });
    assert!(diagnostics[0].contains("tidepool-memo-miss module=Dep"));
    assert!(!diagnostics[1].contains("tidepool-memo-miss module=Dep"));
    assert!(diagnostics[2].contains("tidepool-memo-miss module=Dep"));

    std::fs::write(&source, "module Expr where\nresult =\n").unwrap();
    with_compiler_transaction(|| {
        let mut command = ExtractCmd::new().unwrap();
        command
            .input(&source)
            .output_dir(dir.join("failed"))
            .target("result")
            .include(&dir);
        let run = command.bind().unwrap().execute(&command).unwrap();
        assert!(!run.success(), "invalid source must be rejected");
    });

    std::fs::write(
        &source,
        "module Expr where\nimport Dep\nresult :: Int\nresult = dep + 2\n",
    )
    .unwrap();
    with_compiler_transaction(|| {
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
    });
}

/// `TIDEPOOL_MEMO_TRACE=1` is a diagnostic-only stderr emitter: it must
/// forward the two new `tidepool-memo-*` lines and change nothing about the
/// compiled artifact. Compile identical source with the flag off and on and
/// assert byte-identical stdout (the CBOR/artifact payload).
#[test]
fn memo_trace_flag_adds_diagnostics_without_changing_compiled_output() {
    if std::env::var_os("TIDEPOOL_EXTRACT").is_none()
        || std::env::var_os("TIDEPOOL_EXTRACT_WORKER").is_none()
    {
        eprintln!("memo_trace_flag: SKIPPED (worktree frontend/worker not selected)");
        return;
    }
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
    let without_trace = with_compiler_transaction(|| run_once("without-trace"));

    std::env::set_var("TIDEPOOL_MEMO_TRACE", "1");
    let with_trace = with_compiler_transaction(|| run_once("with-trace"));

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
