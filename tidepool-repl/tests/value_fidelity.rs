//! Wave 3b hardening — DIMENSION E: value-plane fidelity.
//!
//! Adversarial integration tests driving the REAL `tidepool-repl` entry point
//! (session_run — the harness `def`/`eval`/`cmd`
//! helpers are thin 1-item `session_run` wrappers) over multiple turns, per the
//! standing rule (production tool dispatch over real turns, organic GC on the
//! 2 MiB nursery — never a bespoke harness or forced GC).
//!
//! Focus: do bound values of every interesting SHAPE round-trip with fidelity —
//! Text (first-class), values that reference earlier bindings at bind time,
//! Tier-1 closures capturing a session value that must survive GC,
//! nested/recursive ADTs (real con names, not `<unknown>`), Maybe/Either,
//! structured JSON `Value` (Tier-0 + DataConTable), and lists.
//!
//! Each test panics loudly when the session-aware extract is unavailable, and
//! The session auto-opens on the first `session_run`; teardown is implicit.
//!
//! Regression gate: these tests guard the now-fixed kind=4 TypeMetadata force bug
//! (fixed in GhcPipeline.runSessionPipeline — see text_bind.rs header for the
//! root cause). All tests assert SUCCESS; inline regression-witness comments mark
//! the turns that previously fired the kind=4 crash.

mod common;
use common::*;
use serde_json::json;

/// CASE 1 — Text is first-class: bind a Text, then read/transform it.
///
/// `s <- pure (T.pack "hi")` ; `T.length s` => 2 ; `T.unpack s` ; `T.toUpper s`.
/// Text binds (Tier-0 deep_force of a Text heap object) + references back —
/// no cross-turn claim, so one multi-item block proves all four facts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_first_class_bind_and_reference() {
    require_extract();
    let repl = Repl::new();

    // Regression witness: was the kind=4 TypeMetadata crash (now fixed). Previously
    // materializing a Text result fired "forced type metadata (should be dead code)"
    // even with a valid bound `s`; the fix in GhcPipeline.runSessionPipeline
    // resolved it. Now asserts success.
    let out = repl
        .run(&[
            "s <- pure (T.pack \"hi\")",
            "T.length s",
            "T.unpack s",
            "pure (T.toUpper s)",
        ])
        .await;
    let text = out.expect_ok("text bind + 3 read-back forms");
    assert!(
        text.contains("\"value\":2"),
        "T.length s: expected 2, got: {text}"
    );
    assert!(text.contains("hi"), "T.unpack s: expected hi, got: {text}");
    assert!(text.contains("HI"), "T.toUpper s: expected HI, got: {text}");
}

/// CASE 2 — THE KEY DIAGNOSTIC: a Tier-0 Text bind under a DIFFERENT name while
/// a prior (Int) binding is live. NO rebind of the same name.
///
/// open; `x <- pure (1 :: Int)`; `y <- pure (T.pack "hi")`; `T.length y` => 2.
///
/// We have a CONFIRMED bug where rebinding to Text crashes with
/// `kind=4 TypeMetadata` in Tier-0 deep_force. This isolates WHETHER the crash
/// needs the same name:
///   - If THIS crashes → bug is GENERAL: "Tier-0 Text bind crashes whenever ANY
///     prior binding is live (even a different name)".
///   - If THIS passes → the crash is specific to rebinding the SAME name.
///
/// VERDICT (this run): THIS CRASHES. `y <- pure (T.pack "hi")` with the Int `x`
/// live dies at BIND time with `kind=4 (TypeMetadata) msg="hi"` →
/// "bind runtime error: ... forced type metadata (should be dead code)".
/// Corroborated by `text_first_class_bind_and_reference`, where binding a Text as
/// the FIRST/ONLY binding (no prior) does NOT crash. ⇒ The bug is GENERAL: a
/// Tier-0 Text bind crashes whenever ANY prior binding is live (even a DIFFERENT
/// name). It is NOT rebind-same-name-specific. Root-cause hypothesis: with a
/// prior binding live, the bind turn injects that binding's `Val.G<g>` module and
/// seeds its slot into the `ExternalEnv` (session.rs `run_bind` ~L321/L365); the
/// subsequent `deep_force` (forced=true, session.rs ~L368/L375
/// `run_fragment_and_bind`) traverses the Text spine and reaches a TypeMetadata
/// node it treats as dead code → kind=4 yield. The shallow-Int prior binding
/// itself forces fine, so the defect is in deep_force of the Text payload under a
/// multi-module-inject context, not in the prior value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_bind_with_prior_live_binding_diagnostic() {
    require_extract();
    let repl = Repl::new();

    repl.eval("x <- pure (1 :: Int)")
        .await
        .expect_ok("bind x :: Int");

    // The diagnostic bind. If the bug is general (prior-binding-present), this is
    // where `kind=4 TypeMetadata` / "forced type metadata (should be dead code)"
    // surfaces.
    let bind = repl.eval("y <- pure (T.pack \"hi\")").await;
    // Regression witness: was the kind=4 TypeMetadata crash (now fixed). A Tier-0
    // Text bind with a prior live binding used to fire here; now asserts success.
    bind.expect_ok("bind y :: Text with prior Int live (KEY DIAGNOSTIC)");

    let out = repl.eval("T.length y").await;
    assert!(
        out.expect_ok("T.length y").contains('2'),
        "diagnostic text bind: expected 2, got: {}",
        out.text
    );
}

