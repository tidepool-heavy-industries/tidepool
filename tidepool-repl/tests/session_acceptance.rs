//! Wave-2 acceptance — the real `tidepool-repl` entry point, multi-turn, on one
//! resident machine (the standing rule: drive the production tool dispatch over
//! several real turns, not a bespoke harness).
//!
//! Flow: session_open → session_run (def `slug`) → session_run (eval
//! `slug "a b"`) → "a-b" → a SECOND session_run on the SAME machine (heap
//! persists, re-entry via `add_function`/`run_fragment`) → session_close
//! (frees the machine).
//!
//! Requires `tidepool-extract` (the GHC→Core extractor) on `$PATH` or via
//! `TIDEPOOL_EXTRACT`; skips cleanly otherwise.

mod common;

use std::path::PathBuf;

use rmcp::model::{CallToolResult, RawContent};
use tidepool_handlers::{base_decls_with_ask, build_minimal_stack};
use tidepool_repl::{ReplServerConfig, TidepoolReplServer};
use tidepool_runtime::session::ModuleEnv;

/// True if the extract binary can be spawned (exit code irrelevant — the nix
/// wrapper supplies GHC internally, so we must NOT gate on `ghc --version`).
fn extract_available() -> bool {
    let bin = std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".into());
    std::process::Command::new(bin)
        .arg("--numeric-version")
        .output()
        .is_ok()
}

fn text_of(res: &CallToolResult) -> String {
    match &res.content[0].raw {
        RawContent::Text(t) => t.text.clone(),
        other => panic!("expected text content, got {other:?}"),
    }
}

fn build_server() -> TidepoolReplServer {
    let stack = build_minimal_stack();
    let (decls, ask_tag) = base_decls_with_ask(&stack);
    let effects_dir =
        tidepool_mcp::ensure_effects_module(&decls).expect("write Tidepool.Effects module");
    let prelude_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .join("haskell")
        .join("lib");
    let session_root_base = std::env::temp_dir().join(format!(
        "tidepool-repl-test-{}-{}",
        std::process::id(),
        // a per-test-run nonce so reruns don't collide on stale Lib.G<g> files
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let cfg = ReplServerConfig {
        decls,
        ask_tag,
        base_include: vec![effects_dir, prelude_dir],
        module_env: ModuleEnv::standalone_default(),
        session_root_base,
        nursery_size: None,
        continuation_ttl: None,
        wedged_ttl: None,
        turn_timeout: None,
    };
    TidepoolReplServer::new(stack, cfg)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_multi_turn_real_path() {
    if !extract_available() {
        eprintln!(
            "skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT / run in nix develop)"
        );
        return;
    }
    let repl = common::Repl {
        server: build_server(),
    };

    // 1. open
    let r = repl
        .server
        .dispatch_tool("session_open", serde_json::Map::new())
        .await
        .expect("session_open");
    assert_ne!(r.is_error, Some(true), "open errored: {}", text_of(&r));
    assert!(text_of(&r).contains("opened"));

    // 2. define `slug` (Lane A → Tidepool.Session.Lib.G1)
    let turn = repl.def("slug t = T.replace \" \" \"-\" t").await;
    assert!(!turn.is_error, "def errored: {}", turn.text);
    let txt = &turn.text;
    assert!(
        txt.contains("\"generation\":1") || txt.contains("\"generation\": 1"),
        "def: {txt}"
    );

    // 3. eval `slug "a b"` → "a-b" (bootstraps the resident machine)
    let turn = repl.eval("pure (slug \"a b\")").await;
    assert!(!turn.is_error, "eval 1 errored: {}", turn.text);
    assert!(turn.text.contains("a-b"), "eval 1 result: {}", turn.text);

    // 4. a SECOND eval on the SAME machine (re-entry; heap persists)
    let turn = repl.eval("pure (slug \"x y\")").await;
    assert!(!turn.is_error, "eval 2 errored: {}", turn.text);
    assert!(turn.text.contains("x-y"), "eval 2 result: {}", turn.text);

    // 5. close (drops the machine / frees the heap)
    let r = repl
        .server
        .dispatch_tool("session_close", serde_json::Map::new())
        .await
        .expect("session_close");
    assert_ne!(r.is_error, Some(true), "close errored: {}", text_of(&r));
    assert!(text_of(&r).contains("closed"));

    // after close a new turn must report no open session
    let turn = repl.eval("pure (slug \"a b\")").await;
    assert!(turn.is_error, "post-close eval should error");
    // Multi-session: the message now names the session ("no session 'default' open").
    let msg = &turn.text;
    assert!(
        msg.contains("no session") && msg.contains("open"),
        "unexpected: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn double_open_is_capped() {
    if !extract_available() {
        return;
    }
    let server = build_server();
    let r = server
        .dispatch_tool("session_open", serde_json::Map::new())
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true));
    // Same-name re-open is rejected: a second open for the same session name without closing the first must error.
    let r2 = server
        .dispatch_tool("session_open", serde_json::Map::new())
        .await
        .unwrap();
    assert_eq!(r2.is_error, Some(true));
    assert!(text_of(&r2).contains("already open"));
    let _ = server
        .dispatch_tool("session_close", serde_json::Map::new())
        .await;
}
