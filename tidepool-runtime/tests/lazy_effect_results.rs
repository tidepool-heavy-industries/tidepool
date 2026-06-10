//! Lazy effect-result materialization: large list-shaped handler responses
//! are parked host-side and materialized chunk-by-chunk through host-code
//! tail thunks, instead of eagerly converting (and previously, dying on the
//! response node cap). See plans/lazy-effect-results.md.

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

/// Responds to EVERY effect with a large list of strings — stands in for a
/// glob/grep handler returning tens of thousands of paths.
struct BigListDispatcher {
    n: usize,
}

impl DispatchEffect<()> for BigListDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        let items: Vec<String> = (0..self.n).map(|i| format!("item-{i}")).collect();
        cx.respond(items)
    }
}

fn run_with_big_list(code: &str, n: usize) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, "", "", None, None);

    std::env::set_var("TIDEPOOL_LAZY_RESULTS", "1");
    let include = [prelude_dir(), user_lib_dir()];
    let mut dispatcher = BigListDispatcher { n };
    compile_and_run(&source, "result", &include, &mut dispatcher, &())
        .map(|v| v.to_json())
        .map_err(|e| format!("{e}"))
}

#[test]
fn length_of_huge_response_streams() {
    // 12k elements (~36k value nodes): far over the old 10k hard cap.
    // length folds the lazy chunks; consumed cells become garbage.
    let r = run_with_big_list("xs <- glob \"**\"\npure (length xs)", 12_000);
    assert_eq!(r.ok(), Some(serde_json::json!(12_000)));
}

#[test]
fn take_prefix_of_huge_response() {
    // take only forces the first chunk; the rest is never materialized.
    let r = run_with_big_list("xs <- glob \"**\"\npure (take 3 xs)", 12_000);
    assert_eq!(
        r.ok(),
        Some(serde_json::json!(["item-0", "item-1", "item-2"]))
    );
}

#[test]
fn small_responses_stay_eager() {
    // Below the lazy threshold nothing changes.
    let r = run_with_big_list("xs <- glob \"**\"\npure (length xs)", 50);
    assert_eq!(r.ok(), Some(serde_json::json!(50)));
}

#[test]
fn filtered_fold_over_huge_response() {
    // A realistic shape: census-style filter+length over a huge listing,
    // exercising chunk boundaries mid-stream.
    let r = run_with_big_list(
        "xs <- glob \"**\"\npure (length (filter (\\x -> \"item-1\" `isPrefixOf` x) xs))",
        30_000,
    );
    // decimal-starts-with-1 counts in 0..30000: 1+10+100+1000+10000
    assert_eq!(r.ok(), Some(serde_json::json!(11_111)));
}

#[test]
fn take_then_length_bisect() {
    let r = run_with_big_list("xs <- glob \"**\"\npure (length (take 3 xs))", 12_000);
    assert_eq!(r.ok(), Some(serde_json::json!(3)));
}

#[test]
fn whole_lazy_list_result_is_paginated() {
    // Under the MCP template, `pure xs` of a 12k lazy list flows through
    // toJSON + paginateResult: the JIT forces every chunk in-Haskell and the
    // paginator truncates the rendered array to the character budget. The
    // exact element count is budget-dependent; what matters is that the
    // full pipeline survives (no silent thread death) and yields a
    // truncated-but-well-formed prefix. The RAW bridge path (no paginator)
    // is covered by lazy_bisect::variant_d_whole_list_result.
    let r = run_with_big_list("xs <- glob \"**\"\npure xs", 12_000);
    let arr = r.expect("whole-list result must succeed");
    let arr = arr.as_array().expect("expected JSON array");
    assert!(
        arr.len() > 100,
        "expected a substantial paginated prefix, got {}",
        arr.len()
    );
    assert_eq!(arr[0], serde_json::json!("item-0"));
    assert_eq!(arr[1], serde_json::json!("item-1"));
}
