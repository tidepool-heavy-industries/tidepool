//! Reproduction test: T.splitOn in the full MCP preamble context.
//! Validates that alpha-renaming in resolveExternals prevents GHC Unique
//! collisions when multiple external unfoldings share local binder names.

use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

fn prelude_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("haskell/lib")
        .leak()
}

fn user_lib_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".tidepool/lib")
        .leak()
}

struct MockDispatcher;

impl DispatchEffect<()> for MockDispatcher {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<Value, tidepool_effect::error::EffectError> {
        Err(tidepool_effect::error::EffectError::UnhandledEffect { tag })
    }
}

#[test]
fn repro_spliton_full_mcp() {
    // Per #296: rewrite uses compile_and_run with stub dispatcher (option 1).
    // Earlier versions used compile_and_run_pure, which silently returned the
    // unevaluated Eff closure; the test passed without exercising T.splitOn at all.
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code = r#"pure (T.splitOn "," "a,b,c")"#;
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, "", "", None, None);

    let ulp = user_lib_dir();
    if !ulp.join("Library.hs").exists() {
        eprintln!("Skipping: .tidepool/lib/Library.hs not found");
        return;
    }

    let pp = prelude_dir();
    let include = [pp, ulp];

    let mut dispatcher = MockDispatcher;
    let result = compile_and_run(&source, "result", &include, &mut dispatcher, &());

    match &result {
        Ok(val) => eprintln!("OK: {:?}", val.to_json()),
        Err(e) => eprintln!("ERROR: {}", e),
    }

    let val = result.expect("T.splitOn in full MCP context should work");
    assert_eq!(
        val.to_json(),
        serde_json::json!(["a", "b", "c"]),
        "T.splitOn output mismatch"
    );
}

#[test]
fn repro_spliton_no_user_library() {
    // Per #296: rewrite uses compile_and_run with stub dispatcher (option 1).
    // Earlier versions used compile_and_run_pure, which silently returned the
    // unevaluated Eff closure; the test passed without exercising T.splitOn at all.
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false); // no Library
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code = r#"pure (T.splitOn "," "a,b,c")"#;
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, "", "", None, None);

    let pp = prelude_dir();
    let include = [pp];

    let mut dispatcher = MockDispatcher;
    let result = compile_and_run(&source, "result", &include, &mut dispatcher, &());

    match &result {
        Ok(val) => eprintln!("OK: {:?}", val.to_json()),
        Err(e) => eprintln!("ERROR: {}", e),
    }

    let val = result.expect("T.splitOn without Library should work");
    assert_eq!(
        val.to_json(),
        serde_json::json!(["a", "b", "c"]),
        "T.splitOn output mismatch"
    );
}
