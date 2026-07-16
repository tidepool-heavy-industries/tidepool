//! End-to-end proof that `LspNode` (#346) is Show-able and record-dot
//! accessible like the other result records (Hit/Proc/FileRead/Commit — see
//! `bridged_records_extract.rs` for the Commit/StatusEntry/FileDelta version
//! of this same proof).
//!
//! Needs the with-packages GHC on PATH and `TIDEPOOL_EXTRACT` pointing at a
//! freshly built extract binary (see haskell/CLAUDE.md). Skips (passes) when
//! `TIDEPOOL_EXTRACT` is unset so a plain `cargo test` on a checkout without the
//! toolchain does not fail.

use std::path::Path;
use tidepool_testing::NullDispatcher;
use tidepool_eval::value::Value;
use tidepool_testing::eval_harness::EvalHarness;

fn eval_raw(code: &str) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, "", "", None, None);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let lib = root.join(".tidepool/lib");
    EvalHarness::new()
        .with_stdlib()
        .with_include(lib)
        .with_effects_module()
        .run(&src, "result", NullDispatcher)
        .into_result()
        .map(|v| v.to_json())
        .map_err(|e| format!("{e}"))
}

fn eval_ok(code: &str, expected: serde_json::Value) {
    match eval_raw(code) {
        Ok(got) => assert_eq!(
            got, expected,
            "\n  code: {code}\n  want: {expected}\n  got: {got}"
        ),
        Err(e) => panic!("\n  code: {code}\n  compile/run error: {e}"),
    }
}

#[test]
fn lspnode_is_show_and_dot_accessible() {
    if !tidepool_testing::eval_harness::extract_available() {
        eprintln!("skipping: TIDEPOOL_EXTRACT not set (no extract toolchain)");
        return;
    }
    let node =
        r#"(LspNode "myFunc" "MyModule" "function" "src/Lib.hs" (Position 10 4) "myFunc x = x")"#;

    // Record-dot on the REAL (prefixed) accessors.
    eval_ok(
        &format!(r#"pure ({node}.nodeName)"#),
        serde_json::json!("myFunc"),
    );
    eval_ok(
        &format!(r#"pure ({node}.nodeFile)"#),
        serde_json::json!("src/Lib.hs"),
    );
    eval_ok(
        &format!(r#"pure ({node}.nodePos.posLine)"#),
        serde_json::json!(10),
    );
    eval_ok(&format!(r#"pure (nodeLine {node})"#), serde_json::json!(10));

    // Show: a derived instance, not ToJSON — the Text `show` produces is what
    // gets compared (as a JSON string), same as any other `Show a => a -> Text`
    // eval result.
    eval_ok(
        &format!(r#"pure (show {node})"#),
        serde_json::json!(
            "LspNode {nodeName = \"myFunc\", nodeContainer = \"MyModule\", nodeKind = \"function\", nodeFile = \"src/Lib.hs\", nodePos = Position {posLine = 10, posChar = 4}, nodeText = \"myFunc x = x\"}"
        ),
    );
}
