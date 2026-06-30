//! BUG-9 regression: `ask` resume reply must arrive as a structured,
//! optic-extractable `Value` — not a raw JSON string.
//!
//! Before the fix (server.rs session_resume, commit 7c8ff14), the continuation
//! received whatever raw JSON value the caller sent; a response that arrived as
//! a JSON string (the common LLM-reply failure mode, aka #315) was delivered as
//! `Value::String(...)` so `v ^? key "n" . _Double` returned `Nothing`.  The
//! fix calls `tidepool_mcp::validate::validate_response` BEFORE consuming the
//! continuation, which (a) canonicalises a stringified-JSON reply into its
//! parsed shape and (b) leaves an invalid reply's continuation un-consumed so
//! the caller can retry with a corrected response.
//!
//! Requires `TIDEPOOL_EXTRACT` (see project CLAUDE.md); skips cleanly otherwise.

mod common;
use common::*;

use serde_json::json;

// ────────────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────────────

/// Call `session_resume` directly (the `Repl` wrapper doesn't expose it yet).
async fn resume(repl: &Repl, continuation_id: &str, response: serde_json::Value) -> Turn {
    let mut args = serde_json::Map::new();
    args.insert("continuation_id".into(), json!(continuation_id));
    args.insert("response".into(), response);
    let r = repl
        .server
        .dispatch_tool("session_resume", args)
        .await
        .unwrap_or_else(|e| panic!("session_resume transport error: {e:?}"));
    Turn {
        text: text_of(&r),
        is_error: r.is_error == Some(true),
    }
}

/// Extract a number from a `{type,value}` envelope's `value`, tolerant of either
/// a JSON number or a Show-rendered numeric string (`"7.0"`) — Show-default
/// rendering returns the latter for a scalar `Double`.
fn as_num(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}

/// Parse the `{"suspended":true,"continuation_id":"scont_N",...}` JSON returned
/// by a suspended `session_eval` turn; assert the shape is correct and return
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

// ────────────────────────────────────────────────────────────────────────────
// BUG-9 gate: object reply must be optic-extractable
// ────────────────────────────────────────────────────────────────────────────

/// The core BUG-9 regression: resume with `{"n": 5}` and confirm the Haskell
/// computation receives a structured `Value` whose `"n"` field is extractable
/// with `key "n" . _Double`.
///
/// `fromMaybe (-1.0)` is the witness: if the optic succeeds the result is 5.0;
/// if it fails (bug: Value was a String) the result is -1.0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn object_reply_is_extractable_value() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // Eval: ask with an object schema; extract field "n" via optic.
    // fromMaybe (-1.0) distinguishes Just 5.0 (fix: structured Value) from
    // Nothing (bug: Value was a raw JSON string, optic returned Nothing).
    let t = repl
        .eval(
            r#"ask (SObj [("n", SNum)]) "pick" <&> (\v -> fromMaybe (-1.0 :: Double) (v ^? key "n" . _Double))"#,
        )
        .await;
    // The eval must suspend on `ask`, not error.
    assert!(!t.is_error, "eval should suspend, not error: {}", t.text);
    let cont_id = parse_suspended(&t.text);
    assert!(
        cont_id.starts_with("scont_"),
        "unexpected continuation_id format: {cont_id}"
    );

    // Resume with a valid object.  The fix canonicalises and delivers a proper
    // structured Value; the optic then extracts 5.0.
    let t = resume(&repl, &cont_id, json!({"n": 5})).await;
    assert!(!t.is_error, "resume errored: {}", t.text);

    // Parse the rendered {type, value} envelope and extract the Double.
    let envelope: serde_json::Value = serde_json::from_str(t.text.trim())
        .unwrap_or_else(|_| panic!("BUG-9: result should be a Double, got: {}", t.text));
    let result = as_num(&envelope["value"])
        .unwrap_or_else(|| panic!("BUG-9: result should be a Double, got: {}", t.text));
    assert!(
        (result - 5.0).abs() < 1e-9,
        "BUG-9: expected 5.0 from optic extraction (structured Value), got {} \
         (if -1.0, the Value was delivered as a raw JSON string — fix is incomplete)",
        result
    );

    repl.close().await;
}

// ────────────────────────────────────────────────────────────────────────────
// Scalar resume: SNum → value extractable via _Double
// ────────────────────────────────────────────────────────────────────────────

/// `ask SNum` with a bare number response: the delivered `Value` must be a
/// JSON number so `v ^? _Double` succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scalar_reply_extracts_via_double() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl
        .eval(r#"ask SNum "enter a number" <&> (\v -> fromMaybe (-1.0 :: Double) (v ^? _Double))"#)
        .await;
    assert!(!t.is_error, "eval should suspend: {}", t.text);
    let cont_id = parse_suspended(&t.text);

    let t = resume(&repl, &cont_id, json!(7.0)).await;
    assert!(!t.is_error, "resume errored: {}", t.text);

    let envelope: serde_json::Value = serde_json::from_str(t.text.trim())
        .unwrap_or_else(|_| panic!("scalar result should be a Double, got: {}", t.text));
    let result = as_num(&envelope["value"])
        .unwrap_or_else(|| panic!("scalar result should be a Double, got: {}", t.text));
    assert!(
        (result - 7.0).abs() < 1e-9,
        "expected 7.0 from scalar extraction, got {}",
        result
    );

    repl.close().await;
}

// ────────────────────────────────────────────────────────────────────────────
// Invalid reply: continuation NOT consumed, retry with valid reply succeeds
// ────────────────────────────────────────────────────────────────────────────

/// An invalid reply (wrong schema shape) must NOT consume the continuation —
/// the continuation_id must remain live for a subsequent valid resume.
///
/// Protocol: resume with wrong shape → error containing `continuation_not_consumed`
///           → resume AGAIN with the SAME continuation_id and valid response → success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_reply_does_not_consume_continuation() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl
        .eval(r#"ask SNum "enter a number" <&> (\v -> fromMaybe (-1.0 :: Double) (v ^? _Double))"#)
        .await;
    assert!(!t.is_error, "eval should suspend: {}", t.text);
    let cont_id = parse_suspended(&t.text);

    // First resume: wrong shape — a string where a number is expected.
    let t = resume(&repl, &cont_id, json!("not a number")).await;
    assert!(
        t.is_error,
        "invalid reply should produce a validation error, got ok: {}",
        t.text
    );
    assert!(
        t.text.contains("continuation_not_consumed"),
        "error must advertise that the continuation was NOT consumed (so it can be retried): {}",
        t.text
    );

    // Second resume: same continuation_id, now with a valid number — must succeed.
    let t = resume(&repl, &cont_id, json!(42.0)).await;
    assert!(
        !t.is_error,
        "retry with valid reply should succeed, got error: {}",
        t.text
    );
    let envelope: serde_json::Value = serde_json::from_str(t.text.trim())
        .unwrap_or_else(|_| panic!("retry result should be a Double, got: {}", t.text));
    let result = as_num(&envelope["value"])
        .unwrap_or_else(|| panic!("retry result should be a Double, got: {}", t.text));
    assert!(
        (result - 42.0).abs() < 1e-9,
        "expected 42.0 from retry resume, got {}",
        result
    );

    repl.close().await;
}
