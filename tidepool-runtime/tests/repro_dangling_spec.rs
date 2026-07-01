//! Repro: dangling NVar from $sunion (Set.union specialization at Text).
//!
//! When Map.fromListWith fires a combine on a key collision, GHC emits a
//! module-local specialization binding `$sunion` (Set.union @ Text) that is
//! referenced in the Core output but was never included in the translation
//! closure, causing `unresolved variable VarId(0xfe...)` at JIT runtime.
//!
//! Distinct keys never trigger the combine closure so they pass even without
//! the fix — giving us a clean control probe.

use std::path::Path;
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

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

fn run_dangling_probe(code: &str) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(
        &pre,
        &stack,
        &tidepool_mcp::wrap_do(code),
        "",
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
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(v) => Ok(v.to_json()),
        Err(e) => Err(format!("{e}")),
    }
}

/// Key collision — the $sunion combine closure fires.
///
/// Pre-fix: dies with `unresolved variable VarId(0xfe...)` because the
/// $sunion specialization binding is referenced but not emitted.
/// Post-fix: returns 1 (the two "b" entries merged into one Set).
#[test]
fn repro_dangling_spec_collision() {
    let code = concat!(
        r#"pure (length (Map.toList (Map.fromListWith Set.union"#,
        r#" (L.map (\(a,b) -> (a, Set.singleton b))"#,
        r#" [("b"::Text,"a"::Text),("b","c")]))))"#,
    );
    match run_dangling_probe(code) {
        Ok(v) => assert_eq!(
            v,
            serde_json::json!(1),
            "dangling $sunion: key collision must yield map length 1 (two 'b' keys merged)"
        ),
        Err(e) => panic!("dangling $sunion regression: eval failed: {e}"),
    }
}

/// Key collision, merged content — the combine must actually union the sets,
/// not just produce a map of the right size.
#[test]
fn repro_dangling_spec_merged_content() {
    let code = concat!(
        r#"pure (Set.toAscList (Map.findWithDefault Set.empty ("b"::Text)"#,
        r#" (Map.fromListWith Set.union"#,
        r#" (L.map (\(a,b) -> (a, Set.singleton b))"#,
        r#" [("b"::Text,"a"::Text),("b","c")]))))"#,
    );
    match run_dangling_probe(code) {
        Ok(v) => assert_eq!(
            v,
            serde_json::json!(["a", "c"]),
            "dangling $sunion: merged set for key 'b' must be the union of both values"
        ),
        Err(e) => panic!("dangling $sunion merged-content: eval failed: {e}"),
    }
}

/// Distinct keys — combine never fires; control probe for the expression shape.
///
/// This must pass both before and after the fix: it proves the module compiles
/// and the basic Map/Set shape works, without triggering the $sunion reference.
#[test]
fn repro_dangling_spec_distinct_control() {
    let code = concat!(
        r#"pure (length (Map.toList (Map.fromListWith Set.union"#,
        r#" (L.map (\(a,b) -> (a, Set.singleton b))"#,
        r#" [("a"::Text,"b"::Text),("c","b")]))))"#,
    );
    match run_dangling_probe(code) {
        Ok(v) => assert_eq!(
            v,
            serde_json::json!(2),
            "dangling $sunion control: distinct keys must yield map length 2"
        ),
        Err(e) => panic!("dangling $sunion control: eval failed: {e}"),
    }
}
