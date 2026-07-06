//! GHCi-style `it` binding + fold-forward large-value handling — DIMENSION.
//!
//! Drives the REAL `tidepool-repl` MCP entry point (`session_run`) over real
//! turns (the standing rule), per `plans/haskell-interface-polish.md` item #3:
//! a bare final EXPRESSION binds its value to `it` (rebinding every such
//! turn, GHCi parity); a trailing bind (`x <- e` / `_ <- e`) does NOT bind
//! `it`; a result over `truncate::HUGE_CEILING` collapses to a header
//! instead of a partial dump. Each test guards on `extract_available()` and
//! skips cleanly otherwise.

mod common;
use common::*;

use std::path::PathBuf;

use rmcp::model::{CallToolResult, RawContent};
use tidepool_handlers::{
    base_decls_with_ask, build_base_stack, HandlerConfig, DEFAULT_OPENAI_MODEL,
};
use tidepool_repl::{ReplServerConfig, TidepoolReplServer};

/// CASE 1 — a bare final expression binds `it`; usable next turn
/// (`length it`, `take 2 it`), and shows up in `:bindings`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_expr_binds_it_and_is_usable_next_turn() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();

    let t = repl.eval("[1,2,3,4,5] :: [Int]").await;
    t.expect_ok("bare list literal");

    let bindings = repl.cmd(":bindings").await;
    let out = bindings.expect_ok(":bindings after a bare expression");
    assert!(
        out.contains("\"it\""),
        "`it` should be a live binding after a bare expression, got: {out}"
    );

    let t = repl.eval("length it").await;
    let out = t.expect_ok("length it");
    assert!(out.contains('5'), "length it should be 5, got: {out}");

    // `length it` above is ITSELF a bare expression, so it rebound `it` to 5
    // (GHCi parity: `it` rebinds after every evaluated expression, including
    // one that references the prior `it`) — re-establish the list before
    // probing `take`, rather than chaining off the now-stale `it`.
    repl.eval("[1,2,3,4,5] :: [Int]")
        .await
        .expect_ok("re-bind the list");
    let t = repl.eval("take 2 it").await;
    let out = t.expect_ok("take 2 it");
    assert!(
        out.contains('1') && out.contains('2'),
        "take 2 it should be [1,2], got: {out}"
    );
}

/// CASE 2 — `it` rebinds on every bare expression (latest-wins, GHCi parity).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_rebinds_on_next_bare_expression() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();

    repl.eval("10 :: Int").await.expect_ok("first bare expr");
    repl.eval("20 :: Int").await.expect_ok("second bare expr");

    let t = repl.eval("it").await;
    let out = t.expect_ok("it after two bare expressions");
    assert!(
        out.contains("20") && !out.contains("10"),
        "it should hold the LATEST bare expression's value (20), got: {out}"
    );
}

/// CASE 3 — a trailing named bind (`x <- e`) does NOT bind `it`: referencing
/// `it` with no prior bare expression in the session is a scope error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trailing_named_bind_does_not_bind_it() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();

    repl.eval("x <- pure (5 :: Int)")
        .await
        .expect_ok("named bind");

    let t = repl.eval("it").await;
    assert!(
        t.is_error,
        "`it` must be unbound after only a named bind (`x <- e`), got: {}",
        t.text
    );
}

/// CASE 4 — a discard bind (`_ <- e`) also does NOT bind `it` (it is
/// implemented as a bind, not a bare expression, even though the RHS runs on
/// the same reference path — see `Session::run_eval`'s `[] =>` arm).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discard_bind_does_not_bind_it() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();

    repl.eval("_ <- pure (5 :: Int)")
        .await
        .expect_ok("discard bind");

    let t = repl.eval("it").await;
    assert!(
        t.is_error,
        "`it` must be unbound after only a discard bind (`_ <- e`), got: {}",
        t.text
    );
}

// ---------------------------------------------------------------------------
// No-double-execution: a KV read-increment-write counter, run as ONE bare
// final expression. If the expression's effect fired twice (once to bind
// `it`, once to render `value`), the counter would read 2 on the very next
// turn instead of 1. Needs the KV effect, which the minimal (Console+Ask)
// stack used by `common::Repl` doesn't carry — build a full-stack server
// (mirrors `effects_smoke.rs`'s `build_full_server`).
// ---------------------------------------------------------------------------

fn text_of(res: &CallToolResult) -> String {
    match &res.content[0].raw {
        RawContent::Text(t) => t.text.clone(),
        other => panic!("expected text content, got {other:?}"),
    }
}