/// CASE 3 — A bind that REFERENCES an earlier binding at bind time.
///
/// `k <- pure (5 :: Int)`; `m <- pure (k + 1)`; `m` => 6.
/// The `m` bind action reads `k` through the seeded ExternalEnv — `k`'s bind
/// stays its own turn (the property is about the ExternalEnv seeded from a
/// PRIOR turn's binding); `m`'s bind and its readback share the later turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bind_references_earlier_binding() {
    require_extract();
    let repl = Repl::new();

    repl.eval("k <- pure (5 :: Int)")
        .await
        .expect_ok("bind k=5");

    let out = repl.run(&["m <- pure (k + 1)", "m"]).await;
    let text = out.expect_ok("bind m = k + 1 (references k) + reference m");
    assert!(
        text.contains("\"value\":6"),
        "bind-references-earlier: expected 6, got: {text}"
    );
}

/// CASE 4 — A Tier-1 closure capturing an earlier binding survives GC.
///
/// `base <- pure (100 :: Int)`; `f <- pure (\n -> n + base)`; heavy fold (forces
/// minor GC on the 2 MiB nursery); `f 5` => 105.
/// Proves the closure's captured session value (`base`) stays live across GC.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closure_captures_binding_survives_gc() {
    require_extract();
    let repl = Repl::new();

    repl.eval("base <- pure (100 :: Int)")
        .await
        .expect_ok("bind base=100");
    repl.eval("f <- pure (\\n -> n + base)")
        .await
        .expect_ok("bind f (captures base)");

    // ORGANIC GC: ~6 MiB of transient cons into the 2 MiB nursery.
    let fold = repl.eval("foldl' (+) (0 :: Int) [1..200000]").await;
    assert!(
        fold.expect_ok("heavy fold (force GC)")
            .contains("20000100000"),
        "fold: got {}",
        fold.text
    );

    let out = repl.eval("f 5").await;
    assert!(
        out.expect_ok("f 5 after GC").contains("105"),
        "closure-captures-survives-GC: expected 105, got: {}",
        out.text
    );
}

/// CASE 5 — Nested/recursive ADT: bind a tree, sum it a later turn.
///
/// def `data Tree = Leaf Int | Node Tree Tree` + bind `t` share one turn;
/// def `sumT t = case t of { Leaf n -> n; Node a b -> sumT a + sumT b }` +
/// `pure (sumT t)` share a LATER turn. Real con names (Leaf/Node) must resolve
/// from the merged session DataConTable against the tenured heap value (not
/// `<unknown>`) — the cross-turn boundary between bind and use is the proof.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nested_recursive_adt_bind_and_sum() {
    require_extract();
    let repl = Repl::new();

    repl.run(&[
        "data Tree = Leaf Int | Node Tree Tree",
        "t <- pure (Node (Leaf 1) (Node (Leaf 2) (Leaf 3)))",
    ])
    .await
    .expect_ok("def Tree + bind t (nested Tree)");

    // Regression witness: was the kind=4 TypeMetadata crash (now fixed). Spine
    // traversal of a deep bound ADT used to fire here; now asserts success.
    let out = repl
        .run(&[
            "sumT t = case t of { Leaf n -> n; Node a b -> sumT a + sumT b }",
            "pure (sumT t)",
        ])
        .await;
    let text = out.expect_ok("def sumT + sumT t");
    assert!(
        text.contains('6'),
        "nested ADT sum: expected 6, got: {text}"
    );
}

/// CASE 6a — Maybe: bind `Just 7`, case-match it.
///
/// `mb <- pure (Just (7 :: Int))`; `case mb of { Just n -> n; Nothing -> 0 }` => 7.
/// No cross-turn claim — one multi-item block proves it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maybe_bind_and_case() {
    require_extract();
    let repl = Repl::new();

    let out = repl
        .run(&[
            "mb <- pure (Just (7 :: Int))",
            "case mb of { Just n -> n; Nothing -> 0 }",
        ])
        .await;
    let text = out.expect_ok("bind mb = Just 7 + case mb");
    assert!(text.contains('7'), "Maybe case: expected 7, got: {text}");
}

