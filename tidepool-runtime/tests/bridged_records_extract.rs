//! End-to-end proof that user eval code compiles + runs against the GENERATED
//! `Tidepool.Records.Bridged` decls (the Git wire records, whose Rust structs
//! are the single source of truth). Compiling the full MCP preamble already
//! forces `Tidepool.Effects` → `Tidepool.Prelude` → `Tidepool.Records` →
//! `Tidepool.Records.Bridged` to build; these probes additionally construct the
//! records and read fields via record-dot, exercising the generated
//! constructor + selectors.
//!
//! Needs the with-packages GHC on PATH and `TIDEPOOL_EXTRACT` pointing at a
//! freshly built extract binary (see haskell/CLAUDE.md). Skips (passes) when
//! `TIDEPOOL_EXTRACT` is unset so a plain `cargo test` on a checkout without the
//! toolchain does not fail.

use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

struct NullDispatcher;
impl DispatchEffect<()> for NullDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        cx.respond(serde_json::json!(0))
    }
}

fn eval_raw(code: &str) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, "", "", None, None);
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib");
    let lib = root.join(".tidepool/lib");
    let include = [hs.as_path(), lib.as_path(), effects_dir.as_path()];
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(v) => Ok(v.to_json()),
        Err(e) => Err(format!("{e}")),
    }
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
fn user_code_compiles_against_generated_bridged_decls() {
    if std::env::var_os("TIDEPOOL_EXTRACT").is_none() {
        eprintln!("skipping: TIDEPOOL_EXTRACT not set (no extract toolchain)");
        return;
    }
    // Construct a generated `Commit` and read a Text field via record-dot.
    eval_ok(
        r#"pure ((Commit "deadbeef" "subj" "auth" "date" ["a.hs","b.hs"]).sha)"#,
        serde_json::json!("deadbeef"),
    );
    // A [Text] field.
    eval_ok(
        r#"pure ((Commit "s" "subj" "auth" "date" ["a.hs","b.hs"]).files)"#,
        serde_json::json!(["a.hs", "b.hs"]),
    );
    // FileDelta exercises Int + Bool generated fields.
    eval_ok(
        r#"pure ((FileDelta "x.rs" 3 1 False).adds)"#,
        serde_json::json!(3),
    );
    eval_ok(
        r#"pure ((FileDelta "x.rs" 3 1 True).binary)"#,
        serde_json::json!(true),
    );
    // StatusEntry.
    eval_ok(
        r#"pure ((StatusEntry "p" "M ").state)"#,
        serde_json::json!("M "),
    );
    // The generated ToJSON (orphan instance in Records.hs) still works.
    eval_ok(
        r#"pure (toJSON (StatusEntry "p" "M "))"#,
        serde_json::json!({"path": "p", "state": "M "}),
    );
}
