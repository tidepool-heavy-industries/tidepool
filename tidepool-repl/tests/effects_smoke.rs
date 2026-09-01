//! Smoke test: the FULL effect suite (`build_base_stack`) is reachable
//! through the repl's `session_run`, composed over persistent session state.
//!
//! Exercises the always-available effects — Exec (`run`), Fs (`writeFile`/
//! `readFile`), and KV (`kvSet`/`kvGet` across turns) — to prove the wider stack
//! (Console, KV, Fs, Http, Exec, Llm, Git, Time + Ask) wires through the
//! session worker. The cwd/KV sandbox is a fresh tempdir so the effects are isolated.
//! Skips cleanly when the extract isn't available. (Llm needs API creds, so
//! that is smoke-tested live, not here.)

mod common;

use common::{build_full_server, require_extract, run_single, text_of};
use tidepool_repl::TidepoolReplServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_stack_effects_reachable_through_session() {
    require_extract();
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = build_full_server(tmp.path().to_path_buf(), "fx", true);

    async fn eval(server: &TidepoolReplServer, code: &str) -> String {
        let (is_error, text) = run_single(server, code, None).await;
        assert!(!is_error, "turn `{code}` errored: {text}");
        text
    }

    // Exec + Fs fused into one item: `run` a shell command AND round-trip a
    // file write/read, returned as a tuple — two independent assertions over
    // one compile of the same full stack. (A plain bound-value reference under
    // the full stack is covered by gc_field_replay.rs's `gitLog` binding.)
    let t = eval(
        &server,
        "do { p <- run \"echo wave-b-ok\" >>= liftEither ; \
              writeFile \"hello.txt\" \"from-fs\" >>= liftEither ; \
              contents <- readFile \"hello.txt\" >>= liftEither ; \
              pure (p.stdout, contents) }",
    )
    .await;
    assert!(t.contains("wave-b-ok"), "Exec/run output: {t}");
    assert!(t.contains("from-fs"), "Fs read-back: {t}");

    // KV: set on one turn, get on a LATER turn — proves effect state persists
    // alongside the resident heap.
    let _ = eval(&server, "kvSet \"wave-b\" (toJSON (42 :: Int))").await;
    let t = eval(&server, "kvGet \"wave-b\"").await;
    assert!(t.contains("42"), "KV get-after-set: {t}");

    // Typed KV read (`Tidepool.Kv.kvGetAs`), same `@T` idiom as `askUser`/
    // `fork`: present key decodes to `Right (Just a)`, absent key to
    // `Right Nothing`, and a shape mismatch to a legible `Left` — never a
    // crash. The untyped `kvGet` above is unaffected.
    let t = eval(&server, "kvGetAs @Int \"wave-b\"").await;
    assert!(
        t.contains("Right") && t.contains("42"),
        "kvGetAs present-key round-trip: {t}"
    );
    let t = eval(&server, "kvGetAs @Int \"wave-b-absent\"").await;
    assert!(
        // Right Nothing renders through the JSON envelope as {"Right":null},
        // not the literal text "Nothing" (Maybe's ToJSON, not Show).
        t.contains("Right") && t.contains("null"),
        "kvGetAs absent-key: {t}"
    );
    let _ = eval(&server, "kvSet \"wave-b-str\" (toJSON (\"hello\" :: Text))").await;
    let t = eval(&server, "kvGetAs @Int \"wave-b-str\"").await;
    assert!(
        t.contains("Left"),
        "kvGetAs decode-failure should be a legible Left, not a crash: {t}"
    );

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
}

