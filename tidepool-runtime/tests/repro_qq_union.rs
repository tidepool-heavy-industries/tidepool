//! P1 repro: a QQ-using eval must keep freer-simple's `Union` (and the other
//! effect-wiring constructors) in the DataConTable.
//!
//! Importing the `[fmt|]` quoter pulls in `Tidepool.QQ.HsMeta.*`, which import
//! the (now session-exposed) `ghc` package. Those compile-time-only TH binders
//! enter the translated bind set, and the meta walks (`collectUsedDataCons` /
//! `collectTransitiveDCons`) harvested the ghc package's entire constructor
//! universe into the table. A 64-bit varId collision then displaced
//! freer-simple's `Union`, so `ConTags::try_from` failed at machine setup with
//! `missing freer-simple constructor 'Union'` — breaking EVERY QQ eval (even
//! pure ones, since Union is resolved unconditionally at setup).
//!
//! This guards the whole class: QQ × effect dispatch × table integrity.
use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

/// Answers every effect with a JSON `Number` (the same bridge path the live
/// server uses), so the eval dispatches a real effect and round-trips a number.
struct NumberDispatcher;
impl DispatchEffect<()> for NumberDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        cx.respond(serde_json::json!(42.0))
    }
}

/// An effectful eval that interpolates a render-coerced hole via `[fmt|]`.
/// Pre-fix this never reaches execution — `ConTags::try_from` fails at setup
/// because `Union` is missing from the table. Post-fix it yields "got 42".
#[test]
fn repro_qq_union() {
    // Generous stack: pre-fix the table is flooded with the ghc package's
    // constructor universe (~275KB meta), and deserializing/processing it can
    // exhaust the default 2MB test stack before the clean Union error surfaces.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let nonce = std::env::var("NONCE").unwrap_or_default();
    let code = format!(
        "v <- httpGet \"x\"\n-- nonce {nonce}\npure [fmt|got {{maybe 0 round (v ^? _Number)}}|]"
    );
    // Mirror the MCP eval handler: a quoter token in the code injects the
    // Tidepool.QQ import (template_haskell prepends `import ` per line).
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        &tidepool_mcp::wrap_do(&code),
        "Tidepool.QQ (fmt, j)",
        "",
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
    let mut d = NumberDispatcher;
    let r = compile_and_run(&src, "result", &include, &mut d, &());
    match r {
        Ok(v) => assert_eq!(
            v.to_json(),
            serde_json::json!("got 42"),
            "QQ-union regression: a [fmt|] eval over an effect result must render"
        ),
        Err(e) => panic!("QQ-union regression: eval failed (missing Union in DataConTable?): {e}"),
    }
}
