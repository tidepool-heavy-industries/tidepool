//! Lost-session self-explaining errors (all five flavors).
//!
//! A bare "no session 'x' open" was correct but unexplaining after an MCP
//! server restart: sessions are PROCESS-SCOPED — the resident machine and
//! every heap value die with the process; only declarations replay cheaply.
//! The error now states what happened and what to do, and distinguishes a
//! session closed in THIS process from one this process never opened.
//!
//! Every flavor here is a pure error path (it fires before any compile), so
//! none of these tests need the extract binary / GHC — no `extract_available`
//! gating. They drive the real `dispatch_tool` entry point.

mod common;

use common::*;
use serde_json::json;

/// The recovery guidance every lost-session flavor must carry: reopen with
/// session_open and redeclare (declarations replay cheaply; heap values gone).
fn assert_recovery_guidance(msg: &str, ctx: &str) {
    assert!(
        msg.contains("session_open"),
        "{ctx}: should point at session_open: {msg}"
    );
    assert!(
        msg.contains("redeclare") && msg.contains("declarations"),
        "{ctx}: should say declarations replay via redeclare: {msg}"
    );
    assert!(
        msg.contains("heap values are gone"),
        "{ctx}: should say heap values are gone: {msg}"
    );
}

/// The never-opened-in-this-process flavor: names the restart possibility and
/// that sessions are process-scoped (the machine + heap died with the process).
fn assert_never_opened_flavor(msg: &str, session: &str, ctx: &str) {
    assert!(
        msg.contains(&format!("no session '{session}' open in this server process")),
        "{ctx}: should name the session + this process: {msg}"
    );
    assert!(
        msg.contains("restarted"),
        "{ctx}: should mention the server-restart possibility: {msg}"
    );
    assert!(
        msg.contains("process-scoped"),
        "{ctx}: should say sessions are process-scoped: {msg}"
    );
    assert!(
        msg.contains("died with the old process"),
        "{ctx}: should say the resident machine died with the old process: {msg}"
    );
    assert_recovery_guidance(msg, ctx);
}

/// The closed-earlier-in-this-process flavor: distinguishable from the
/// never-opened flavor (no restart speculation — we KNOW it was closed here).
fn assert_closed_flavor(msg: &str, session: &str, ctx: &str) {
    assert!(
        msg.contains(&format!("no session '{session}' open")),
        "{ctx}: should name the session: {msg}"
    );
    assert!(
        msg.contains("closed earlier in this server process"),
        "{ctx}: should say it was closed in this process: {msg}"
    );
    assert!(
        !msg.contains("restarted"),
        "{ctx}: a session closed HERE must not speculate about a restart: {msg}"
    );
    assert_recovery_guidance(msg, ctx);
}

/// `session_run` a trivial item on a named session, returning the raw turn.
async fn run_named(repl: &Repl, session: &str) -> Turn {
    let mut args = serde_json::Map::new();
    args.insert("items".into(), json!(["pure (1 :: Int)"]));
    args.insert("session".into(), json!(session));
    let r = repl
        .server
        .dispatch_tool("session_run", args)
        .await
        .unwrap_or_else(|e| panic!("session_run transport error: {e:?}"));
    Turn {
        text: text_of(&r),
        is_error: r.is_error == Some(true),
    }
}

async fn open_named(repl: &Repl, session: &str) -> Turn {
    let mut args = serde_json::Map::new();
    args.insert("session".into(), json!(session));
    let r = repl
        .server
        .dispatch_tool("session_open", args)
        .await
        .unwrap_or_else(|e| panic!("session_open transport error: {e:?}"));
    Turn {
        text: text_of(&r),
        is_error: r.is_error == Some(true),
    }
}

async fn close_named(repl: &Repl, session: &str) -> Turn {
    let mut args = serde_json::Map::new();
    args.insert("session".into(), json!(session));
    let r = repl
        .server
        .dispatch_tool("session_close", args)
        .await
        .unwrap_or_else(|e| panic!("session_close transport error: {e:?}"));
    Turn {
        text: text_of(&r),
        is_error: r.is_error == Some(true),
    }
}

/// `session_resume`/`session_abort` on a missing session yield a protocol-level
/// `McpError` (not a `CallToolResult`); return its message text.
async fn resume_err(repl: &Repl, session: &str, continuation_id: &str) -> String {
    let mut args = serde_json::Map::new();
    args.insert("continuation_id".into(), json!(continuation_id));
    args.insert("response".into(), json!("whatever"));
    args.insert("session".into(), json!(session));
    match repl.server.dispatch_tool("session_resume", args).await {
        Err(e) => e.message.to_string(),
        Ok(r) => panic!("expected McpError from session_resume, got: {}", text_of(&r)),
    }
}