/// CASE 6b — Either: bind `Left 1`, case-match it.
///
/// `e <- pure (Left (1 :: Int) :: Either Int Int)`;
/// `case e of { Left a -> a; Right b -> b }` => 1.
/// No cross-turn claim — one multi-item block proves it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn either_bind_and_case() {
    require_extract();
    let repl = Repl::new();

    let out = repl
        .run(&[
            "e <- pure (Left (1 :: Int) :: Either Int Int)",
            "case e of { Left a -> a; Right b -> b }",
        ])
        .await;
    let text = out.expect_ok("bind e = Left 1 + case e");
    assert!(text.contains('1'), "Either case: expected 1, got: {text}");
}

/// CASE 7 — Structured JSON `Value`: bind an `object`, read a field back.
///
/// `v <- pure (object [("a", toJSON (1 :: Int)), ("b", toJSON (2 :: Int))])`;
/// `renderJson v` (and an optics read `v ^? key "a" . _Integer`).
/// Tier-0 structured value + DataConTable round-trip; no cross-turn claim, so
/// one multi-item block proves all three facts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structured_json_value_bind_and_read() {
    require_extract();
    let repl = Repl::new();

    // Regression witness: was the kind=4 TypeMetadata crash (now fixed). Reading
    // back a bound JSON `Value` used to fire here; now asserts success.
    let out = repl
        .run(&[
            "v <- pure (object [(\"a\", toJSON (1 :: Int)), (\"b\", toJSON (2 :: Int))])",
            "renderJson v",
            "v ^? key \"a\" . _Integer",
        ])
        .await;
    let text = out.expect_ok("bind v + renderJson v + optics read");
    assert!(
        text.contains("\\\"a\\\"") || text.contains("\"a\"") || text.contains('a'),
        "renderJson v: expected to mention field a, got: {text}"
    );
    assert!(
        text.contains('1') && text.contains('2'),
        "renderJson v: expected 1 and 2, got: {text}"
    );
    // Optics read of a single field — should be 1 (the final item's top-level value).
    assert!(
        text.contains("\"value\":1"),
        "v ^? key a . _Integer: expected 1, got: {text}"
    );
}

/// CASE 8 — A list binding survives GC; length + sum read back.
///
/// `xs <- pure [1..1000 :: Int]`; heavy fold (GC); `pure (length xs)` => 1000;
/// `pure (sum xs)` => 500500.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_bind_survives_gc() {
    require_extract();
    let repl = Repl::new();

    repl.eval("xs <- pure [1..1000 :: Int]")
        .await
        .expect_ok("bind xs = [1..1000]");

    // ORGANIC GC.
    let fold = repl.eval("foldl' (+) (0 :: Int) [1..200000]").await;
    assert!(
        fold.expect_ok("heavy fold (force GC)")
            .contains("20000100000"),
        "fold: got {}",
        fold.text
    );

    // Regression witness: was the kind=4 TypeMetadata crash (now fixed). Forcing
    // the bound list spine used to fire here even though the result is an Int;
    // now asserts success.
    let len = repl.eval("pure (length xs)").await;
    assert!(
        len.expect_ok("length xs after GC").contains("1000"),
        "length xs: expected 1000, got: {}",
        len.text
    );

    let total = repl.eval("pure (sum xs)").await;
    assert!(
        total.expect_ok("sum xs after GC").contains("500500"),
        "sum xs: expected 500500, got: {}",
        total.text
    );
}

/// CASE 9 — A function bound, then a value bound FROM applying it.
///
/// `g <- pure (\n -> n * 2 :: Int)`; `h <- pure (g 21)`; `h` => 42.
/// The closure is applied at bind time and its result rooted as a Tier-0
/// value — no cross-turn claim, so one multi-item block proves it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn function_applied_at_bind_time() {
    require_extract();
    let repl = Repl::new();

    let out = repl
        .run(&["g <- pure (\\n -> n * 2 :: Int)", "h <- pure (g 21)", "h"])
        .await;
    let text = out.expect_ok("bind g + bind h = g 21 + reference h");
    assert!(
        text.contains("42"),
        "function-applied-at-bind: expected 42, got: {text}"
    );
}

