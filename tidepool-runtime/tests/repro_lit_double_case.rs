//! P0 repro: a data case on a boxed-literal wrapper constructor (`D#`) applied
//! to a *bare* Lit heap object.
//!
//! Rust-side effect-result materialization builds the vendored-aeson `Number`
//! constructor with field = `Value::Lit(Literal::LitDouble(..))` — a raw heap
//! Lit, NOT a boxed `D#` Con (see `tidepool-bridge/src/json.rs`). When compiled
//! Haskell unwraps such a Double with `round` (GHC's `roundDoubleInt` does
//! `case x of { D# ds -> ... }`), the JIT's `emit_data_dispatch` loads the
//! constructor tag from the heap header, finds no matching alt (the object is a
//! Lit, not a Con), and falls through to the case trap. This brought down every
//! live MCP eval that round-tripped a number through an effect boundary.
//!
//! The dispatcher answers `httpGet` with a JSON `Number`, materialized through
//! the exact `serde_json::Value::to_value` bridge path the MCP server uses.
use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

/// Answers every effect with a JSON `Number`, materialized via the same bridge
/// code (`serde_json::Value::to_value`) the live server uses — producing a
/// `Con(Number, [Lit(LitDouble 42.0)])`.
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

/// Extract the Double out of a Rust-materialized `Number` and unwrap it with
/// `round` (a data case on `D#`). Pre-fix this trapped; post-fix it yields 42.
#[test]
fn repro_lit_double_case() {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let nonce = std::env::var("NONCE").unwrap_or_default();
    let code =
        format!("v <- httpGet \"x\"\n-- nonce {nonce}\npure (maybe (-1) round (v ^? _Number))");
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
    let mut d = NumberDispatcher;
    let r = compile_and_run(&src, "result", &include, &mut d, &());
    match r {
        Ok(v) => assert_eq!(
            v.to_json(),
            serde_json::json!(42),
            "lit-double-case regression: round of a Rust-materialized Number must \
             unwrap the bare Lit Double instead of trapping"
        ),
        Err(e) => panic!("lit-double-case regression: eval failed (case trap?): {e}"),
    }
}
