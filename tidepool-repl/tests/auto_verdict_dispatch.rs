//! REPL `Auto` items dispatch directly from a present GHC classification.
//!
//! A bare expression classified as `Expr` must compile only the expression
//! target; it must not also probe the declaration target.

mod common;
use common::*;

use std::os::unix::fs::PermissionsExt;

/// An `Auto` item classified `Expr` compiles `__result` exactly once and never
/// attempts a declaration compile targeting `result`. Conversely, a missing
/// classification artifact stops after the classifier spawn and cannot launch
/// either compile route.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_expr_verdict_routes_once_but_missing_artifact_does_not_route() {
    require_extract();
    let real_extract =
        std::env::var("TIDEPOOL_EXTRACT").expect("require_extract() installs TIDEPOOL_EXTRACT");

    // This test observes process-boundary requests. A resident daemon would
    // receive them over its socket and correctly bypass the wrapper below.
    std::env::remove_var("TIDEPOOL_EXTRACT_DAEMON_SOCKET");

    // A logging, delegating wrapper: record every invocation's argv, then
    // exec the real extract so the turn actually runs to completion.
    let wrap_dir = tempfile::TempDir::new().unwrap();
    let log_path = wrap_dir.path().join("calls.log");
    let wrapper = wrap_dir.path().join("tidepool-extract-logging");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\necho \"$@\" >> {}\nexec {} \"$@\"\n",
            log_path.display(),
            real_extract
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("TIDEPOOL_EXTRACT", &wrapper);

    let repl = Repl::new();
    // A bare expression: no leading decl keyword (`Auto`), and GHC's
    // decl-context parse genuinely fails on it, so its batch verdict is
    // `Expr` — the exact shape item 13 is about.
    let out = repl.eval_ok("pure (1 :: Int)").await;
    assert!(
        out.contains('1'),
        "expected the turn to succeed with value 1, got: {out}"
    );

    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    let requests: Vec<tidepool_extract_cmd::ExtractRequest> = log
        .lines()
        .map(|line| {
            let argv = line.split_whitespace().map(Into::into).collect::<Vec<_>>();
            tidepool_extract_cmd::ExtractRequest::decode_worker_argv(&argv)
                .unwrap_or_else(|error| panic!("invalid logged worker request: {error}: {line}"))
        })
        .collect();
    let target_count = |target: &str| {
        requests
            .iter()
            .filter(|request| request.target_names().iter().any(|name| name == target))
            .count()
    };
    let decl_probe_attempts = target_count("result");
    assert_eq!(
        decl_probe_attempts, 0,
        "an Auto item with a precomputed Expr verdict must never attempt a \
         declaration compile (target `result`) — it should dispatch \
         straight to run_eval. Full extract call log:\n{log}"
    );

    let expr_compile_attempts = target_count("__result");
    assert_eq!(
        expr_compile_attempts, 1,
        "expected exactly one expression compile (target `__result`) for \
         this turn. Full extract call log:\n{log}"
    );

    // Fault phase: the extractor exits successfully but omits classify.json.
    // `classify_block` therefore returns a typed missing-artifact failure.
    // That failure must end the block after this ONE spawn, rather than being
    // rendered and sniffed as permission to try a declaration or expression.
    let fault_log = wrap_dir.path().join("fault-calls.log");
    let faulty = wrap_dir.path().join("tidepool-extract-missing-classify");
    std::fs::write(
        &faulty,
        format!(
            "#!/bin/sh\necho \"$@\" >> {}\nexit 0\n",
            fault_log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&faulty, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("TIDEPOOL_EXTRACT", &faulty);

    let fault = repl.eval("pure (2 :: Int)").await;
    assert!(
        fault.is_error,
        "missing classifier artifact must fail: {fault:?}"
    );
    assert!(
        fault.text.contains("**failure-class:** `infra`"),
        "missing classifier artifact must retain its infrastructure class: {fault:?}"
    );
    let fault_calls = std::fs::read_to_string(&fault_log).unwrap_or_default();
    assert_eq!(
        fault_calls.lines().count(),
        1,
        "missing classifier artifact must not launch a fallback compile: {fault_calls}"
    );
}
