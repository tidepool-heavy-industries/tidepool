//! [fmt|...|] format-spec rejection: `:e`/`:E`/`:g`/`:G` (exponential/general)
//! are rejected at COMPILE time, because rendering them needs `floatToDigits`
//! (the `clz#` primop the JIT cannot run). The quoter `fail`s with a message
//! naming the spec and pointing at `:f`; that surfaces as a GHC splice error,
//! so `compile_and_run` returns `Err` whose text contains the message.
//!
//! This is the aeson-style "drop the un-strippable leaf, name it loudly"
//! contract: a free, precise compile error instead of a runtime trap.
use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_testing::eval_harness::EvalHarness;

/// Never actually invoked — compilation fails first.
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

fn try_compile(hole: &str) -> Result<(), String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let nonce = std::env::var("NONCE").unwrap_or_default();
    let code = format!("-- nonce {nonce}\npure {hole}");
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        &tidepool_mcp::wrap_do(&code),
        "Tidepool.QQ (fmt, j)",
        "",
        None,
        None,
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let lib = root.join(".tidepool/lib");
    match EvalHarness::new()
        .with_stdlib()
        .with_include(lib)
        .with_effects_module()
        .run(&src, "result", NullDispatcher)
        .into_result()
    {
        Ok(_) => Ok(()),
        // `Display` on `RuntimeError`/`CompileError::Diagnostics` is a terse
        // structural summary now (structured spans, not rendered text) — the
        // classifier's message is the joined diagnostic text callers actually
        // want to assert on.
        Err(e) => Err(tidepool_runtime::classify(&e).message),
    }
}

#[test]
fn fmt_spec_rejects_exponential() {
    run();
}

fn run() {
    // `:e` must be rejected at compile time, naming the spec and suggesting :f.
    let err = match try_compile("[fmt|{(1.5 :: Double):e}|]") {
        Ok(()) => panic!("expected ':e' to be rejected at compile time, but it compiled"),
        Err(e) => e,
    };
    assert!(
        err.contains(":e") && err.contains(":f"),
        "rejection message should name ':e' and point at ':f'; got:\n{err}"
    );
    assert!(
        err.to_lowercase().contains("not supported")
            || err.to_lowercase().contains("floatdigits")
            || err.to_lowercase().contains("floattodigits"),
        "rejection should explain why ':e' is unsupported; got:\n{err}"
    );

    // The matched valid spec `:f` (fixed-point) must still compile and run.
    try_compile("[fmt|{(1.5 :: Double):.2f}|]")
        .expect("':.2f' (fixed-point) should compile and run");
}
