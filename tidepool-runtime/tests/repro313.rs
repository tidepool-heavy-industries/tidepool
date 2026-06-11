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
        // TWO occurrences: drives occ2 down the False/False (deepest) path,
        // the branch the #313 TailCtx leak miscompiled.
        cx.respond("alpha countTable beta countTable gamma\n".to_string())
    }
}

/// Answers FsRead with a body containing the needle exactly once
/// (patchFile's success path); FsWrite gets ().
struct PatchDispatcher;
impl DispatchEffect<()> for PatchDispatcher {
    fn dispatch(
        &mut self,
        _tag: u64,
        request: &Value,
        cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        if let Value::Con(con_id, _) = request {
            if cx.table().get_by_name("FsWrite") == Some(*con_id) {
                return cx.respond(());
            }
        }
        cx.respond("line one\nthe old needle line\nline three\n".to_string())
    }
}

/// Layer0-audit member of the #313 TailCtx class: Patch.patchFile is a
/// cross-module M fn with the double-T.breakOn occurrence check inlined;
/// pre-fix it died on the success/ambiguous paths with "apply_cont_heap:
/// result con_tag <garbage> is neither Val nor E".
#[test]
fn repro_313_patch_class() {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let nonce = std::env::var("NONCE").unwrap_or_default();
    let code = format!("-- nonce {nonce}\npatchFile \"f.txt\" \"old needle\" \"new needle\"");
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
    let mut d = PatchDispatcher;
    let r = compile_and_run(&src, "result", &include, &mut d, &());
    match r {
        Ok(v) => assert_eq!(
            v.to_json(),
            serde_json::json!("patched f.txt"),
            "#313 Patch-class regression: patchFile success path must complete"
        ),
        Err(e) => panic!("#313 Patch-class regression: eval failed: {e}"),
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
    // FORCE=1: force the Int result inside the user continuation (`x + 1`
    // unboxes); if occ2 miscompiled to return Text, the trap moves from the
    // downstream toJSON render into the user k — localizing the bad value.
    let force = std::env::var("FORCE").is_ok();
    let code = if force {
        format!("x <- t11\n-- nonce {nonce}\npure (x + 1)")
    } else {
        format!("x <- t11\n-- nonce {nonce}\npure x")
    };
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
    // Regression gate (#313 t11): two occurrences → 2 (FORCE=1 → 3). The
    // TailCtx leak returned the breakOn remainder Text instead, trapping
    // downstream in the render path.
    let expected = if force { 3 } else { 2 };
    match r {
        Ok(v) => {
            println!("RESULT OK: {:?}", v.to_json());
            assert_eq!(
                v.to_json(),
                serde_json::json!(expected),
                "#313 t11 regression: occ2 must count {expected} occurrences"
            );
        }
        Err(e) => panic!("#313 t11 regression: eval failed: {e}"),
    }
}