async fn abort_err(repl: &Repl, session: &str, continuation_id: &str) -> String {
    let mut args = serde_json::Map::new();
    args.insert("continuation_id".into(), json!(continuation_id));
    args.insert("session".into(), json!(session));
    match repl.server.dispatch_tool("session_abort", args).await {
        Err(e) => e.message.to_string(),
        Ok(r) => panic!("expected McpError from session_abort, got: {}", text_of(&r)),
    }
}

// ---------------------------------------------------------------------------
// Flavor 1 — session_run on a session this process never opened (typo, or a
// caller talking to a freshly started server).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_on_never_opened_session_explains_process_scope() {
    let repl = Repl::new();
    let t = run_named(&repl, "ghost").await;
    t.expect_err("run on never-opened session");
    assert_never_opened_flavor(&t.text, "ghost", "run/never-opened");
}

// ---------------------------------------------------------------------------
// Flavor 2 — session_run on a session opened and CLOSED in this process:
// distinguished from the never-opened flavor (no restart speculation).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_on_closed_session_says_closed_here() {
    let repl = Repl::new();
    open_named(&repl, "x").await.expect_ok("open x");
    close_named(&repl, "x").await.expect_ok("close x");
    let t = run_named(&repl, "x").await;
    t.expect_err("run on closed session");
    assert_closed_flavor(&t.text, "x", "run/closed");
}

// ---------------------------------------------------------------------------
// Flavor 3 — simulated server restart: session opened in process A, then A's
// server dies; the same name against a FRESH server B must get the
// never-opened-in-this-process flavor (restart possibility + process scope).
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_after_server_restart_explains_lost_session() {
    let server_a = Repl::new();
    open_named(&server_a, "x").await.expect_ok("open x on A");
    // The "restart": server A goes away entirely, taking its process-scoped
    // sessions (resident machine + heap) with it.
    drop(server_a);

    let server_b = Repl::new();
    let t = run_named(&server_b, "x").await;
    t.expect_err("run on pre-restart session name");
    assert_never_opened_flavor(&t.text, "x", "run/after-restart");
}

// ---------------------------------------------------------------------------
// Flavor 4 — session_resume on a missing session: same explanation, and the
// continuation_id stays in the message.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_on_missing_session_explains_and_names_continuation() {
    let repl = Repl::new();

    // Never-opened flavor.
    let msg = resume_err(&repl, "ghost", "scont_42").await;
    assert_never_opened_flavor(&msg, "ghost", "resume/never-opened");
    assert!(
        msg.contains("scont_42") && msg.contains("cannot be resumed"),
        "resume: should keep the continuation_id context: {msg}"
    );

    // Closed flavor through the same site.
    open_named(&repl, "y").await.expect_ok("open y");
    close_named(&repl, "y").await.expect_ok("close y");
    let msg = resume_err(&repl, "y", "scont_43").await;
    assert_closed_flavor(&msg, "y", "resume/closed");
    assert!(
        msg.contains("scont_43") && msg.contains("cannot be resumed"),
        "resume: should keep the continuation_id context: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Flavor 5 — session_abort on a missing session: same explanation, and the
// continuation_id stays in the message.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_on_missing_session_explains_and_names_continuation() {
    let repl = Repl::new();

    // Never-opened flavor.
    let msg = abort_err(&repl, "ghost", "scont_7").await;
    assert_never_opened_flavor(&msg, "ghost", "abort/never-opened");
    assert!(
        msg.contains("scont_7") && msg.contains("cannot be aborted"),
        "abort: should keep the continuation_id context: {msg}"
    );

    // Closed flavor through the same site.
    open_named(&repl, "z").await.expect_ok("open z");
    close_named(&repl, "z").await.expect_ok("close z");
    let msg = abort_err(&repl, "z", "scont_8").await;
    assert_closed_flavor(&msg, "z", "abort/closed");
    assert!(
        msg.contains("scont_8") && msg.contains("cannot be aborted"),
        "abort: should keep the continuation_id context: {msg}"
    );
}

// ---------------------------------------------------------------------------
// session_close shares the same lost-session error (scaffold wiring): closing
// a never-opened name explains itself too.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_on_never_opened_session_explains_process_scope() {
    let repl = Repl::new();
    let t = close_named(&repl, "ghost").await;
    t.expect_err("close on never-opened session");
    assert_never_opened_flavor(&t.text, "ghost", "close/never-opened");
}
