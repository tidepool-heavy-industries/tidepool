//! Regression coverage from the lazy-results hang bisect (2026-06-10).
//!
//! Root cause (found via gdb watchdog-abort backtrace): a 12k-element
//! response spine's recursive `Value` Drop at the end of the jit_machine
//! effect arm overflowed the eval thread's stack → SIGSEGV outside signal
//! protection → silent thread exit → caller hang. These variants reconstruct
//! the MCP module manually — same preamble, same effect stack — varying the
//! result body across the consumption shapes that triggered it:
//!   A: discard xs entirely        → response dropped unconsumed
//!   B: toJSON (take 3 xs)         → partial consumption, no paginateResult
//!   C: paginateResult (template)  → the full MCP template shape

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

/// Assemble the MCP module manually: real preamble + real stack, custom
/// result body (no template_haskell so paginateResult is opt-in per variant).
fn run_variant(result_body: &str, n: usize) -> Result<serde_json::Value, String> {
    run_variant_typed(result_body, "Value", n)
}

fn run_variant_typed(
    result_body: &str,
    result_ty: &str,
    n: usize,
) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let mut source = preamble;
    source.push_str("-- [user]\n");
    source.push_str(&format!("result :: Eff {} {}\n", stack, result_ty));
    source.push_str("result = do\n");
    for line in result_body.lines() {
        source.push_str(&format!("  {}\n", line));
    }

    std::env::set_var("TIDEPOOL_LAZY_RESULTS", "1");
    let include = [prelude_dir(), user_lib_dir()];
    let mut dispatcher = BigListDispatcher { n };
    compile_and_run(&source, "result", &include, &mut dispatcher, &())
        .map(|v| v.to_json())
        .map_err(|e| format!("{e}"))
}

#[test]
fn variant_a_discard() {
    // Lazy response delivered, never consumed past the entry force.
    let r = run_variant("xs <- glob \"**\"\nlet _ = xs\npure Null", 12_000);
    assert_eq!(r.ok(), Some(serde_json::json!(null)));
}

#[test]
fn variant_b_tojson_take() {
    // take + toJSON, but NO paginateResult.
    let r = run_variant("xs <- glob \"**\"\npure (toJSON (take 3 xs))", 12_000);
    assert_eq!(
        r.ok(),
        Some(serde_json::json!(["item-0", "item-1", "item-2"]))
    );
}

#[test]
fn variant_d_whole_list_result() {
    // `pure xs` WITHOUT paginateResult: the entire 12k lazy list ships
    // through the result bridge as a raw cons spine. heap_to_value drains
    // every chunk (forcing tails mid-walk → GC under the host walker),
    // builds a 12k-deep Value, renders it (renderer truncates at 10k +
    // "..."), and drops it — exercising the rooted spine walk, the
    // iterative spine conversion, and Value's iterative Drop on a default
    // test thread. Historically each of those was a recursive stack
    // overflow → silent thread death.
    let r = run_variant_typed("xs <- glob \"**\"\npure xs", "[Text]", 12_000);
    let arr = r.expect("whole-list result must succeed");
    let arr = arr.as_array().expect("expected JSON array");
    assert_eq!(arr.len(), 10_001);
    assert_eq!(arr[0], serde_json::json!("item-0"));
    assert_eq!(arr[9_999], serde_json::json!("item-9999"));
    assert_eq!(arr[10_000], serde_json::json!("..."));
}

#[test]
fn variant_c_paginate() {
    // The full MCP template shape (the originally-reported hang).
    let r = run_variant(
        "xs <- glob \"**\"\nlet _r = take 3 xs\npaginateResult 4096 (toJSON _r)",
        12_000,
    );
    assert_eq!(
        r.ok(),
        Some(serde_json::json!(["item-0", "item-1", "item-2"]))
    );
}
