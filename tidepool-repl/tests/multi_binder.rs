//! DIMENSION: multi-binder / pattern binds.
//!
//! Drives the REAL `tidepool-repl` MCP entry point over real turns (per the
//! standing rule — no bespoke wiring) and probes what happens when a single
//! bind statement introduces MORE THAN ONE name: a tuple bind `(a, b) <- e`,
//! a `let`-tuple `let (x, y) = e`, a three-tuple, etc.
//!
//! BUG-5 SHIPS: flat-tuple multi-binder binds now root EACH component. A bind
//! `(a, b) <- pure (1, 2)` makes both `a` and `b` independently referenceable
//! and GC-safe. Nested tuple patterns also work (each variable is bound flat).
//!
//! REJECTION guard: patterns whose result type is not a matching tuple
//! (e.g. type errors, non-tuple constructors) still loudly reject via a GHC
//! compile error from the extract.
//!
//! Each test guards on `require_extract()` and panics loudly otherwise.

use crate::common;
use common::*;
use serde_json::json;

/// Parse a multi-item `.run()` block's raw envelope into its `items` array
/// (strips the optional `## Output\n...\n## Result\n` captured-output prefix,
/// same as `dispatch`'s callers elsewhere).
fn items_of(turn: &Turn) -> Vec<serde_json::Value> {
    let json_part = if let Some(pos) = turn.text.rfind("\n## Result\n") {
        &turn.text[pos + "\n## Result\n".len()..]
    } else {
        &turn.text
    };
    serde_json::from_str::<serde_json::Value>(json_part)
        .ok()
        .and_then(|v| v.get("items").and_then(|a| a.as_array()).cloned())
        .unwrap_or_default()
}

/// Item `idx`'s JSON `value` from a multi-item block: inline in `items[idx]`
/// unless it's the block's LAST executed item, whose value is hoisted to the
/// top-level `value` only (`Block::render`'s slim shape).
fn item_value(turn: &Turn, idx: usize, is_last: bool) -> serde_json::Value {
    if is_last {
        let json_part = if let Some(pos) = turn.text.rfind("\n## Result\n") {
            &turn.text[pos + "\n## Result\n".len()..]
        } else {
            &turn.text
        };
        return serde_json::from_str::<serde_json::Value>(json_part)
            .ok()
            .and_then(|v| v.get("value").cloned())
            .unwrap_or(serde_json::Value::Null);
    }
    items_of(turn)
        .get(idx)
        .and_then(|item| item.get("value"))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// CASE 1 — `let` single bind (control).
/// `let x = (5 :: Int)` then `x + 1` => 6. Pins down that the single-binder
/// `let` path is healthy (multi-bind must NOT interfere with single binds).
/// No cross-turn claim — one multi-item block proves it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn let_single_bind() {
    require_extract();
    let repl = Repl::new();

    let out = repl.run(&["let x = (5 :: Int)", "x + 1"]).await;
    let text = out.expect_ok("let x = 5 + x + 1");
    assert!(text.contains('6'), "let-single: expected 6, got: {text}");
}

