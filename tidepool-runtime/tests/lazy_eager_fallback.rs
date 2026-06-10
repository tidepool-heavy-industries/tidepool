//! Eager (opt-out) coverage for long list-shaped effect responses.
//!
//! With TIDEPOOL_LAZY_RESULTS=0 (lazy is default-on), long spines are still
//! flattened by value and materialized ITERATIVELY
//! (host_fns::materialize_cons_list) — never recursively converted or
//! recursively dropped. Pre-fix, a 12k response either died on the old 10k
//! node cap or, post-cap-raise, silently killed the eval thread in
//! `Value`'s recursive destructor.
//!
//! Own file = own process: the lazy tests set the env var process-globally.

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
    ) -> Result<Value, tidepool_effect::error::EffectError> {
        let items: Vec<String> = (0..self.n).map(|i| format!("item-{i}")).collect();
        cx.respond(items)
    }
}

fn run_eager(code: &str, n: usize) -> Result<serde_json::Value, String> {
    std::env::set_var("TIDEPOOL_LAZY_RESULTS", "0");
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, "", "", None, None);

    let include = [prelude_dir(), user_lib_dir()];
    let mut dispatcher = BigListDispatcher { n };
    compile_and_run(&source, "result", &include, &mut dispatcher, &())
        .map(|v| v.to_json())
        .map_err(|e| format!("{e}"))
}

#[test]
fn eager_big_list_materializes_iteratively() {
    // 12k elements (~36k nodes): over the OLD 10k cap, under the new 100k.
    // Must fully materialize without lazy gating — and without the silent
    // eval-thread death the recursive paths caused.
    let r = run_eager("xs <- glob \"**\"\npure (length xs)", 12_000);
    assert_eq!(r.ok(), Some(serde_json::json!(12_000)));
}

#[test]
fn eager_oversize_list_errors_cleanly() {
    // ~5x the node cap: must surface EffectResponseTooLarge as a clean
    // error — historically the error path itself could die in the deep
    // drop of the rejected response.
    let r = run_eager("xs <- glob \"**\"\npure (length xs)", 200_000);
    let err = r.expect_err("oversize eager response must error");
    assert!(
        err.contains("too large") || err.contains("TooLarge") || err.contains("100000"),
        "expected response-size error, got: {err}"
    );
}
