//! DO-BLOCK INVARIANT — acceptance tests (no product changes here).
//!
//! THE INVARIANT under test:
//!   "For any items that would be legal as consecutive statements in a GHCi
//!    do-block, `session_run` behaves identically — scoping/input, statement
//!    forms, per-item failure granularity, clean diagnostics — with ZERO
//!    observable trace of which internal plane (decl vs. value/materialized)
//!    anything landed on."
//!
//! Background: a pure `let x = e` / `x = e` bind is lowered to the DECL plane
//! (compiled as its own gen module, generalizes); an effectful `x <- eff` bind
//! MATERIALIZES a heap value (monomorphic). The invariant says a caller must
//! not be able to TELL which happened from observable behavior.
//!
//! Every test drives the REAL production entry point through the shared
//! `common` harness (`Repl::new` / `open_ok` / `eval` / `run` / `close`), each
//! `repl.eval(..)` being one `session_run`. Cross-CALL persistence facets use
//! SEPARATE `eval` calls (one `session_run` each), never a single multi-item
//! block. Facets that are KNOWN to leak the plane are encoded as the test that
//! WOULD prove opacity and marked `#[ignore = "... ledger #NN"]` — they are the
//! implementers' work-list, not a product fix for this task.
//!
//! Each test skips cleanly (early return) when the session-aware extract is
//! unavailable, so the file still COMPILES without `TIDEPOOL_EXTRACT`.

mod common;
use common::*;

use rmcp::model::RawContent;

// ---------------------------------------------------------------------------
// Local helper: `session_run` a single item WITH an `input` payload lane.
//
// The shared `common::Repl` helper does not thread `input`, so we dispatch the
// tool directly (same production entry point, `dispatch_tool("session_run", …)`)
// and reduce the slim block envelope to `(ok, value/type text)` the same way
// `common`'s `run_block_single` does.
// ---------------------------------------------------------------------------
async fn eval_with_input(repl: &Repl, item: &str, input: serde_json::Value) -> Turn {
    let mut args = serde_json::Map::new();
    args.insert(
        "items".to_string(),
        serde_json::Value::Array(vec![serde_json::Value::String(item.to_string())]),
    );
    args.insert("input".to_string(), input);

    let res = repl
        .server
        .dispatch_tool("session_run", args)
        .await
        .expect("dispatch session_run with input");
    let raw_text = match &res.content[0].raw {
        RawContent::Text(t) => t.text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    let raw_is_error = res.is_error == Some(true);

    // Strip the "## Output\n…\n## Result\n" prefix if captured output present.
    let json_part = if let Some(pos) = raw_text.rfind("\n## Result\n") {
        &raw_text[pos + "\n## Result\n".len()..]
    } else {
        &raw_text
    };

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_part) {
        if let Some(item0) = v.get("items").and_then(|arr| arr.get(0)) {
            let ok = item0
                .get("ok")
                .and_then(|o| o.as_bool())
                .unwrap_or(!raw_is_error);
            let mut result = serde_json::Map::new();
            if let Some(obj) = item0.as_object() {
                for (k, val) in obj {
                    if k != "kind" && k != "ok" {
                        result.insert(k.clone(), val.clone());
                    }
                }
            }
            for key in &["value", "type", "truncated"] {
                if let Some(val) = v.get(*key) {
                    if !val.is_null() && !result.contains_key(*key) {
                        result.insert(key.to_string(), val.clone());
                    }
                }
            }
            let text = if result.is_empty() {
                raw_text.clone()
            } else {
                serde_json::Value::Object(result).to_string()
            };
            return Turn {
                text,
                is_error: !ok,
            };
        }
    }
    Turn {
        text: raw_text,
        is_error: raw_is_error,
    }
}