/// Declarations and later `session_run` items share the full resident
/// vocabulary: `M`/`Eff`/`Member`, effect verbs, Prelude shadows, and qualified
/// library namespaces. Persisted declarations may therefore remain
/// row-polymorphic until a later invocation selects the concrete session row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_def_sees_full_eval_vocabulary() {
    require_extract();
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = build_full_server(tmp.path().to_path_buf(), "fx", true);

    // Exercise both the convenient concrete `M` alias and a three-effect
    // `Member`-polymorphic program alongside qualified library namespaces.
    let (is_error, text) = run_single(
        &server,
        concat!(
            include_str!("effects_smoke/member_composition.hs"),
            "\n\n\
             sh :: Text -> M Text\n\
         sh cmd = run cmd >>= liftEither <&> \\p -> p.stdout\n\
         \n\
         uniqSorted :: [Int] -> [Int]\n\
         uniqSorted = L.sort . Set.toList . Set.fromList\n\
         \n\
         -- shell-effect module (Git) must be in DECL scope too\n\
         dirtyCount :: M Int\n\
         dirtyCount = Git.gitStatus >>= liftEither <&> length",
        ),
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

    // The custom Haskell program composes Exec, FsWrite, and FsRead without
    // naming their row order. Both branches execute through the production
    // Rust handlers: the first round-trips command output through the sandbox,
    // while the second exposes the filesystem authority rejection as the
    // program's local typed error.
    let escape_name = format!(
        "{}-member-composition-escape.txt",
        tmp.path()
            .file_name()
            .expect("tempdir basename")
            .to_string_lossy()
    );
    let escaped_path = tmp
        .path()
        .parent()
        .expect("tempdir parent")
        .join(&escape_name);
    assert!(
        !escaped_path.exists(),
        "escape probe path must start absent"
    );
    let code = format!(
        "do {{ ok <- captureCommand \"printf member-composed-ok\" \"member-composed.txt\"; \
         denied <- captureCommand \"printf denied\" \"../{escape_name}\"; \
         pure (ok, denied) }}"
    );
    let (is_error, text) = run_single(&server, &code, None).await;
    assert!(!is_error, "calling `captureCommand`: {text}");
    assert!(
        text.contains("member-composed-ok") && text.contains("CaptureFs"),
        "composed effect results: {text}"
    );
    assert!(
        text.contains("FsSandbox"),
        "sandbox failure should stay typed and local: {text}"
    );
    assert!(
        !escaped_path.exists(),
        "Rust filesystem authority must reject the escape"
    );

    // Use the pure decl that needed L./Set.
    let (is_error, text) = run_single(&server, "pure (uniqSorted [3,1,2,3,1])", None).await;
    assert!(!is_error, "calling `uniqSorted`: {text}");
    assert!(
        text.contains('1') && text.contains('3'),
        "uniqSorted output: {text}"
    );
}

/// Run a multi-item `session_run` block and return the parsed result JSON
/// (the `{items, value, type?, truncated?}` envelope), stripping any
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
    require_extract();
    let tmp = tempfile::tempdir().expect("tempdir");
    let server = build_full_server(tmp.path().to_path_buf(), "fx", true);

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
    assert_eq!(
        last_item.get("type").and_then(|t| t.as_str()),
        Some("Int"),
        "bare pure reference should report its type, not null; got: {last_item}"
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
    let last_item = items.last().expect("at least one item");
    assert_eq!(
        last_item.get("type").and_then(|t| t.as_str()),
        Some("[Int]"),
        "monadic expr with trailing `where` should report its inner type, not null; got: {last_item}"
    );
    // The eff path renders via Show-default (toWire); a [Int] renders as a
    // structural JSON array (ToWire container instances) with native number
    // leaves (ToWire Int delegates to toJSON) — the fix under test is the
    // non-null TYPE above; here we confirm both elements as JSON numbers.
    let val_arr = v
        .get("value")
        .and_then(|x| x.as_array())
        .unwrap_or_else(|| panic!("where-expr value should be a JSON array; got: {v}"));
    assert!(
        val_arr.contains(&serde_json::json!(10)) && val_arr.contains(&serde_json::json!(20)),
        "where-expr value should contain both elements; got: {v}"
    );
}
