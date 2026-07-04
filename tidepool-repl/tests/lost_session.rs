//! `session_resume` error paths on the single implicit session.
//!
//! With one implicit session (auto-opened by `session_run`, reset by
//! `session_reset`) there is no named-session / process-scope story — but a
//! resume can still miss in three distinguishable ways, each a protocol-level
//! `McpError` (not a `CallToolResult`) with a clear message:
//!   1. no session is running yet (never ran) — `continuation_id` cannot resume;
//!   2. the session is running but NOT awaiting a resume (idle);
//!   3. the session IS suspended, but on a DIFFERENT continuation.
//!
//! Case 1 fires before any compile, so it needs no extract binary. Cases 2–3
//! need a real turn, so they gate on `extract_available`. All drive the real
//! `dispatch_tool` entry point.

mod common;

use common::*;
use serde_json::json;

/// `session_resume` whose miss is a protocol-level `McpError`: return its text.
async fn resume_err(repl: &Repl, continuation_id: &str, response: serde_json::Value) -> String {
    let mut args = serde_json::Map::new();
    args.insert("continuation_id".into(), json!(continuation_id));
    args.insert("response".into(), response);
    match repl.server.dispatch_tool("session_resume", args).await {
        Err(e) => e.message.to_string(),
        Ok(r) => panic!(
            "expected McpError from session_resume, got: {}",
            text_of(&r)
        ),
    }
}

/// Parse a `{"suspended":true,"continuation_id":"scont_N",...}` turn and return
/// the continuation id.
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

// ---------------------------------------------------------------------------
// Case 1 — resume before any session has run: no session is running.
// (Pure error path — fires before any compile, so no extract needed.)
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_with_no_session_errors() {
    let repl = Repl::new();
    let msg = resume_err(&repl, "scont_42", json!("whatever")).await;
    assert!(
        msg.contains("no session is running"),
        "should say no session is running: {msg}"
    );
    assert!(
        msg.contains("scont_42") && msg.contains("cannot be resumed"),
        "should keep the continuation_id context: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Case 2 — resume on an idle (running-but-not-suspended) session.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_when_not_suspended_errors() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    // A good turn auto-opens the session and leaves it Idle.
    repl.eval("pure (1 :: Int)").await.expect_ok("good eval");

    let msg = resume_err(&repl, "scont_1", json!("whatever")).await;
    assert!(
        msg.contains("not awaiting a resume"),
        "idle session resume should say it is not awaiting a resume: {msg}"
    );
    assert!(
        msg.contains("scont_1"),
        "should keep the continuation_id context: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Case 3 — suspended, but resume names a DIFFERENT continuation.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_wrong_continuation_while_suspended_errors() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    let t = repl.eval(r#"ask SNum "pick a number""#).await;
    assert!(!t.is_error, "ask should suspend: {}", t.text);
    let cont_id = parse_suspended(&t.text);

    // Resume a DIFFERENT continuation than the one pending.
    let msg = resume_err(&repl, "scont_does_not_exist", json!(7.0)).await;
    assert!(
        msg.contains("suspended on continuation") && msg.contains(&cont_id),
        "mismatch should name the pending continuation ({cont_id}): {msg}"
    );

    // The real continuation is still live — resume it to leave the session clean.
    repl.resume(&cont_id, json!(7.0))
        .await
        .expect_ok("resume the real continuation");
}