/// Parse the multi-item block envelope produced by `Repl::run` into the
/// per-item objects, so a test can assert on per-item failure granularity
/// (which items ran, which are `ok`).
fn parse_items(raw_text: &str) -> Vec<serde_json::Value> {
    let json_part = if let Some(pos) = raw_text.rfind("\n## Result\n") {
        &raw_text[pos + "\n## Result\n".len()..]
    } else {
        raw_text
    };
    serde_json::from_str::<serde_json::Value>(json_part)
        .ok()
        .and_then(|v| {
            v.get("items")
                .and_then(|a| a.as_array())
                .map(|a| a.to_vec())
        })
        .unwrap_or_default()
}

// ===========================================================================
// FACET 1 — Cross-call scoping: both planes persist identically across calls.
// ===========================================================================

/// An effectful bind (`n <- pure …` — materialized) AND a pure bind
/// (`let m = …` — decl plane) both persist across SEPARATE `session_run` calls
/// and are visible in a LATER call, identically. Each `repl.eval(..)` is one
/// `session_run`, so this proves cross-CALL persistence, not within-block scope.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_call_scoping_both_planes_persist() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // Call 1: effectful bind (materializes a heap value, monomorphic).
    repl.eval("n <- pure (5 :: Int)")
        .await
        .expect_ok("call1: effectful bind n");
    // Call 2: pure bind (lowered to the decl plane, generalizes).
    repl.eval("let m = (10 :: Int)")
        .await
        .expect_ok("call2: pure bind m");

    // Call 3: reference the effectful bind alone.
    let a = repl.eval_ok("pure (n + 1)").await;
    assert!(a.contains('6'), "n + 1 should be 6 across calls, got: {a}");

    // Call 4: reference BOTH planes together — persisted identically.
    let b = repl.eval_ok("pure (n + m + 1)").await;
    assert!(
        b.contains("16"),
        "n + m + 1 should be 16 (both planes visible in a later call), got: {b}"
    );

    repl.close().await.expect_ok("close");
}

// ===========================================================================
// FACET 2 — `input` payload lane.
// ===========================================================================

/// The `input` payload passed on a block is in scope for an item that
/// references it. Passing `input = 7` and evaluating `pure input` surfaces 7.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_payload_lane_in_scope() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // `input :: Aeson.Value` — a JSON number 7 renders back as 7.
    let out = eval_with_input(&repl, "pure input", serde_json::json!(7)).await;
    let text = out.expect_ok("input payload lane referenced in an item");
    assert!(
        text.contains('7'),
        "input payload (7) should be in scope, got: {text}"
    );

    repl.close().await.expect_ok("close");
}

/// Regression guard (input-lane fix 843bd05): a user binding literally NAMED
/// `input` — with NO payload lane passed — is NOT confused with the payload
/// lane. `let input = (7 :: Int)` binds; `pure (input + 1)` → 8. The
/// materialize guard keys on the decl-compile error ("not in scope: input"),
/// not a textual scan for the word `input`, so a user's own `input` binding is
/// honored.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_binding_named_input_not_confused_with_lane() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // No `input` payload passed on these calls — the only `input` in scope is
    // the user's own binding.
    repl.eval("let input = (7 :: Int)")
        .await
        .expect_ok("bind a value literally named `input`");
    let out = repl.eval_ok("pure (input + 1)").await;
    assert!(
        out.contains('8'),
        "user `input` binding used (input + 1 = 8), not the payload lane, got: {out}"
    );

    repl.close().await.expect_ok("close");
}

// ===========================================================================
// FACET 3 — All statement forms legal as consecutive items in one block.
// ===========================================================================

/// One block mixing every statement form a GHCi do-block admits: a top-level
/// decl (`f x = …`), a pure `let` bind, an effectful `x <- pure …` bind, and a
/// bare final expression. All classify + run, and the final `value` is
/// populated from the trailing expression.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_statement_forms_consecutive() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let out = repl
        .run(&[
            "g3 x = x + (1 :: Int)", // decl (Auto → decl)
            "let y3 = (10 :: Int)",  // pure let bind (decl plane)
            "z3 <- pure (5 :: Int)", // effectful bind (materialize)
            "pure (g3 z3 + y3)",     // bare final expression → 5+1+10 = 16
        ])
        .await;
    let text = out.expect_ok("all statement forms consecutive in one block");
    assert!(
        text.contains("16"),
        "trailing expression populates value (g3 z3 + y3 = 16), got: {text}"
    );

    repl.close().await.expect_ok("close");
}