/// CASE 10 — ToWire container instances: lists/Maybe/tuples render as
/// STRUCTURAL JSON (Array/Null), not a flat Show string, while the Show floor
/// (a bare custom ADT) is unchanged and String/Text still render bare.
///
/// Parses `Turn.text` as JSON and asserts on the `value` field's JSON shape
/// directly, rather than substring-matching, so a regression to the flat
/// Show-string form (`"[1,2,3]"` as a JSON STRING) is caught even though it
/// would still textually "contain" the digits.
fn value_of(turn: &Turn) -> serde_json::Value {
    let v = serde_json::from_str::<serde_json::Value>(&turn.text)
        .unwrap_or_else(|e| panic!("turn text is not JSON ({e}): {}", turn.text));
    // `run_block_single` (common/mod.rs) deliberately omits `value` from the
    // merged JSON when it is JSON `null` (its merge loop skips null so it
    // never clobbers an item field) — so a MISSING key here means the real
    // value was `null`, exactly the case `Nothing` renders as.
    v.get("value").cloned().unwrap_or(serde_json::Value::Null)
}

/// As [`value_of`], but for a MULTI-item `.run()` block's raw envelope: item
/// `idx`'s value is inline in `items[idx].value` unless it's the block's LAST
/// executed item, whose value is hoisted to the top-level `value` only
/// (`Block::render`'s slim shape — see `session.rs`'s `finish_block`).
fn item_value(turn: &Turn, idx: usize, is_last: bool) -> serde_json::Value {
    let v = serde_json::from_str::<serde_json::Value>(&turn.text)
        .unwrap_or_else(|e| panic!("turn text is not JSON ({e}): {}", turn.text));
    if is_last {
        return v.get("value").cloned().unwrap_or(serde_json::Value::Null);
    }
    v.get("items")
        .and_then(|items| items.get(idx))
        .and_then(|item| item.get("value"))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn towire_list_of_int_renders_as_json_array() {
    require_extract();
    let repl = Repl::new();
    let out = repl.eval("pure ([1, 2, 3] :: [Int])").await;
    out.expect_ok("list of Int");
    let v = value_of(&out);
    // Structural array AND native scalar leaves: `ToWire Int` (delegating to
    // toJSON) renders each element as a JSON number, so `[Int]` -> `[1,2,3]`.
    assert_eq!(
        v,
        json!([1, 2, 3]),
        "expected a structural JSON array of number leaves, got: {v}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn towire_list_of_records_renders_as_array_of_show_leaves() {
    require_extract();
    let repl = Repl::new();
    // Mixed decl+expr items are allowed in one block — no cross-turn claim.
    let out = repl
        .run(&[
            "data Pt = Pt { px :: Int, py :: Int } deriving Show",
            "pure [Pt 1 2, Pt 3 4]",
        ])
        .await;
    out.expect_ok("def Pt + list of records");
    let v = item_value(&out, 1, true);
    let arr = v
        .as_array()
        .unwrap_or_else(|| panic!("expected a JSON array of records, got: {v}"));
    assert_eq!(arr.len(), 2, "expected 2 elements, got: {v}");
    for elem in arr {
        let s = elem
            .as_str()
            .unwrap_or_else(|| panic!("record leaf should be a Show-string, got: {elem}"));
        assert!(
            s.contains("Pt"),
            "record leaf should be the derived Show text: {s}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn towire_string_and_text_stay_bare() {
    require_extract();
    let repl = Repl::new();

    // A bare [Char]/String result stays a plain JSON string, not `["'h'",...]`;
    // Text stays bare too — both checked in one block (no cross-turn claim).
    let out = repl.run(&["pure \"hi\"", "pure (T.pack \"hi\")"]).await;
    out.expect_ok("bare String + bare Text");
    assert_eq!(
        item_value(&out, 0, false),
        json!("hi"),
        "String must render bare, not as a list of chars"
    );
    assert_eq!(
        item_value(&out, 1, true),
        json!("hi"),
        "Text must render bare"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn towire_maybe_renders_as_null_or_payload() {
    require_extract();
    let repl = Repl::new();

    // `Maybe` contributes the Just/Nothing -> payload/null structure; the Int
    // payload renders as a native JSON number via `ToWire Int`. No cross-turn
    // claim — both checked in one block.
    let out = repl
        .run(&["pure (Just (3 :: Int))", "pure (Nothing :: Maybe Int)"])
        .await;
    out.expect_ok("Just 3 + Nothing");
    assert_eq!(
        item_value(&out, 0, false),
        json!(3),
        "Just 3 should unwrap to the bare number payload 3, not \"Just 3\" or \"3\""
    );
    assert_eq!(
        item_value(&out, 1, true),
        serde_json::Value::Null,
        "Nothing should render as JSON null"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn towire_bare_show_only_adt_still_hits_the_floor() {
    require_extract();
    let repl = Repl::new();
    // Mixed decl+expr items in one block — no cross-turn claim.
    let out = repl
        .run(&[
            "data Color = Red | Green | Blue deriving Show",
            "pure Green",
        ])
        .await;
    out.expect_ok("def Color + bare Show-only ADT");
    assert_eq!(
        item_value(&out, 1, true),
        json!("Green"),
        "a non-container Show-only ADT must still hit the Show floor"
    );
}
