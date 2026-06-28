//! Wave B smoke test: the FULL effect suite (`build_base_stack`) is reachable
//! through the repl's `session_eval`, composed over persistent session state.
//!
//! Exercises the always-available effects — Exec (`run`), Fs (`writeFile`/
//! `readFile`), and KV (`kvSet`/`kvGet` across turns) — to prove the wider stack
//! (Console, KV, Fs, SG, Http, Exec, Lsp, Llm + Ask) wires through the session
//! worker. The cwd/KV sandbox is a fresh tempdir so the effects are isolated.
//! Skips cleanly when the extract isn't available. (LSP is daemon-gated and Llm
//! needs API creds, so those are smoke-tested live, not here.)

use std::path::PathBuf;

use rmcp::model::{CallToolResult, RawContent};
use tidepool_handlers::{base_decls_with_ask, build_base_stack, HandlerConfig, DEFAULT_OPENAI_MODEL};
use tidepool_repl::{ReplServerConfig, TidepoolReplServer};
use tidepool_runtime::session::ModuleEnv;

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

fn obj(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
        .collect()
}

/// Build a server with the FULL effect stack (the Wave-B default), rooted at
/// `cwd` so Fs/Exec/KV operate in an isolated sandbox.
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
    let prelude_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .join("haskell")
        .join("lib");
    let session_root_base = std::env::temp_dir().join(format!(
        "tidepool-repl-fx-{}-{}",
        std::process::id(),
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
    };
    TidepoolReplServer::new(stack, cfg)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_stack_effects_reachable_through_session() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = build_full_server(tmp.path().to_path_buf());

    async fn eval(server: &TidepoolReplServer, code: &str) -> String {
        let r = server
            .dispatch_tool("session_eval", obj(&[("code", code)]))
            .await
            .expect("session_eval dispatch");
        assert_ne!(r.is_error, Some(true), "turn `{code}` errored: {}", text_of(&r));
        text_of(&r)
    }

    let r = server
        .dispatch_tool("session_open", serde_json::Map::new())
        .await
        .expect("session_open");
    assert_ne!(r.is_error, Some(true), "open: {}", text_of(&r));

    // Exec: `run` a shell command — the result is (exit, stdout, stderr).
    let t = eval(&server, "run \"echo wave-b-ok\"").await;
    assert!(t.contains("wave-b-ok"), "Exec/run output: {t}");

    // Fs: write then read a file in the cwd sandbox (round-trip).
    let t = eval(
        &server,
        "writeFile \"hello.txt\" \"from-fs\" >> readFile \"hello.txt\"",
    )
    .await;
    assert!(t.contains("from-fs"), "Fs read-back: {t}");

    // KV: set on one turn, get on a LATER turn — proves effect state persists
    // alongside the resident heap.
    let _ = eval(&server, "kvSet \"wave-b\" (toJSON (42 :: Int))").await;
    let t = eval(&server, "kvGet \"wave-b\"").await;
    assert!(t.contains("42"), "KV get-after-set: {t}");

    // The full stack didn't break a plain bound-value reference.
    let _ = eval(&server, "x <- pure (7 :: Int)").await;
    let t = eval(&server, "x + 1").await;
    assert!(t.contains("8"), "bound-value ref under full stack: {t}");

    let _ = server
        .dispatch_tool("session_close", serde_json::Map::new())
        .await;
}