// ===========================================================================
// FACET 4 — Per-item failure granularity + clean diagnostics.
// ===========================================================================

/// A 3-item block where item 2 fails to compile: item 1 succeeded (its
/// `ok:true` is reported), item 3 did NOT run (execution stops on first error),
/// and the surfaced error is a clean mapped diagnostic — no raw generated
/// module noise (`Tidepool.Session`, `G<n>.hs:` paths).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_item_failure_granularity_and_clean_diag() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    let out = repl
        .run(&[
            "let a4 = (1 :: Int)",     // item 0 — OK
            "pure (nonexistent4 + 1)", // item 1 — compile error (not in scope)
            "let c4 = (99 :: Int)",    // item 2 — must NOT run
        ])
        .await;
    // The block as a whole failed.
    assert!(
        out.is_error,
        "block with a failing item 2 must surface as an error: {}",
        out.text
    );

    let items = parse_items(&out.text);
    assert!(
        !items.is_empty(),
        "expected a parseable per-item envelope, got: {}",
        out.text
    );
    // Item 0 ran and succeeded.
    assert_eq!(
        items[0].get("ok").and_then(|o| o.as_bool()),
        Some(true),
        "item 0 (let a4) should report ok:true, got: {}",
        out.text
    );
    // Execution stopped at the first error: item 2 did NOT run.
    assert!(
        items.len() < 3,
        "item 3 (let c4) must NOT run after item 2 failed — envelope should have < 3 items, got {}: {}",
        items.len(),
        out.text
    );
    // The failing item reports ok:false.
    assert_eq!(
        items
            .last()
            .and_then(|it| it.get("ok"))
            .and_then(|o| o.as_bool()),
        Some(false),
        "the last reported item must be the failing one (ok:false), got: {}",
        out.text
    );

    // Clean diagnostic: no generated-module / .hs-path noise leaks to the user.
    assert!(
        !out.text.contains("Tidepool.Session"),
        "error must not leak the generated session module name: {}",
        out.text
    );
    assert!(
        !out.text.contains(".hs:"),
        "error must not leak a generated `G<n>.hs:L:C` path: {}",
        out.text
    );

    repl.close().await.expect_ok("close");
}

// ===========================================================================
// FACET 5 — Plane opacity: pure and effectful binds are interchangeable.
// ===========================================================================

/// PASS side of the invariant: two binds of the same shape/type — one pure
/// (`let`, decl plane) and one effectful (`<- pure`, may materialize) — are
/// observably interchangeable. Both are usable at a later call with identical
/// result shape; a caller cannot tell which plane each landed on. (Non-colliding
/// names, so no Prelude-shadow leak is in play — that is the ignored case below.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plane_opacity_binds_interchangeable() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // Pure bind (decl plane) and effectful bind (materialize), same type.
    repl.eval("let purej = (5 :: Int)")
        .await
        .expect_ok("pure bind purej");
    repl.eval("effk <- pure (5 :: Int)")
        .await
        .expect_ok("effectful bind effk");

    // Each used alone at a later call — identical observable result.
    let a = repl.eval_ok("pure (purej + 1)").await;
    assert!(a.contains('6'), "purej + 1 = 6, got: {a}");
    let b = repl.eval_ok("pure (effk + 1)").await;
    assert!(b.contains('6'), "effk + 1 = 6, got: {b}");

    // And together — both planes coexist and are indistinguishable in use.
    let c = repl.eval_ok("pure (purej + effk)").await;
    assert!(c.contains("10"), "purej + effk = 10, got: {c}");

    repl.close().await.expect_ok("close");
}

