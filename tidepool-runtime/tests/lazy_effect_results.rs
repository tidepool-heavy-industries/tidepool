//! Lazy effect-result materialization: large list-shaped handler responses
//! are parked host-side and materialized chunk-by-chunk through host-code
//! tail thunks, instead of eagerly converting (and previously, dying on the
//! response node cap).

use std::path::PathBuf;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_testing::eval_harness::EvalHarness;

fn user_lib_dir() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".tidepool/lib")
}

/// Responds to EVERY effect with a large list of strings — stands in for a
/// handler returning tens of thousands of items (the eval calls `kvKeys`, an
/// untagged `M [Text]` verb; the dispatcher ignores the tag).
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
    let source = tidepool_mcp::template_haskell(
        &preamble,
        &stack,
        &tidepool_mcp::wrap_do(code),
        "",
        "",
        None,
        None,
    );

    std::env::set_var("TIDEPOOL_LAZY_RESULTS", "1");
    let dispatcher = BigListDispatcher { n };
    EvalHarness::new()
        .with_stdlib()
        .with_include(user_lib_dir())
        .with_effects_module()
        .run(&source, "result", dispatcher)
        .into_result()
        .map(|v| v.to_json())
        .map_err(|e| format!("{e}"))
}

/// Responds to every effect with a `Right(big list)` — the shape #335's
/// errors-tagged list verbs (`glob`/`grep`/`listDir`) produce (`Either <Err>
/// [T]`, delivered eagerly because a `Left` must be decided at the boundary and
/// `Right(stream)` is inexpressible). `probe_list_spine` does NOT recognize the
/// 1-field `Right` Con as a cons spine, so this takes the eager `value_to_heap`
/// path (a stack-safe hylomorphism) plus the 100k-node cap — NOT the lazy park.
struct EitherBigListDispatcher {
    n: usize,
}

impl DispatchEffect<()> for EitherBigListDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        let items: Vec<String> = (0..self.n).map(|i| format!("item-{i}")).collect();
        cx.respond(Ok::<Vec<String>, String>(items))
    }
}

fn run_with_either_big_list(code: &str, n: usize) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let source = tidepool_mcp::template_haskell(
        &preamble,
        &stack,
        &tidepool_mcp::wrap_do(code),
        "",
        "",
        None,
        None,
    );

    std::env::set_var("TIDEPOOL_LAZY_RESULTS", "1");
    let dispatcher = EitherBigListDispatcher { n };
    EvalHarness::new()
        .with_stdlib()
        .with_include(user_lib_dir())
        .with_effects_module()
        .run(&source, "result", dispatcher)
        .into_result()
        .map(|v| v.to_json())
        .map_err(|e| format!("{e}"))
}

/// #335 stack-safety invariant: an errors-tagged list verb returns
/// `Right(big list)` EAGERLY (no lazy park — `probe_list_spine` doesn't peek
/// inside the `Right`), so a >2000-element result must still complete without a
/// stack overflow. Safety here comes NOT from the park guard but from
/// `value_to_heap` being a stack-safe hylomorphism + `Value`'s iterative
/// `Drop` + the 100k-node `Eager` cap — all of which apply to the
/// `Right`-wrapped shape. 12k elements (~48k nodes) is well over the 2k park
/// threshold and under the 100k cap.
#[test]
fn errors_tagged_right_wrapped_big_list_is_stack_safe() {
    let r = run_with_either_big_list("Right xs <- glob \"**\"\npure (length xs)", 12_000);
    assert_eq!(r.clone().ok(), Some(serde_json::json!(12_000)), "{r:?}");
}

#[test]
fn length_of_huge_response_streams() {
    // 12k elements (~36k value nodes): far over the old 10k hard cap.
    // length folds the lazy chunks; consumed cells become garbage.
    let r = run_with_big_list("xs <- kvKeys\npure (length xs)", 12_000);
    assert_eq!(r.ok(), Some(serde_json::json!(12_000)));
}

#[test]
fn take_prefix_of_huge_response() {
    // take only forces the first chunk; the rest is never materialized.
    let r = run_with_big_list("xs <- kvKeys\npure (take 3 xs)", 12_000);
    assert_eq!(
        r.ok(),
        Some(serde_json::json!(["item-0", "item-1", "item-2"]))
    );
}

#[test]
fn small_responses_stay_eager() {
    // Below the lazy threshold nothing changes.
    let r = run_with_big_list("xs <- kvKeys\npure (length xs)", 50);
    assert_eq!(r.ok(), Some(serde_json::json!(50)));
}

#[test]
fn filtered_fold_over_huge_response() {
    // A realistic shape: census-style filter+length over a huge listing,
    // exercising chunk boundaries mid-stream.
    let r = run_with_big_list(
        "xs <- kvKeys\npure (length (filter (\\x -> \"item-1\" `isPrefixOf` x) xs))",
        30_000,
    );
    // decimal-starts-with-1 counts in 0..30000: 1+10+100+1000+10000
    assert_eq!(r.ok(), Some(serde_json::json!(11_111)));
}

#[test]
fn take_then_length_bisect() {
    let r = run_with_big_list("xs <- kvKeys\npure (length (take 3 xs))", 12_000);
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
    let r = run_with_big_list("xs <- kvKeys\npure xs", 12_000);
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
