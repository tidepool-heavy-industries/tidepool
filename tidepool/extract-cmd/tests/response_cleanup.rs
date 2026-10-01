//! Actual direct frontend cleanup after a worker advertises an oversized frame.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "bounded transport fixtures assert exact process outcomes"
)]

use std::fs::File;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tidepool_extract_cmd::{ExtractCmd, ResolvedExtractBin};

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn rejected_response(name: &str, transaction: bool) {
    if std::env::var("TIDEPOOL_RESPONSE_CLEANUP_CHILD").as_deref() == Ok(name) {
        let mut command = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(env!(
            "CARGO_BIN_EXE_tidepool-extract"
        )));
        command.input("Unused.hs").check_source();
        let endpoint = command.bind_direct().expect("matching worker binds");
        let error = if transaction {
            let mut transaction = endpoint.transaction().expect("worker accepts transaction");
            transaction.execute(&command).unwrap_err()
        } else {
            endpoint.execute(&command).unwrap_err()
        };
        assert!(
            !error.permits_rebind(),
            "accepted request cannot replay: {error}"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("worker.rs");
    let worker = dir.path().join("worker");
    std::fs::write(
        &source,
        include_str!("response_cleanup/oversized_worker.rs"),
    )
    .unwrap();
    #[allow(
        clippy::disallowed_methods,
        reason = "compile an owned ELF transport fixture"
    )]
    let status = Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(&worker)
        .status()
        .unwrap();
    assert!(status.success());
    let pid_file = dir.path().join("worker.pid");
    let count_file = dir.path().join("requests");
    let output_file = dir.path().join("child.log");
    let output = File::create(&output_file).unwrap();
    let command = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved("unused"));
    let flag = command.worker_argv()[0].clone();
    #[allow(
        clippy::disallowed_methods,
        reason = "owned libtest child has bounded wait and kill/reap guard"
    )]
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env("TIDEPOOL_RESPONSE_CLEANUP_CHILD", name)
            .env("TIDEPOOL_EXTRACT_WORKER", &worker)
            .env("TIDEPOOL_GHC_LIBDIR", "/unused/ghc/lib")
            .env("TIDEPOOL_TEST_WORKER_FLAG", flag)
            .env("TIDEPOOL_TEST_WORKER_PID", &pid_file)
            .env("TIDEPOOL_TEST_REQUEST_COUNT", &count_file)
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "bound frontend failed to settle after frame rejection"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let log = std::fs::read_to_string(output_file).unwrap();
    assert!(status.success(), "owned request helper failed: {log}");
    let pid: u32 = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "worker {pid} was not reaped: {log}"
    );
    assert_eq!(std::fs::read_to_string(count_file).unwrap(), "1\n");
    eprintln!("{name}: rejected submitted response, no replay, worker {pid} reaped\n{log}");
}

#[test]
fn direct_oversized_response_aborts_blocked_worker() {
    rejected_response("direct_oversized_response_aborts_blocked_worker", false);
}

#[test]
fn direct_transaction_oversized_response_aborts_blocked_worker() {
    rejected_response(
        "direct_transaction_oversized_response_aborts_blocked_worker",
        true,
    );
}
