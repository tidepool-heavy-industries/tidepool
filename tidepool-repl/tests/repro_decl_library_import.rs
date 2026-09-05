//! Regression guard for the decl/stmt import-scope asymmetry fix.
//!
//! A `decl` item must receive `import Library` whenever
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

use crate::common;

use std::path::PathBuf;

use rmcp::model::{CallToolResult, RawContent};
use tidepool_handlers::{build_base_stack, HandlerConfig, DEFAULT_OPENAI_MODEL};
use tidepool_mcp::EffectRoster;
use tidepool_repl::{ReplServerConfig, TidepoolReplServer};

use common::require_extract;

fn text_of(res: &CallToolResult) -> String {
    match &res.content[0].raw {
        RawContent::Text(t) => t.text.clone(),
        other => panic!("expected text content, got {other:?}"),
    }
}

/// Build a server with the real project `.tidepool/lib` on the include path
/// AND `session_decl_module_env(true)` (decl items get `import Library`).
/// `.tidepool/lib/Library.hs` is a committed repo fixture, not an optional
/// artifact — its absence is a broken checkout, not a legitimate skip.
fn build_server_with_real_library(cwd: PathBuf) -> TidepoolReplServer {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf();
    let project_lib = repo_root.join(".tidepool").join("lib");
    assert!(
        project_lib.join("Library.hs").is_file(),
        ".tidepool/lib/Library.hs not found — this test is meaningless without \
         the real project Library facade; check the checkout"
    );

    let kv_path = cwd.join("kv.json");
    let handler_cfg = HandlerConfig {
        cwd,
        kv_path,
        llm_model: DEFAULT_OPENAI_MODEL.to_string(),
    };
    let stack = build_base_stack(&handler_cfg);
    let roster = EffectRoster::from_handlers(&stack);
    let effects_dir =
        tidepool_mcp::ensure_effects_module(roster.decls()).expect("write Tidepool.Effects module");
    let prelude_dir = repo_root.join("haskell").join("lib");
    let mut base_include = effects_dir.include_paths().to_vec();
    base_include.push(prelude_dir);
    base_include.push(project_lib);
    let session_root_base = std::env::temp_dir().join(format!(
        "tidepool-repl-declimp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let module_env = tidepool_mcp::session_decl_module_env(roster.decls(), true);
    let cfg = ReplServerConfig {
        roster,
        base_include,
        module_env,
        session_root_base,
        nursery_size: None,
        continuation_ttl: None,
        wedged_ttl: None,
        turn_timeout: None,
        lib_dirs: Vec::new(),
        stdlib_dir: None,
        patterns_path: None,
    };
    TidepoolReplServer::new(stack, cfg)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decl_resolves_library_reexported_type_without_explicit_import() {
    require_extract();
    let cwd =
        std::env::temp_dir().join(format!("tidepool-repl-declimp-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).unwrap();
    let server = build_server_with_real_library(cwd);

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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decl_defining_a_library_reexported_name_does_not_collide() {
    require_extract();
    let cwd =
        std::env::temp_dir().join(format!("tidepool-repl-declimp-cwd2-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).unwrap();
    let server = build_server_with_real_library(cwd);

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
}
