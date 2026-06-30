//! Wave B smoke test: the FULL effect suite (`build_base_stack`) is reachable
//! through the repl's `session_run`, composed over persistent session state.
//!
//! Exercises the always-available effects — Exec (`run`), Fs (`writeFile`/
//! `readFile`), and KV (`kvSet`/`kvGet` across turns) — to prove the wider stack
//! (Console, KV, Fs, SG, Http, Exec, Lsp, Llm + Ask) wires through the session
//! worker. The cwd/KV sandbox is a fresh tempdir so the effects are isolated.
//! Skips cleanly when the extract isn't available. (LSP is daemon-gated and Llm
//! needs API creds, so those are smoke-tested live, not here.)

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
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf();
    let prelude_dir = repo_root.join("haskell").join("lib");
    // Project verb library (parity with production): puts `Library` on the
    // include path so the preamble auto-imports it and `.tidepool/lib` verbs are
    // in scope.
    let project_lib = repo_root.join(".tidepool").join("lib");
    let mut base_include = vec![effects_dir, prelude_dir];
    if project_lib.is_dir() {
        base_include.push(project_lib);
    }
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
        base_include,
        // Match production (`main.rs`): the full-stack server gives Lane-A decls
        // the eval pragmas+imports, so `session_def` helpers share the eval
        // vocabulary (`M`, the effect verbs, `L.`/`Set.`, the Prelude shadows).
        module_env: tidepool_mcp::session_decl_module_env(),
        session_root_base,
        nursery_size: None,
        continuation_ttl: None,
    };
    TidepoolReplServer::new(stack, cfg)
}

/// Dispatch a 1-item `session_run` block and unwrap `items[0]`.
/// Returns `(is_error, result_text)` after stripping the block envelope.
async fn run_single(
    server: &TidepoolReplServer,
    item: &str,
    input: Option<serde_json::Value>,
) -> (bool, String) {
    let mut args = serde_json::Map::new();
    args.insert(
        "items".into(),
        serde_json::Value::Array(vec![serde_json::Value::String(item.to_string())]),
    );
    if let Some(inp) = input {
        args.insert("input".into(), inp);
    }
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
            let text = item0
                .get("result")
                .and_then(|r| r.as_str())
                .unwrap_or(&raw)
                .to_string();
            return (!ok, text);
        }
    }
    (raw_is_error, raw)
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
        let (is_error, text) = run_single(server, code, None).await;
        assert!(!is_error, "turn `{code}` errored: {text}");
        text
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

    // Project `Library` is auto-imported (parity with eval): a `.tidepool/lib`
    // verb is in scope bare. `chunksOf` is a pure Schemes verb re-exported by
    // Library. (Regression for: REPL listed lib verbs in :vocab but couldn't
    // call them.)
    let t = eval(&server, "pure (chunksOf 2 [1,2,3,4,5 :: Int])").await;
    assert!(
        t.contains("[\n      1") || t.contains("[1") || t.contains('5'),
        "Library verb chunksOf should be in scope + return chunks: {t}"
    );

    // input payload lane: the `input :: Aeson.Value` binding is in scope and the
    // `Aeson.` qualifier the injection emits resolves (regression for the
    // missing-Aeson-import bug found in live dogfood).
    let (is_error, text) = run_single(
        &server,
        "pure (input ^? key \"name\" . _String)",
        Some(serde_json::json!({"name": "from-the-input-lane", "n": 42})),
    )
    .await;
    assert!(!is_error, "input lane errored: {text}");
    assert!(
        text.contains("from-the-input-lane"),
        "input lane value: {text}",
    );

    let _ = server
        .dispatch_tool("session_close", serde_json::Map::new())
        .await;
}

/// `session_run` items see the FULL eval vocabulary — `M` + the effect verbs,
/// the `Tidepool.Prelude` shadows, and the `L.`/`Set.` qualified namespaces —
/// not just the lens-free `T`/`Map` of `standalone_default`. This is the payoff
/// of the production `session_decl_module_env`: a decl item can be an effectful
/// verb (`sh :: Text -> M Text`) and use list/set combinators, then be called
/// from a later `session_run`. (Regression for the decl/eval preamble asymmetry.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_def_sees_full_eval_vocabulary() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = build_full_server(tmp.path().to_path_buf());

    let r = server
        .dispatch_tool("session_open", serde_json::Map::new())
        .await
        .expect("session_open");
    assert_ne!(r.is_error, Some(true), "open: {}", text_of(&r));

    // A decl that uses `M` + the `run` effect verb (Tidepool.Effects) AND the
    // `L.`/`Set.` qualified namespaces — all out of scope under the old
    // T+Map-only decl preamble.
    let (is_error, text) = run_single(
        &server,
        "sh :: Text -> M Text\n\
         sh cmd = run cmd <&> \\(_,out,_) -> out\n\
         \n\
         uniqSorted :: [Int] -> [Int]\n\
         uniqSorted = L.sort . Set.toList . Set.fromList\n\
         \n\
         -- shell-effect module (Git) must be in DECL scope too\n\
         dirtyCount :: M Int\n\
         dirtyCount = Git.gitStatus <&> length",
        None,
    )
    .await;
    assert!(
        !is_error,
        "session_run with full vocabulary should compile: {text}",
    );

    // Use the effectful decl from a later eval turn.
    let (is_error, text) = run_single(&server, "sh \"echo decl-vocab-ok\"", None).await;
    assert!(!is_error, "calling `sh`: {text}");
    assert!(text.contains("decl-vocab-ok"), "sh output: {text}");

    // Use the pure decl that needed L./Set.
    let (is_error, text) = run_single(&server, "pure (uniqSorted [3,1,2,3,1])", None).await;
    assert!(!is_error, "calling `uniqSorted`: {text}");
    assert!(
        text.contains('1') && text.contains('3'),
        "uniqSorted output: {text}"
    );

    let _ = server
        .dispatch_tool("session_close", serde_json::Map::new())
        .await;
}