/// CASE 2 — Tuple bind both components (the BUG-5 headline), including the
/// GC-rooting guard.
/// `(a, b) <- pure ((1 :: Int), (2 :: Int))` binds both `a` and `b`.
/// Both are independently referenceable; `a + b == 3`. Then a heavy foldl'
/// (~6 MiB into the 2 MiB nursery) forces real minor collections, and both
/// components must still resolve correctly afterward — a dangling slot would
/// crash or produce garbage instead of 1/2.
///
/// Pre-GC checks (bind + `:bindings` + both reads + sum) share ONE turn — none
/// of them make a cross-turn claim. The GC-forcing turn stands alone: the gap
/// it opens IS the property. Post-GC reads (`a`, `b`) share the final turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tuple_bind_both_components() {
    require_extract();
    let repl = Repl::new();

    let pre = repl
        .run(&[
            "(a, b) <- pure ((1 :: Int), (2 :: Int))",
            ":bindings",
            "a",
            "b",
            "a + b",
        ])
        .await;
    let text = pre.expect_ok("tuple bind + :bindings + a + b + a+b");
    eprintln!("[tuple_bind] pre-GC turn -> {text}");

    // Both components are in scope (item 1, `:bindings`).
    let bindings_text = items_of(&pre)
        .get(1)
        .map(|v| v.to_string())
        .unwrap_or_default();
    assert!(
        bindings_text.contains("\"a\"") && bindings_text.contains("\"b\""),
        "both a and b must be bound, got: {bindings_text}"
    );

    // Components are independently referenceable (items 2 and 3).
    assert_eq!(
        item_value(&pre, 2, false),
        json!(1),
        "a should be 1, got: {text}"
    );
    assert_eq!(
        item_value(&pre, 3, false),
        json!(2),
        "b should be 2, got: {text}"
    );

    // Sum is correct (item 4, the block's final item).
    assert_eq!(
        item_value(&pre, 4, true),
        json!(3),
        "expected a + b == 3, got: {text}"
    );

    // GC-rooting guard: heavy allocation (foldl' over 200k elements, ~6 MiB into
    // the 2 MiB nursery) forces real minor collections; both slots must still
    // resolve correctly afterward — a dangling slot would crash or produce
    // garbage instead of 1/2.
    let _ = repl
        .eval("foldl' (+) (0 :: Int) [1..200000]")
        .await
        .expect_ok("gc stressor");

    let post = repl.run(&["a", "b"]).await;
    let text = post.expect_ok("a + b post-GC");
    assert_eq!(
        item_value(&post, 0, false),
        json!(1),
        "post-GC a should be 1, got: {text}"
    );
    assert_eq!(
        item_value(&post, 1, true),
        json!(2),
        "post-GC b should be 2, got: {text}"
    );
}

/// CASE 3 — `let`-tuple works.
/// `let (x, y) = ((10 :: Int), (20 :: Int))` binds both x and y;
/// `x + y == 30`. No cross-turn claim — one multi-item block proves it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn let_tuple_works() {
    require_extract();
    let repl = Repl::new();

    let out = repl
        .run(&["let (x, y) = ((10 :: Int), (20 :: Int))", "x + y"])
        .await;
    let text = out.expect_ok("let-tuple bind + x + y");
    eprintln!("[let_tuple] turn -> is_error={} text={text}", out.is_error);
    assert!(text.contains("30"), "expected x + y == 30, got: {text}");
}

/// CASE 4 — Three-tuple works.
/// `(p, q, r) <- pure ((1::Int),(2::Int),(3::Int))` binds all three;
/// `p + q + r == 6`. No cross-turn claim — one multi-item block proves it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_tuple_works() {
    require_extract();
    let repl = Repl::new();

    let out = repl
        .run(&[
            "(p, q, r) <- pure ((1::Int),(2::Int),(3::Int))",
            "p + q + r",
        ])
        .await;
    let text = out.expect_ok("three-tuple bind + p+q+r");
    eprintln!(
        "[three_tuple] turn -> is_error={} text={text}",
        out.is_error
    );
    assert!(text.contains('6'), "expected p + q + r == 6, got: {text}");
}

/// CASE 5 — Type-mismatch multi-bind is LOUDLY REJECTED (GHC compile error).
/// `(a, b) <- pure (42 :: Int)` has 2 binders but the action returns a plain
/// `Int`, not a 2-tuple. GHC reports a type error at compile time — a loud,
/// clean rejection. The session must remain usable after the error.
///
/// The failing bind needs its own turn (it errors, so nothing after it in the
/// same block would run). The 3 recovery checks (nothing leaked, a fresh bind
/// works, it reads back) share ONE later turn — no cross-turn claim among them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mismatched_type_rejected_loudly() {
    require_extract();
    let repl = Repl::new();

    let bind = repl.eval("(a, b) <- pure (42 :: Int)").await;
    eprintln!(
        "[mismatch] bind turn -> is_error={} text={}",
        bind.is_error, bind.text
    );
    // Must error — can't match (a,b) pattern against Int.
    bind.expect_err("type-mismatch multi-bind should be rejected");

    let recovery = repl.run(&[":bindings", "let z = (9 :: Int)", "z"]).await;
    let text = recovery.expect_ok(":bindings after reject + let z + z");

    // Neither variable leaked into scope.
    let bindings_text = items_of(&recovery)
        .first()
        .map(|v| v.to_string())
        .unwrap_or_default();
    assert!(
        !bindings_text.contains("\"a\"") && !bindings_text.contains("\"b\""),
        "rejected bind must leave nothing bound, got: {bindings_text}"
    );

    // Session survived: a fresh bind works and reads back.
    assert!(text.contains('9'), "post-reject z: {text}");
}
