//! #313 repro outside the MCP server: stderr (CASE TRAP / [BUG] bad
//! pointer breadcrumbs) is visible here.
use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

struct TupleDispatcher;
impl DispatchEffect<()> for TupleDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        _request: &Value,
        cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        cx.respond("alpha countTable beta\n".to_string())
    }
}

#[test]
fn repro_313() {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    // NONCE busts the cache so each run is a FRESH compile — probing
    // whether the miscompile is deterministic for identical source.
    let nonce = std::env::var("NONCE").unwrap_or_default();
    let code = format!("x <- t11\n-- nonce {nonce}\npure x");
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        &tidepool_mcp::wrap_do(&code),
        "Probe",
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
    let mut d = TupleDispatcher;
    let r = compile_and_run(&src, "result", &include, &mut d, &());
    match r {
        Ok(v) => println!("RESULT OK: {:?}", v.to_json()),
        Err(e) => println!("RESULT ERR: {e}"),
    }
}
