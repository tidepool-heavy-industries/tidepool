//! Success-path GHC warnings must survive a clean compile and appear in the
//! rendered eval result instead of being silently dropped (previously
//! `compile_and_run` only checked `warnings.has_io` and discarded the rest —
//! see `plans/diagnostics-flow-recon.md` patch point (a)).
//!
//! Mirrors the harness in `text_breakon_replace_mcp.rs` (real `compile_and_run`
//! with the standard MCP preamble/effect stack + `.tidepool/lib`/stdlib include
//! path), but inspects `EvalResult::warnings()`/`to_string_pretty()` instead of
//! just the JSON value.

use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::{compile_and_run, EvalResult};

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
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        Err(tidepool_effect::error::EffectError::UnhandledEffect { tag })
    }
}

/// Compile `code` (with optional top-level `helpers`) through the real
/// pipeline with the standard MCP preamble/effect stack, returning the raw
/// `EvalResult` (the eval is expected to SUCCEED).
fn run_mcp(code: &str, helpers: &str) -> EvalResult {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, "", helpers, None, None);

    let pp = prelude_dir();
    let ulp = user_lib_dir();
    assert!(
        ulp.join("Library.hs").exists(),
        ".tidepool/lib/Library.hs not found"
    );
    let eff = tidepool_mcp::ensure_effects_module(&decls)
        .expect("write effects module")
        .leak() as &Path;
    let include = [pp, ulp, eff];

    let mut dispatcher = MockDispatcher;
    compile_and_run(&source, "result", &include, &mut dispatcher, &())
        .expect("compile_and_run failed")
}

/// Two overlapping clauses for `f` trigger GHC's (on-by-default)
/// `-Woverlapping-patterns` diagnostic. The eval still succeeds (`f 0`
/// resolves to the first clause), so this is exactly the success-path case
/// that used to vanish silently.
#[test]
fn overlapping_pattern_warning_surfaces_in_result() {
    let result = run_mcp("pure (f 0 :: Int)", "f :: Int -> Int\nf x = 1\nf x = 2\n");

    assert!(
        !result.warnings().is_empty(),
        "expected a captured GHC warning for the overlapping `f` clauses"
    );
    assert!(
        result
            .warnings()
            .iter()
            .any(|w| w.to_lowercase().contains("overlapping")),
        "expected an overlapping-patterns warning, got: {:?}",
        result.warnings()
    );

    let rendered = result.to_string_pretty();
    assert!(
        rendered.contains("## Warnings"),
        "rendered result should carry a Warnings section, got: {rendered}"
    );
    assert!(rendered.to_lowercase().contains("overlapping"));
    // The value itself still renders first, unaffected by the warning.
    assert!(rendered.starts_with('1'));
}

/// A clean compile stays byte-identical to the pre-warnings rendering — no
/// `## Warnings` noise on the happy path.
#[test]
fn clean_compile_has_no_warnings_section() {
    let result = run_mcp("pure (1 + 1 :: Int)", "");

    assert!(
        result.warnings().is_empty(),
        "expected no warnings for a clean compile, got: {:?}",
        result.warnings()
    );
    let rendered = result.to_string_pretty();
    assert!(!rendered.contains("Warnings"), "got: {rendered}");
    assert_eq!(rendered, "2");
}
