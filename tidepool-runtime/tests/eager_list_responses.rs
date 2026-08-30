//! Regression coverage for list-shaped effect responses, which are EAGER:
//! every element converts at dispatch time and the heap spine is built
//! ITERATIVELY (`host_fns::materialize_cons_list`) — never recursively
//! converted or recursively dropped. Pre-history, a 12k response either
//! died on the old 10k node cap or silently killed the eval thread in
//! `Value`'s recursive destructor; these tests are what stands between the
//! eager path and that class of failure.

use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_testing::eval_harness::user_lib_dir;
use tidepool_testing::eval_harness::EvalHarness;

struct BigListDispatcher {
    n: usize,
}

impl DispatchEffect<()> for BigListDispatcher {
    fn dispatch(
        &mut self,
        _request: &Value,
        cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<Option<tidepool_effect::Response>, tidepool_effect::error::EffectError> {
        let items: Vec<String> = (0..self.n).map(|i| format!("item-{i}")).collect();
        cx.respond_list(items).map(Some)
    }
}

fn run_list(code: &str, n: usize) -> Result<serde_json::Value, String> {
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

#[test]
fn big_list_materializes_iteratively() {
    // 12k elements (~36k nodes): over the OLD 10k cap, under the 100k one.
    // Must fully materialize — without the silent eval-thread death the
    // recursive paths caused.
    let r = run_list("xs <- kvKeysP \"\"\npure (length xs)", 12_000);
    assert_eq!(r.ok(), Some(serde_json::json!(12_000)));
}

#[test]
fn empty_list_is_nil() {
    let r = run_list("xs <- kvKeysP \"\"\npure (length xs)", 0);
    assert_eq!(r.ok(), Some(serde_json::json!(0)));
}

#[test]
fn list_elements_round_trip() {
    let r = run_list("xs <- kvKeysP \"\"\npure (take 3 xs)", 50);
    assert_eq!(
        r.ok(),
        Some(serde_json::json!(["item-0", "item-1", "item-2"]))
    );
}

#[test]
fn oversize_list_errors_cleanly() {
    // ~5x the node cap: must surface EffectResponseTooLarge as a clean
    // error — historically the error path itself could die in the deep
    // drop of the rejected response.
    let r = run_list("xs <- kvKeysP \"\"\npure (length xs)", 200_000);
    let err = r.expect_err("oversize response must error");
    assert!(
        err.contains("too large") || err.contains("TooLarge") || err.contains("100000"),
        "expected response-size error, got: {err}"
    );
}