/// OPACITY LEAK (ledger #36) — decl-plane cannot intentionally shadow a Prelude
/// re-export, so a PURE bind of a Prelude name behaves DIFFERENTLY from an
/// effectful one: the invariant says they should be interchangeable. `let
/// lookup = (42 :: Int)` routes to the decl plane, whose Prelude import does NOT
/// hide session names → "Ambiguous occurrence 'lookup'" on later use. The fix
/// is to hide session-redefined names from the decl module's Prelude import
/// (mirroring the stmt plane). Un-ignore when the do-block invariant fix lands.
#[ignore = "do-block: decl-plane can't shadow a Prelude re-export via a pure bind, ledger #36"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plane_opacity_pure_bind_shadows_prelude_name() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    repl.eval("let lookup = (42 :: Int)")
        .await
        .expect_ok("pure bind named `lookup` (Prelude re-export)");
    // A pure bind should shadow the Prelude name exactly as an effectful bind
    // would — no ambiguous-occurrence, no plane leak.
    let used = repl.eval_ok("pure (lookup + 1)").await;
    assert!(
        used.contains("43"),
        "pure bind must shadow Prelude.lookup (42 + 1 = 43), got: {used}"
    );

    repl.close().await.expect_ok("close");
}

/// OPACITY LEAK (ledger #28) — a fully-open `HasField`-constrained record-dot
/// helper (`let f h = h.path`, both the record type AND field type free) binds
/// on the WARM live server but fails COLD in the test harness, falling to the
/// materialize path whose `pure f` can't monomorphize the unresolved `HasField`
/// constraint → "No instance for HasField". The invariant wants a record-dot
/// helper to bind on the decl plane and generalize regardless of process warmth.
/// Un-ignore when `SessionLib::define`'s cold Core extraction is fixed.
#[ignore = "do-block: fully-open HasField record-dot helper fails cold in-harness, ledger #28"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plane_opacity_open_hasfield_helper_binds_cold() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let repl = Repl::new();
    repl.open_ok().await;

    // Fully open: `HasField "path" r a` with both r and a free (no `T.toUpper`
    // to pin the result type). Must bind as a generalized decl, not materialize.
    let out = repl.eval("let getPath h = h.path").await;
    let text = out.expect_ok("open record-dot helper binds (decl plane, generalizes)");
    assert!(
        text.contains("HasField") || text.contains("path"),
        "open record-dot helper should show its constrained type, got: {text}"
    );

    repl.close().await.expect_ok("close");
}

/// OPACITY LEAK (ledger #31) — Record field naming is inconsistent across effect
/// records (`Hit` uses bare `text`/`path`/`line`; `Match` from `sgFind` uses
/// `match`-prefixed `matchText`/`matchFile`/`matchLine`), so the same record-dot
/// idiom leaks WHICH record (and thus which producing plane/effect) a value came
/// from. Proving opacity requires exercising both `Hit` (Fs) and `Match` (SG)
/// records — but the MINIMAL test stack (`build_minimal_stack`) has neither the
/// `Fs` nor the `SG` effect, so this facet CANNOT be encoded in-harness. Left as
/// a documented placeholder; the real proof must run against the full server
/// stack once field naming is unified (bridge-generator work).
#[ignore = "do-block: Match/Hit field-name divergence — needs Fs+SG effects (absent from minimal stack), ledger #31"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plane_opacity_match_hit_field_naming_uniform() {
    // Cannot be exercised: the minimal test stack has no Fs (`grepGlob`→Hit) or
    // SG (`sgFind`→Match) effect. Encoded as an ignored placeholder so the
    // work-list carries it; see the report for why it is un-encodable here.
    if !extract_available() {
        return;
    }
    unreachable!("placeholder — see #[ignore] reason; not runnable on the minimal stack");
}
