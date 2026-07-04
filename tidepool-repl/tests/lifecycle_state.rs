//! Lifecycle state-machine regression suite (the `SessionState` refactor).
//!
//! These guard the concurrency bug class the smeared lifecycle allowed, all
//! through the real `dispatch_tool` entry point:
//!   - H1: `session_reset` while a turn is suspended on an `ask` must NOT hang
//!     (the worker is parked on `response_rx`, not the command channel; reset
//!     releases the suspension so it can unwind and observe the `Close`) and it
//!     must DROP the pending ask (abort folds into reset).
//!   - M5: a `session_run` on a suspended session is REJECTED with a clear error
//!     (the busy-guard), not silently queued behind the parked worker.
//!   - H2: an abandoned suspension (never resumed) is reaped back to `Idle` so it
//!     doesn't leak a worker thread + JIT machine.
//!   - H3: a runaway turn is cancelled at a JIT safepoint and self-heals to Idle.
//!
//! Requires `TIDEPOOL_EXTRACT` (see project CLAUDE.md); skips cleanly otherwise.

mod common;
use common::*;

use serde_json::json;
use std::time::Duration;

/// Parse a `{"suspended":true,"continuation_id":"scont_N",...}` turn and return
/// the continuation id (asserting the turn actually suspended).
fn parse_suspended(text: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(text)
        .unwrap_or_else(|_| panic!("expected JSON from suspended turn, got: {text}"));
    assert_eq!(
        v["suspended"],
        json!(true),
        "turn was not suspended: {text}"
    );
    v["continuation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no continuation_id in: {text}"))
        .to_string()
}

/// H1: resetting a session that is parked on an `ask` must return promptly — it
/// must NOT deadlock on `WorkerHandle::shutdown`'s `join()` — and it must DROP
/// the pending continuation (abort folds into reset). We wrap the reset in a
/// generous timeout: a hang fails the test instead of stalling the suite.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_while_suspended_drops_ask_and_recovers() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();

    // Suspend on an `ask` and DO NOT resume it.
    let t = repl.eval(r#"ask SNum "pick a number""#).await;
    assert!(!t.is_error, "ask should suspend, not error: {}", t.text);
    let cont_id = parse_suspended(&t.text);
    assert!(
        cont_id.starts_with("scont_"),
        "unexpected cont id: {cont_id}"
    );

    // Reset WITHOUT resuming — must come back well within the worker's 30s ack
    // window, not hang.
    let reset = tokio::time::timeout(Duration::from_secs(45), repl.reset()).await;
    let turn = reset.expect("H1 REGRESSION: session_reset hung while a continuation was parked");
    assert!(
        turn.contains("reset"),
        "reset should report reset, got: {}",
        turn.text
    );

    // The pending ask was DROPPED: resuming the old continuation now fails
    // (the fresh session is not awaiting it).
    let stale = repl.resume(&cont_id, json!(7.0)).await;
    assert!(
        stale.is_error,
        "resuming a reset-away continuation should fail, got ok: {}",
        stale.text
    );

    // The fresh session is usable.
    let after = repl.eval("pure (1 :: Int)").await;
    assert!(
        !after.is_error,
        "post-reset run should work: {}",
        after.text
    );
}

/// M5: a `session_run` while the session is suspended on an `ask` must be
/// rejected with a clear busy/suspended error (not queued behind the parked
/// worker, where it would later mutate state against a dropped listener).
/// Resuming the original continuation must then still work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_on_suspended_session_is_rejected() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();

    let t = repl.eval(r#"ask SNum "pick a number""#).await;
    assert!(!t.is_error, "ask should suspend: {}", t.text);
    let cont_id = parse_suspended(&t.text);

    // A second run while suspended → rejected (busy-guard).
    let busy = repl.eval("pure (1 :: Int)").await;
    assert!(
        busy.is_error,
        "M5: run on a suspended session should be rejected, got ok: {}",
        busy.text
    );
    assert!(
        busy.text.contains("suspended") && busy.text.contains(&cont_id),
        "M5: rejection should name the suspension + continuation id, got: {}",
        busy.text
    );

    // The original continuation is still live — resume succeeds.
    let resumed = repl.resume(&cont_id, json!(7.0)).await;
    assert!(
        !resumed.is_error,
        "resume after a rejected concurrent run should work: {}",
        resumed.text
    );

    // And the session is back to Idle: a fresh run now succeeds.
    let after = repl.eval("pure (2 :: Int)").await;
    assert!(
        !after.is_error,
        "post-resume run should work: {}",
        after.text
    );
    assert!(
        after.text.contains('2'),
        "post-resume value: {}",
        after.text
    );
}