fn build_full_server(cwd: PathBuf) -> TidepoolReplServer {
    let kv_path = cwd.join("kv.json");
    let handler_cfg = HandlerConfig {
        cwd,
        kv_path,
        llm_model: DEFAULT_OPENAI_MODEL.to_string(),
    };
    let stack = build_base_stack(&handler_cfg);
    let (decls, ask_tag) = base_decls_with_ask(&stack);
    let effects_dir =
        tidepool_mcp::ensure_effects_module(&decls).expect("write Tidepool.Effects module");
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf();
    let prelude_dir = repo_root.join("haskell").join("lib");
    let session_root_base = std::env::temp_dir().join(format!(
        "tidepool-repl-it-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let module_env = tidepool_mcp::session_decl_module_env(&decls, false);
    let cfg = ReplServerConfig {
        decls,
        ask_tag,
        base_include: vec![effects_dir, prelude_dir],
        module_env,
        session_root_base,
        nursery_size: None,
        continuation_ttl: None,
        wedged_ttl: None,
        turn_timeout: None,
    };
    TidepoolReplServer::new(stack, cfg)
}

/// Dispatch a 1-item `session_run` block and unwrap `items[0]` the same way
/// `effects_smoke.rs::run_single` does — inline item fields merged with the
/// top-level `value`/`type`/`truncated`.
async fn run_single(server: &TidepoolReplServer, item: &str) -> (bool, String) {
    let mut args = serde_json::Map::new();
    args.insert(
        "items".into(),
        serde_json::Value::Array(vec![serde_json::Value::String(item.to_string())]),
    );
    let r = server
        .dispatch_tool("session_run", args)
        .await
        .expect("session_run dispatch");
    let raw = text_of(&r);
    let raw_is_error = r.is_error == Some(true);
    let json_part = if let Some(pos) = raw.rfind("\n## Result\n") {
        &raw[pos + "\n## Result\n".len()..]
    } else {
        &raw
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
                raw.clone()
            } else {
                serde_json::Value::Object(result).to_string()
            };
            return (!ok, text);
        }
    }
    (raw_is_error, raw)
}

/// CASE 5 — the effectful final expression's side effect fires EXACTLY ONCE:
/// a KV read-increment-write counter run as one bare expression must leave
/// the counter at 1, not 2 (a double-run — once for the bind, once for the
/// render — would double-increment it).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn effectful_bare_expression_runs_its_effect_exactly_once() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = build_full_server(tmp.path().to_path_buf());

    let counter_turn = "do { mv <- kvGet \"counter\" ; \
         let { n = maybe 1 (\\v -> maybe 0 id (v ^? _Int) + 1) mv } ; \
         kvSet \"counter\" (toJSON (n :: Int)) ; pure n }";
    let (is_error, text) = run_single(&server, counter_turn).await;
    assert!(!is_error, "counter turn errored: {text}");
    assert!(
        text.contains('1') && !text.contains('2'),
        "single run of the counter expression should yield 1, got: {text}"
    );

    // A SEPARATE, later turn reads the persisted counter back — proves the
    // bind above did not ALSO increment it a second time via the render step.
    let (is_error, text) = run_single(&server, "kvGet \"counter\"").await;
    assert!(!is_error, "kvGet \"counter\" errored: {text}");
    assert!(
        text.contains('1'),
        "counter must be 1 after exactly one increment (no double-execution), got: {text}"
    );
}

// ---------------------------------------------------------------------------
// Fold-forward: over `truncate::HUGE_CEILING`, the response `value` collapses
// to a header ({type,size,note}) instead of a partial structural preview —
// the full value is still bound to `it` and fetchable via `:stub 0`.
// ---------------------------------------------------------------------------

/// CASE 6 — a result over `HUGE_CEILING` renders header-only, stays bound to
/// `it` (usable next turn), and the full value is fetchable via `:stub 0`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn huge_value_is_header_only_bound_to_it_and_stub_fetchable() {
    if !extract_available() {
        return;
    }
    let repl = Repl::new();

    // 40_000 chars renders well past HUGE_CEILING (RESULT_BUDGET * 8 = 32_768).
    let t = repl.eval("T.replicate 40000 \"x\"").await;
    let out = t.expect_ok("huge Text value");
    let parsed: serde_json::Value = serde_json::from_str(out)
        .unwrap_or_else(|e| panic!("response should be JSON: {e}; got: {out}"));
    let value = &parsed["value"];
    assert!(
        value.is_object(),
        "a huge result's `value` must be a header object, not a preview, got: {out}"
    );
    assert_eq!(
        value["type"].as_str(),
        Some("Text"),
        "huge-value header must name the type, got: {out}"
    );
    let size = value["size"]
        .as_u64()
        .unwrap_or_else(|| panic!("huge-value header must carry a numeric size, got: {out}"));
    assert!(
        size > 32_768,
        "reported size should exceed HUGE_CEILING: {size}"
    );
    let note = value["note"]
        .as_str()
        .unwrap_or_else(|| panic!("huge-value header must carry a note, got: {out}"));
    assert!(
        note.contains("`it`") && !note.to_lowercase().contains("fold"),
        "huge-value note must be neutral (mentions `it`, not a fold-specific recipe): {note}"
    );

    // The full value is still bound to `it` — usable next turn.
    let t = repl.eval("T.length it").await;
    let out = t.expect_ok("T.length it after a huge value");
    assert!(
        out.contains("40000"),
        "it should still hold the FULL 40000-char value, got: {out}"
    );

    // ...and fetchable in full via :stub 0.
    let stub = repl.cmd(":stub 0").await;
    let out = stub.expect_ok(":stub 0 after a huge value");
    assert!(
        out.contains(&"x".repeat(100)),
        ":stub 0 should page back the full huge string, got a {}-char response",
        out.len()
    );
}
