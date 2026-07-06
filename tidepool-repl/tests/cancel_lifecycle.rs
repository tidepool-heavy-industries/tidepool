//! Cancel-wedge regression suite: a cancelled turn must resolve its own state
//! (heap intact, session usable) instead of stranding at `Busy`.
//!
//! Before the fix, `drive` (the single writer of terminal state) ran inside the
//! RPC future; a client cancel — rmcp cancels `RequestContext.ct`, and over the
//! HTTP path may drop the future — left the terminal write unrun, wedging the
//! session at `Busy` with only `session_reset` (which DROPS THE HEAP) to recover.
//! The fix runs `drive` on a detached task and observes `context.ct`, so the turn
//! always resolves its own state.
//!
//! Requires `TIDEPOOL_EXTRACT` (see project CLAUDE.md); skips cleanly otherwise.

mod common;
use common::*;

use serde_json::json;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// The regression, reproduced through the ACTUAL failure mechanism: DROP the
/// turn's RPC future before it resolves (the harness dropping the request).
/// On the pre-fix code `drive` was awaited inline, so dropping the future left
/// the terminal-state write unrun → session stranded at `Busy` forever, only
/// `session_reset` (which drops the heap) to recover. With the detached
/// resolver the turn resolves its own state regardless: the earlier binding is
/// still readable and a fresh run is accepted (not busy-rejected).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_midturn_preserves_heap_and_unwedges() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();

    // Bind a value into the session heap.
    let bound = repl.eval("x <- pure (42 :: Int)").await;
    assert!(!bound.is_error, "bind should succeed: {}", bound.text);

    // Start a turn and DROP its future before it can resolve (a tiny timeout
    // fires while the worker is still compiling, so the awaiting future is
    // dropped mid-flight). This is the exact orphaning the bug turned into a
    // permanent wedge.
    let dropped =
        tokio::time::timeout(Duration::from_millis(50), repl.eval("pure (7 :: Int)")).await;
    assert!(
        dropped.is_err(),
        "test setup: the turn resolved before we could drop its future (raise nothing — \
         it means compile was instant; rerun): {dropped:?}"
    );

    // The heap survived AND the session unwedged: the earlier binding is still
    // readable and a fresh run is accepted (not busy-rejected on a stranded
    // `Busy`). Pre-fix this would loop until the poll bound and panic.
    let after = repl.eval_until_ok("x").await;
    assert!(
        after.text.contains("42"),
        "CANCEL-WEDGE REGRESSION: heap binding lost or session wedged after a dropped turn: {}",
        after.text
    );
}

/// The shared `cancel_slot` hazard: a cancel fires `CancelHandle::cancel()` on
/// the resident machine. That flag must NOT poison the next turn — `run_turn`
/// clears it at turn start (`session.rs` `reset_cancel`). Cancel a turn, then run
/// a normal COMPUTE turn and assert it produces its value (not a spurious abort).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_does_not_poison_next_turn() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();

    // Bootstrap the resident machine (publishes its cancel handle).
    let warm = repl.eval("pure (1 :: Int)").await;
    assert!(!warm.is_error, "warmup run should succeed: {}", warm.text);

    let ct = CancellationToken::new();
    ct.cancel();
    let _ = repl.eval_cancellable("pure (2 :: Int)", ct).await;

    // A fresh compute turn must run to completion — a leftover cancel flag would
    // abort it. `eval_until_ok` tolerates the resolver still settling the prior
    // turn's state.
    let after = repl.eval_until_ok("pure (99 :: Int)").await;
    assert!(
        after.text.contains("99"),
        "next turn was spuriously cancelled (poisoned cancel flag): {}",
        after.text
    );
}

/// The second `drive_detached` call site: cancelling a `session_resume` mid-turn
/// must recover the session the same way, not wedge it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_during_resume_preserves_session() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();

    // Suspend on an `ask`.
    let t = repl.eval(r#"ask SNum "pick a number""#).await;
    assert!(!t.is_error, "ask should suspend, not error: {}", t.text);
    let v: serde_json::Value = serde_json::from_str(&t.text)
        .unwrap_or_else(|_| panic!("expected suspended JSON: {}", t.text));
    let cont_id = v["continuation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no continuation_id in: {}", t.text))
        .to_string();

    // Cancel the resume mid-turn.
    let ct = CancellationToken::new();
    ct.cancel();
    let _ = repl.resume_cancellable(&cont_id, json!(7.0), ct).await;

    // The session recovers: a fresh run is accepted and returns its value.
    let after = repl.eval_until_ok("pure (5 :: Int)").await;
    assert!(
        after.text.contains('5'),
        "session did not recover after a cancelled resume: {}",
        after.text
    );
}
