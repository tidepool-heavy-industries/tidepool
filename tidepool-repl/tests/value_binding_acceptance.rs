//! Wave 3b — THE HEADLINE ACCEPTANCE SWEEP: value binding end-to-end through the
//! REAL `tidepool-repl` entry point, multi-turn, with organic GC between bind and
//! read (the standing rule: production tool dispatch over real turns, natural
//! allocation/collection — never a bespoke harness or forced GC).
//!
//! Binds an Int (Tier-0 scalar), a JSON `Value` (Tier-0 structured + DataConTable
//! render), and a function (Tier-1 closure — proves prior-fragment code stays
//! callable after `add_function`); reads/calls each back several turns later,
//! AFTER a real collection forced by a small session nursery + heavy allocation.
//!
//! Requires the Wave-3b `tidepool-extract` (set `TIDEPOOL_EXTRACT`, with the
//! with-packages GHC on `PATH` + `TIDEPOOL_GHC_LIBDIR`); skips cleanly otherwise.

use std::path::PathBuf;

use rmcp::model::{CallToolResult, RawContent};
use tidepool_handlers::{base_decls_with_ask, build_minimal_stack};
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

/// Build a server with a deliberately SMALL nursery (2 MiB) so the heavy-alloc
/// turns force a real minor GC between bind and read.
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
        "tidepool-repl-vb-{}-{}",
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
        nursery_size: Some(1 << 21), // 2 MiB — small enough to GC organically
    };
    TidepoolReplServer::new(stack, cfg)
}

fn s(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), serde_json::Value::String((*v).to_string())))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn value_binding_int_json_function_survive_gc() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let server = build_server();

    // helper: dispatch session_eval and assert no error, returning the text.
    async fn eval(server: &TidepoolReplServer, code: &str) -> String {
        let r = server
            .dispatch_tool("session_eval", s(&[("code", code)]))
            .await
            .expect("session_eval dispatch");
        assert_ne!(r.is_error, Some(true), "turn `{code}` errored: {}", text_of(&r));
        text_of(&r)
    }

    // 0. open
    let r = server
        .dispatch_tool("session_open", serde_json::Map::new())
        .await
        .expect("session_open");
    assert_ne!(r.is_error, Some(true), "open: {}", text_of(&r));

    // 0b. define a custom ADT (Lane A → Tidepool.Session.Lib.G1). A value of this
    //     user type, bound below, is what proves the DataConTable MERGE: its `Box`
    //     constructor is registered on the bind turn and must resolve on a LATER
    //     turn's case-match (gen-versioned module addressing, not a wired-in con).
    let r = server
        .dispatch_tool("session_def", s(&[("decl", "data Box = Box Int")]))
        .await
        .expect("session_def");
    assert_ne!(r.is_error, Some(true), "def Box: {}", text_of(&r));

    // 1. BIND an Int (Tier-0 scalar) — bootstraps the resident machine.
    let t = eval(&server, "x <- pure (42 :: Int)").await;
    assert!(t.contains("bound"), "bind x: {t}");

    // 2. read it back a later turn: x + 1 => 43 (pure reference, ExternalEnv slot-load).
    let t = eval(&server, "x + 1").await;
    assert!(t.contains("43"), "x + 1: {t}");

    // 3. BIND a custom-ADT value (Tier-0 structured; the `Box` con enters the
    //    session table on THIS turn).
    let t = eval(&server, "b <- pure (Box 7)").await;
    assert!(t.contains("bound"), "bind b: {t}");

    // 4. case-match it a later turn — the `Box` con must resolve from the merged
    //    session DataConTable (bound a turn ago) against the tenured heap value.
    let t = eval(&server, "case b of Box n -> n + 100").await;
    assert!(t.contains("107"), "case b: {t}");

    // 5. ORGANIC GC: a heavy strict fold allocates ~6 MiB of transient cons into
    //    the 2 MiB nursery → multiple real minor collections.
    let t = eval(&server, "foldl' (+) (0 :: Int) [1..200000]").await;
    assert!(t.contains("20000100000"), "fold sum: {t}");

    // 6. BIND a function (Tier-1 closure — stored as-is, not deep-forced).
    let t = eval(&server, "f <- pure (\\n -> n + (1 :: Int))").await;
    assert!(t.contains("bound"), "bind f: {t}");

    // 7. call it a later turn: f 10 => 11 (prior-fragment code still callable).
    let t = eval(&server, "f 10").await;
    assert!(t.contains("11"), "f 10: {t}");

    // 8. MORE organic GC between the binds and the final re-reads.
    let _ = eval(&server, "foldl' (+) (0 :: Int) [1..200000]").await;

    // 9. AFTER the collections, every binding still resolves/renders correctly.
    let t = eval(&server, "x + 1").await;
    assert!(t.contains("43"), "post-GC x + 1: {t}");
    let t = eval(&server, "case b of Box n -> n + 100").await;
    assert!(t.contains("107"), "post-GC case b: {t}");
    let t = eval(&server, "f 10").await;
    assert!(t.contains("11"), "post-GC f 10: {t}");

    // 10. :bindings lists all three current bindings.
    let r = server
        .dispatch_tool("session_cmd", s(&[("command", ":bindings")]))
        .await
        .expect("session_cmd :bindings");
    let t = text_of(&r);
    assert!(t.contains("\"x\"") && t.contains("\"b\"") && t.contains("\"f\""), ":bindings: {t}");

    // 11. close.
    let r = server
        .dispatch_tool("session_close", serde_json::Map::new())
        .await
        .expect("session_close");
    assert_ne!(r.is_error, Some(true), "close: {}", text_of(&r));
}
