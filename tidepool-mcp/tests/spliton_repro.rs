//! Reproduction test: T.splitOn in the full MCP preamble context.
//! Validates that alpha-renaming in resolveExternals prevents GHC Unique
//! collisions when multiple external unfoldings share local binder names.

use std::path::Path;

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

#[test]
fn repro_spliton_full_mcp() {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code = r#"pure (T.splitOn "," "a,b,c")"#;
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, "", "", None, Some(4096));

    let ulp = user_lib_dir();
    if !ulp.join("Library.hs").exists() {
        eprintln!("Skipping: .tidepool/lib/Library.hs not found");
        return;
    }

    let pp = prelude_dir();
    let include = [pp, ulp];
    let result = tidepool_runtime::compile_and_run_pure(&source, "result", &include);
    match &result {
        Ok(val) => eprintln!("OK: {:?}", val.to_json()),
        Err(e) => eprintln!("ERROR: {}", e),
    }
    assert!(
        result.is_ok(),
        "T.splitOn in full MCP context should work: {:?}",
        result.err()
    );
}

#[test]
fn repro_spliton_no_user_library() {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, false); // no Library
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code = r#"pure (T.splitOn "," "a,b,c")"#;
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, "", "", None, Some(4096));

    let pp = prelude_dir();
    let include = [pp];
    let result = tidepool_runtime::compile_and_run_pure(&source, "result", &include);
    match &result {
        Ok(val) => eprintln!("OK: {:?}", val.to_json()),
        Err(e) => eprintln!("ERROR: {}", e),
    }
    assert!(
        result.is_ok(),
        "T.splitOn without Library should work: {:?}",
        result.err()
    );
}