/// Run a multi-item `session_run` block and return the parsed result JSON
/// (the `{items, value, generation, valGeneration}` envelope), stripping any
/// `## Output` / `## Result` framing.
async fn run_block(
    server: &TidepoolReplServer,
    items: &[&str],
    input: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut args = serde_json::Map::new();
    args.insert(
        "items".into(),
        serde_json::Value::Array(
            items
                .iter()
                .map(|s| serde_json::Value::String(s.to_string()))
                .collect(),
        ),
    );
    if let Some(inp) = input {
        args.insert("input".into(), inp);
    }
    let r = server
        .dispatch_tool("session_run", args)
        .await
        .expect("session_run dispatch");
    let raw = text_of(&r);
    let json_part = match raw.rfind("\n## Result\n") {
        Some(pos) => &raw[pos + "\n## Result\n".len()..],
        None => &raw,
    };
    serde_json::from_str(json_part).unwrap_or_else(|_| serde_json::json!({"raw": raw}))
}

/// Block-runner cleanups (dogfood findings, fixed inline):
///   1. the `input` lane decodes a stringified-JSON payload (MCP clients
///      double-encode it) — matching the stateless `eval` tool;
///   2. `input` is in scope for `let`/bind items, not just bare expressions;
///   3. a bare pure expression reports its inferred `type`, not `null`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_runner_input_and_type_cleanups() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = build_full_server(tmp.path().to_path_buf());
    let r = server
        .dispatch_tool("session_open", serde_json::Map::new())
        .await
        .expect("session_open");
    assert_ne!(r.is_error, Some(true), "open: {}", text_of(&r));

    // (1)+(2): input arrives DOUBLE-ENCODED as a JSON string (the MCP-client
    // shape); it must decode to a structured Value AND be visible to a `let`
    // item, then to the final bare-expression reference.
    let stringified = serde_json::Value::String(r#"{"name": "Inanna", "n": 42}"#.to_string());
    let v = run_block(
        &server,
        &["let who = input ^? key \"name\" . _String", "who"],
        Some(stringified),
    )
    .await;
    assert_eq!(
        v.get("value").cloned().unwrap_or(serde_json::Value::Null),
        serde_json::json!("Inanna"),
        "input lane should decode + be in `let` scope; got: {v}"
    );

    // (3): a bare pure expression referencing a binding reports its inferred
    // type (not null). `doubled :: Int`.
    let v = run_block(
        &server,
        &["n <- pure (21 :: Int)", "let doubled = n * 2", "doubled"],
        None,
    )
    .await;
    let items = v
        .get("items")
        .and_then(|i| i.as_array())
        .expect("items array");
    let last_item = items.last().expect("at least one item");
    let result_str = last_item
        .get("result")
        .and_then(|r| r.as_str())
        .unwrap_or("");
    let result_json: serde_json::Value = serde_json::from_str(result_str).unwrap_or_default();
    assert_eq!(
        result_json.get("type").and_then(|t| t.as_str()),
        Some("Int"),
        "bare pure reference should report its type, not null; got: {result_str}"
    );
    assert_eq!(v.get("value"), Some(&serde_json::json!(42)), "value: {v}");

    // (4): a MONADIC expression carrying a trailing `where` reports its INNER
    // type (the eff-first path's type probe must tolerate `where` — it hoists the
    // expr to a module-level `__probe` binding where `where` attaches legally).
    // Regression for the `type: null` wart on `<expr> where …`.
    let v = run_block(
        &server,
        &["pure (take 2 ys) where ys = [10, 20, 30] :: [Int]"],
        None,
    )
    .await;
    let items = v
        .get("items")
        .and_then(|i| i.as_array())
        .expect("items array");
    let result_str = items
        .last()
        .and_then(|it| it.get("result"))
        .and_then(|r| r.as_str())
        .unwrap_or("");
    let result_json: serde_json::Value = serde_json::from_str(result_str).unwrap_or_default();
    assert_eq!(
        result_json.get("type").and_then(|t| t.as_str()),
        Some("[Int]"),
        "monadic expr with trailing `where` should report its inner type, not null; got: {result_str}"
    );
    // The eff path renders via Show-default (toWire), so a [Int] comes back as the
    // Show string "[10,20]" — the fix under test is the non-null TYPE above; here
    // we only confirm the value carries both elements.
    let val_str = v.get("value").and_then(|x| x.as_str()).unwrap_or("");
    assert!(
        val_str.contains("10") && val_str.contains("20"),
        "where-expr value should contain both elements; got: {v}"
    );

    let _ = server
        .dispatch_tool("session_close", serde_json::Map::new())
        .await;
}
