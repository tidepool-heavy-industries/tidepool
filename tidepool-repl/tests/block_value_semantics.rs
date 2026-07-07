//! `run_block`'s top-level `value`/`truncated` dedup: F1 (strips fields from
//! the WRONG item when a value-producing item is followed by an unrelated
//! Meta payload that also carries a `value` key, e.g. `:stub`) and F2 (a
//! block ending in a bind/meta must leave the top-level `value` null instead
//! of leaking an EARLIER expression's value — see
//! `plans/repo-review-2026-07-06/06-repl-session.md`).
//!
//! Both bugs live in the same `run_block` post-loop step, fixed together: the
//! item to strip is recorded by INDEX at the moment `TurnOutcome::Value` sets
//! `last_value`, and that recorded value only survives if it is also the
//! FINAL executed item (otherwise the block didn't end in an expression, so
//! the top-level `value` is null and nothing is stripped).

mod common;
use common::*;

/// Parse the full slim block envelope (not just `items[0]`, unlike
/// `common::Repl::run_block_single`'s per-item unwrap) — these tests assert on
/// cross-item interactions the single-item helper can't see.
fn envelope(text: &str) -> serde_json::Value {
    let json_part = if let Some(pos) = text.rfind("\n## Result\n") {
        &text[pos + "\n## Result\n".len()..]
    } else {
        text
    };
    serde_json::from_str(json_part)
        .unwrap_or_else(|e| panic!("not a JSON block envelope ({e}): {text}"))
}

/// Failure A (F1, data loss): a value-producing expression that TRUNCATES,
/// followed by a `:stub 0` fetch in the SAME block. Before the fix, the
/// post-loop dedup found ":stub 0"'s Meta result as "the last ok item" and
/// stripped ITS `value` — destroying the fetched content — while top-level
/// `value` still showed item 0's truncated value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stub_item_after_truncating_expr_keeps_its_own_value() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();

    let t = repl
        .run(&[
            "pure (object [(\"region\", toJSON (T.replicate 4470 \"x\"))])",
            ":stub 0",
        ])
        .await;
    assert!(t.ok(), "block should succeed: {}", t.text);
    let env = envelope(&t.text);

    // item 1 (`:stub 0`) must retain ITS OWN `value` — the full fetched
    // content — not have it stripped by item 0's unrelated dedup.
    let item1 = &env["items"][1];
    assert_eq!(item1["stub"], serde_json::json!(0), "envelope: {env}");
    assert_eq!(
        item1["value"].as_str().unwrap_or_else(|| panic!(
            "F1 regression: stub's value was stripped by an unrelated item's \
             dedup: {env}"
        )),
        "x".repeat(4470),
        ":stub 0 must round-trip the full truncated content: {env}"
    );

    // The block does not end in an expression (it ends in the `:stub` meta
    // command), so per F2 the top-level `value` must be null.
    assert!(
        env["value"].is_null(),
        "block ends in `:stub`, not an expression — top-level value must be null: {env}"
    );
}

/// Failure B (F2): a block ending in a bind must leave the top-level `value`
/// null, not repeat an EARLIER expression's value. Before the fix, `last_value`
/// was set by ANY value-producing item, so a trailing bind after an
/// expression leaked the expression's value to the top level (exactly the
/// duplication the dedup step exists to eliminate, just misapplied).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_ending_in_bind_leaves_top_level_value_null_no_duplication() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();

    let t = repl.run(&["2 + 2", "x <- pure (5 :: Int)"]).await;
    assert!(t.ok(), "block should succeed: {}", t.text);
    let env = envelope(&t.text);

    assert!(
        env["value"].is_null(),
        "block ends in a bind, not the earlier expression — top-level value must be null: {env}"
    );
    // The non-final expression keeps its OWN inline value exactly once (the
    // documented "non-final expression" shape) — not duplicated anywhere else.
    assert_eq!(env["items"][0]["value"], serde_json::json!(4), "envelope: {env}");
    assert_eq!(env["items"][1]["bound"], serde_json::json!("x"), "envelope: {env}");
}

/// Control: a block ending in a bare expression still populates the top-level
/// `value`/`type` and suppresses the duplicate inline copy on that FINAL item
/// — the good path the F1/F2 fix must not regress.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_ending_in_expression_populates_top_level_and_suppresses_item_value() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();

    let t = repl.run(&["x <- pure (5 :: Int)", "x + 1"]).await;
    assert!(t.ok(), "block should succeed: {}", t.text);
    let env = envelope(&t.text);

    assert_eq!(env["value"], serde_json::json!(6), "envelope: {env}");
    assert!(
        env["items"][1].get("value").is_none(),
        "final expression item must not duplicate `value` inline: {env}"
    );
}
