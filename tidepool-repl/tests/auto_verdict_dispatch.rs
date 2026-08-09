//! Codex review item 13 — REPL `Auto` items must dispatch straight from a
//! present classify verdict instead of paying for a doomed declaration probe
//! GHC's own parser already ruled out.
//!
//! Key source under test: `tidepool-repl/src/session.rs` (`run_one_item`'s
//! `Auto` arm) and `tidepool-runtime/src/session/mod.rs`
//! (`validate_candidate`, the decl-plane's `--target result` extract spawn).
//!
//! The old cascade unconditionally tried `run_def` (a decl-plane compile,
//! `--target result`) before falling back to `run_eval` on a GHC parse
//! error. For a bare expression (whose block-batch verdict already says
//! `Expr`), that probe was GUARANTEED to fail — GHC's classify verdict and
//! the decl-context parse agree, so the probe never had a chance of
//! succeeding. This test drives the real `tidepool-repl` entry point through
//! a logging/delegating `TIDEPOOL_EXTRACT` wrapper and asserts NO
//! `--target result` invocation happens for an `Auto` expression turn.

mod common;
use common::*;

use std::os::unix::fs::PermissionsExt;

/// An `Auto` item classified `Expr` by the block's batch verdict must never
/// attempt a declaration compile. Pinned red-then-green: reverting
/// `run_one_item`'s `Auto` arm to the unconditional try-cascade (attempt
/// `run_def` first, regardless of `verdict`) makes this fail — the log picks
/// up a `--target result` decl-probe spawn before the real `--target
/// __result` expression compile.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_expr_verdict_skips_declaration_probe() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let real_extract =
        std::env::var("TIDEPOOL_EXTRACT").expect("extract_available() installs TIDEPOOL_EXTRACT");

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
    let decl_probe_attempts = log
        .lines()
        .filter(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words
                .windows(2)
                .any(|w| w[0] == "--target" && w[1] == "result")
        })
        .count();
    assert_eq!(
        decl_probe_attempts, 0,
        "an Auto item with a precomputed Expr verdict must never attempt a \
         declaration compile (`--target result`) — it should dispatch \
         straight to run_eval. Full extract call log:\n{log}"
    );

    let expr_compile_attempts = log
        .lines()
        .filter(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words
                .windows(2)
                .any(|w| w[0] == "--target" && w[1] == "__result")
        })
        .count();
    assert_eq!(
        expr_compile_attempts, 1,
        "expected exactly one expression compile (`--target __result`) for \
         this turn. Full extract call log:\n{log}"
    );
}
