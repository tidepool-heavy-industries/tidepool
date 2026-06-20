//! Regression: `[fmt|{x:.Nf}|]` / `:%` on NON-FINITE Doubles must render
//! Python's spellings (inf / -inf / nan), not the `round`-primop's saturated
//! garbage. Live before the fix, `{1.0/0.0:.2f}` printed
//! "92233720368547758.07" (round of +Inf saturates the Int) and `{0.0/0.0:.2f}`
//! printed "0.00" (round of NaN is 0) — a silent wrong-output bug. The guard
//! lives in `Tidepool.QQ.Fmt.Runtime.fmtFrac` (finiteness tested without
//! RealFloat: `d == d` is False only for NaN; `|d| > maxFiniteDouble` only for
//! +/-Inf).
use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

/// Never invoked — these holes are pure.
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

fn eval_hole(hole: &str) -> serde_json::Value {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let nonce = std::env::var("NONCE").unwrap_or_default();
    let code = format!("-- nonce {nonce}\npure ({hole} :: Text)");
    // Pin the non-finite values to Double at top level — inline `1.0/0.0` in a
    // hole defaults to a GMP-pulling type, unrelated to what's under test.
    let helpers = "infD, ninfD, nanD :: Double\n\
                   infD = 1.0 / 0.0\n\
                   ninfD = (0.0 - 1.0) / 0.0\n\
                   nanD = 0.0 / 0.0\n";
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        &tidepool_mcp::wrap_do(&code),
        "Tidepool.QQ (fmt)",
        helpers,
        None,
        None,
    );
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls)
        .expect("write effects module")
        .leak() as &Path;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib").leak() as &Path;
    let lib = root.join(".tidepool/lib").leak() as &Path;
    let include = [hs, lib, effects_dir];
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(v) => v.to_json(),
        Err(e) => panic!("eval failed for `{hole}`: {e}"),
    }
}

fn run() {
    use serde_json::json;
    // Non-finite: Python spellings, percent suffix + width/align preserved.
    assert_eq!(eval_hole("[fmt|{infD:.2f}|]"), json!("inf"));
    assert_eq!(eval_hole("[fmt|{ninfD:.2f}|]"), json!("-inf"));
    assert_eq!(eval_hole("[fmt|{nanD:.2f}|]"), json!("nan"));
    assert_eq!(eval_hole("[fmt|{infD:.0%}|]"), json!("inf%"));
    assert_eq!(eval_hole("[fmt|{infD:>6.2f}|]"), json!("   inf"));
    // Finite path unchanged.
    assert_eq!(eval_hole("[fmt|{3.14159:.2f}|]"), json!("3.14"));
    assert_eq!(eval_hole("[fmt|{0.0:.2f}|]"), json!("0.00"));
}

#[test]
fn fmt_nonfinite_renders_python_spellings() {
    // fmt-spec evals nest deep through -O2-inlined Core; needs > 2 MiB stack.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}
