//! Regression guard for the decl/stmt import-scope asymmetry fix.
//!
//! Before this fix, a `decl` item never got `import Library` regardless of
//! whether a project `Library` facade was on the include path (hardcoded
//! `false` in `session_decl_module_env`) — so a decl referencing a
//! Library-re-exported type (e.g. `Edit.EditOutcome`) needed its own explicit
//! import even though `:vocab` listed it as available, while a `stmt` item
//! (which gets the full eval preamble) did not. Fixed by threading a real
//! `user_library` flag through `session_decl_module_env`
//! (`tidepool-mcp/src/preamble.rs`) from `main.rs`.
//!
//! That fix alone would reintroduce the ambiguous-occurrence bug class
//! (BUG-7) the `stmt` path already guards against: importing `Library`
//! unqualified into every decl module risks colliding with a decl that
//! defines a name Library also re-exports (e.g. `Schemes.Rose`). Guarded by
//! a `hiding (...)` clause built from the session's own cumulative decl
//! heads (`render_module`, `tidepool-runtime/src/session/render.rs`) — the
//! same mechanism `hide_module_names` already applies on the stmt-preamble
//! side, ported to the decl-module import path.
//!
//! This test needs the repo's REAL `.tidepool/lib/Library.hs` (re-exports
//! `Edit.EditOutcome`, `Schemes.Rose`, …) — it is not self-contained like
//! most repl tests, since the whole point is exercising the real project
//! Library facade.

use std::path::PathBuf;

use rmcp::model::{CallToolResult, RawContent};
use tidepool_handlers::{
    base_decls_with_ask, build_base_stack, HandlerConfig, DEFAULT_OPENAI_MODEL,
};
use tidepool_repl::{ReplServerConfig, TidepoolReplServer};

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

/// Build a server with the real project `.tidepool/lib` on the include path
/// AND `session_decl_module_env(true)` (decl items get `import Library`).
/// Skips (via the caller's `extract_available` check) if the repo doesn't
/// have `.tidepool/lib/Library.hs` — this test is meaningless without it.
fn build_server_with_real_library(cwd: PathBuf) -> Option<TidepoolReplServer> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf();
    let project_lib = repo_root.join(".tidepool").join("lib");
    if !project_lib.join("Library.hs").is_file() {
        return None;
    }

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
    let prelude_dir = repo_root.join("haskell").join("lib");
    let base_include = vec![effects_dir, prelude_dir, project_lib];
    let session_root_base = std::env::temp_dir().join(format!(
        "tidepool-repl-declimp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let cfg = ReplServerConfig {
        decls,
        ask_tag,
        base_include,
        module_env: tidepool_mcp::session_decl_module_env(true),
        session_root_base,
        nursery_size: None,
        continuation_ttl: None,
        wedged_ttl: None,
        turn_timeout: None,
    };
    Some(TidepoolReplServer::new(stack, cfg))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decl_resolves_library_reexported_type_without_explicit_import() {
    if !extract_available() {
        return;
    }
    let cwd =
        std::env::temp_dir().join(format!("tidepool-repl-declimp-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).unwrap();
    let Some(server) = build_server_with_real_library(cwd) else {
        return;
    };

    server
        .dispatch_tool("session_open", serde_json::Map::new())
        .await
        .expect("open transport ok");

    // `EditOutcome` is defined in `.tidepool/lib/Edit.hs` and re-exported by
    // `Library` — NOT imported explicitly here. Before the fix this failed
    // "Not in scope: type constructor or class 'EditOutcome'".
    let mut args = serde_json::Map::new();
    args.insert(
        "items".to_string(),
        serde_json::Value::Array(vec![serde_json::Value::String(
            "describeOutcome :: EditOutcome -> String\ndescribeOutcome _ = \"outcome\"".to_string(),
        )]),
    );
    let r = server
        .dispatch_tool("session_run", args)
        .await
        .expect("dispatch ok");
    let text = text_of(&r);
    assert!(
        r.is_error != Some(true),
        "decl referencing Library-reexported EditOutcome should compile without an explicit \
         import: {text}"
    );

    server
        .dispatch_tool("session_close", serde_json::Map::new())
        .await
        .expect("close transport ok");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decl_defining_a_library_reexported_name_does_not_collide() {
    if !extract_available() {
        return;
    }
    let cwd =
        std::env::temp_dir().join(format!("tidepool-repl-declimp-cwd2-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).unwrap();
    let Some(server) = build_server_with_real_library(cwd) else {
        return;
    };

    server
        .dispatch_tool("session_open", serde_json::Map::new())
        .await
        .expect("open transport ok");

    // `Rose` is defined in `.tidepool/lib/Schemes.hs` (`data Rose a = Rose a
    // [Rose a]`) and re-exported by `Library`. Redefining it here would be an
    // "Ambiguous occurrence" without the hiding guard on `import Library`.
    let mut args = serde_json::Map::new();
    args.insert(
        "items".to_string(),
        serde_json::Value::Array(vec![serde_json::Value::String(
            "data Rose = RoseLeaf | RoseNode Rose Rose".to_string(),
        )]),
    );
    let r = server
        .dispatch_tool("session_run", args)
        .await
        .expect("dispatch ok");
    let text = text_of(&r);
    assert!(
        !text.contains("Ambiguous occurrence"),
        "redefining Library's Rose must not be an ambiguous occurrence \
         (the hiding guard should prevent it): {text}"
    );
    assert!(
        r.is_error != Some(true),
        "decl shadowing Library's Rose should compile cleanly: {text}"
    );

    server
        .dispatch_tool("session_close", serde_json::Map::new())
        .await
        .expect("close transport ok");
}
