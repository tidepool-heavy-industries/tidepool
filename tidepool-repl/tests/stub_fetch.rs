//! Truncation stubs are FETCHABLE — the `:stub <n>` round-trip through the
//! REAL `tidepool-repl` entry point (`dispatch_tool` over `session_run`, per
//! the standing rule; see `common/mod.rs`).
//!
//! Motivation (dogfooding): a large field rendered as `[~4470 chars -> stub_0]`
//! with NO way to dereference `stub_0` — the Haskell-side `paginateTrunc`
//! discarded the elided subtrees before the value reached Rust. The truncation
//! now happens Rust-side (`truncate.rs`), stashing the subtrees on the session
//! where `:stub <n> [page]` retrieves them in full.

mod common;
use common::*;

/// Extract the item-result JSON out of a `Turn` (the harness already unwraps
/// `items[0].result` to `Turn.text`).
fn parse_result(turn_text: &str) -> serde_json::Value {
    serde_json::from_str(turn_text)
        .unwrap_or_else(|e| panic!("item result is not JSON ({e}): {turn_text}"))
}

/// The whole arc in one session: an oversized object field truncates to a
/// `stub_0` marker with the self-teaching `:stub` hint; `:stub 0` round-trips
/// the FULL 4470-char content exactly; a second truncating result REPLACES the
/// stash (stub_0 now serves the new content); an unknown id errors
/// self-explainingly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stub_roundtrip_replace_and_unknown() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // 1. A >4096-char field: truncated with a fetchable stub_0 marker + hint.
    let t = repl
        .eval("pure (object [(\"region\", toJSON (T.replicate 4470 \"x\"))])")
        .await;
    let res = parse_result(t.expect_ok("oversized-field eval"));
    let region = res["value"]["region"]
        .as_str()
        .unwrap_or_else(|| panic!("region not a string: {res}"));
    assert_eq!(
        region, "[~4472 chars -> stub_0]",
        "marker must name stub_0 (4470 chars + 2 quotes)"
    );
    let hint = res["truncated"]
        .as_str()
        .unwrap_or_else(|| panic!("truncated hint missing: {res}"));
    assert!(
        hint.contains(":stub <n>") && hint.contains(":stub 0"),
        "hint must teach the :stub fetch: {hint}"
    );

    // 2. :stub 0 round-trips the full content EXACTLY.
    let t = repl.cmd(":stub 0").await;
    let fetched = parse_result(t.expect_ok(":stub 0"));
    assert_eq!(fetched["stub"], serde_json::json!(0));
    assert_eq!(
        fetched["value"].as_str().expect("stub value is the string"),
        "x".repeat(4470),
        ":stub 0 must return the full 4470-char content"
    );

    // 3. A second truncating result replaces the stash: stub_0 is the NEW content.
    let t = repl
        .eval("pure (object [(\"other\", toJSON (T.replicate 5000 \"y\"))])")
        .await;
    let res = parse_result(t.expect_ok("second oversized eval"));
    assert_eq!(
        res["value"]["other"],
        serde_json::json!("[~5002 chars -> stub_0]")
    );
    let t = repl.cmd(":stub 0").await;
    let fetched = parse_result(t.expect_ok(":stub 0 after replace"));
    assert_eq!(
        fetched["value"].as_str().expect("stub value is the string"),
        "y".repeat(5000),
        "the old stub_0 must now serve the NEW stash"
    );

    // 4. Unknown id: self-explaining error (how many stashed + replacement rule).
    let t = repl.cmd(":stub 99").await;
    let err = parse_result(&t.text);
    let msg = err["error"].as_str().expect(":stub 99 must error");
    assert!(
        msg.contains("no stub_99 stashed")
            && msg.contains("1 stub(s) available")
            && msg.contains("replaced by the next truncated result"),
        "self-explaining unknown-stub error: {msg}"
    );

    repl.close().await;
}

/// A value within the budget is untouched: no marker, no `truncated` key —
/// the additive-only JSON shape guarantee for existing consumers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn small_result_has_no_truncation_key() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let t = repl
        .eval("pure (object [(\"region\", toJSON (T.replicate 40 \"x\"))])")
        .await;
    let res = parse_result(t.expect_ok("small eval"));
    assert_eq!(res["value"]["region"], serde_json::json!("x".repeat(40)));
    assert!(
        res.get("truncated").is_none(),
        "no truncated key for an in-budget value: {res}"
    );

    repl.close().await;
}