/// H3 (self-healing): a turn that runs away past the turn budget is cancelled at
/// a JIT safepoint, and the session RECOVERS to `Idle` — a follow-up `session_run`
/// succeeds instead of being busy-rejected on a stuck `Wedged`. This guards the
/// cooperative-cancel wiring (the resident machine's `CancelHandle`, published by
/// the worker, fired on timeout). Before it, an allocating/tail runaway wedged the
/// session until reset/reap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timed_out_runaway_self_heals_to_idle() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    // Turn budget must exceed the per-eval GHC compile so the timeout fires
    // during the LOOP, not compilation. A novel expression compiles in ~13s
    // quiet, but under a full-parallel `cargo nextest` run the extract compile
    // is CPU-starved and can crawl well past that; 75s gives headroom for the
    // starvation the ghc-heavy test-group cap already bounds (see
    // .config/nextest.toml), while the reaper TTL stays far above it so
    // recovery still comes from cancel alone, not reaping.
    let repl = Repl::with_timeout(Duration::from_secs(75), Duration::from_secs(300));

    // A pure allocating runaway — `sum` over an infinite `[Int]` never returns
    // and allocates cons cells, so it polls the JIT gc safepoint. The bare-expr
    // lane wants an `Eff … a`; the result is forced by the render INSIDE the
    // do-block (during the run), so this loops where the cancel flag is polled.
    // It is the session's FIRST turn: the machine publishes its cancel handle
    // the instant it bootstraps (before the loop), so even a first-turn runaway
    // is abortable.
    let runaway = repl.eval("pure (sum [1 :: Int ..])").await;
    assert!(
        runaway.is_error,
        "runaway should time out, not return: {}",
        runaway.text
    );
    assert!(
        runaway.text.contains("timed out"),
        "expected a timeout error, got: {}",
        runaway.text
    );
    assert!(
        runaway.text.contains("recovered"),
        "H3 REGRESSION: the runaway timed out but the session did NOT self-heal \
         (cancel safepoint never fired): {}",
        runaway.text
    );

    // The session recovered to Idle: a fresh run is ACCEPTED (not busy-rejected on
    // a stuck Wedged) and returns its value.
    let after = repl.eval("pure (42 :: Int)").await;
    assert!(
        !after.is_error,
        "H3: after a cancelled runaway the session should be Idle and accept a fresh \
         run, got: {}",
        after.text
    );
    assert!(
        after.text.contains("42"),
        "post-recovery value: {}",
        after.text
    );
}

/// H2: an abandoned suspension (never resumed) is reaped back to `Idle` after the
/// TTL, freeing the worker for reuse (no thread/heap leak). We build a server
/// with a tiny TTL, suspend, wait past it, and confirm a fresh run succeeds —
/// which the busy-guard would reject if the session were still Suspended.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandoned_suspension_is_reaped_to_idle() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::with_ttl(Duration::from_millis(300));

    let t = repl.eval(r#"ask SNum "pick a number""#).await;
    assert!(!t.is_error, "ask should suspend: {}", t.text);
    let _cont_id = parse_suspended(&t.text);

    // Wait well past the TTL (reaper sweeps ~4x per TTL ⇒ tick ~75ms).
    tokio::time::sleep(Duration::from_millis(2000)).await;

    // The reaper should have dropped the abandoned suspension and returned the
    // session to Idle — so a fresh run is accepted (not busy-rejected).
    let after = repl.eval("pure (42 :: Int)").await;
    assert!(
        !after.is_error,
        "H2: after the reaper reclaims an abandoned suspension the session should be Idle \
         and accept a fresh run, got: {}",
        after.text
    );
    assert!(after.text.contains("42"), "post-reap value: {}", after.text);
}
