//! Regression repro for the `Data.List.NonEmpty.group`/`groupBy` CASE TRAP.
//!
//! `pure (toJSON (map NE.toList (NE.group [1,1,2,3,3,3::Int])))` with
//! `import qualified Data.List.NonEmpty as NE` should yield
//! `[[1,1],[2],[3,3,3]]` but instead crashes with
//! `signal-crash: case trap: scrutinee constructor not among case
//! alternatives` (heap tag 0 — an unforced partial application reaching a
//! list `case`).
//!
//! ROOT CAUSE (confirmed 2026-06-21, branch `fix/ne-group-case-trap`):
//!   `NE.group`/`NE.groupBy [Int]` is rewritten by GHC's SPEC rule
//!   `"SPEC groupBy @[]"` into a reference to base's specialized binding
//!   `groupBy_$sgroupBy :: (a->a->Bool) -> [a] -> [NonEmpty a]` (arity 2).
//!   base ships this binding with NO unfolding and NO fat interface
//!   (`Data.List.NonEmpty: no mi_extra_decls`), so Tidepool's
//!   `Tidepool.Resolve.attemptSpecFallback` despecializes the OccName to
//!   `groupBy` and binds `groupBy_$sgroupBy = groupBy` — a BARE ALIAS to the
//!   *generic* `groupBy :: Foldable f => (a->a->Bool) -> f a -> [NonEmpty a]`
//!   (arity 3). The generic takes the `Foldable` DICTIONARY as its first value
//!   arg, which the specialization eliminated. Aliasing bare DROPS that arg, so
//!   every call `groupBy_$sgroupBy eq xs` (2 args) under-saturates the arity-3
//!   generic and yields a PARTIAL APPLICATION. That PAP flows where the result
//!   list `[NonEmpty a]` is expected; the enclosing list `case` reads the
//!   closure's heap tag (0) → no match → CASE TRAP.
//!
//!   This is a CLASS of bugs: any boot-library SPECIALISE binding that erases a
//!   dictionary value arg, ships without an unfolding/fat-iface, and is reached
//!   via its SPEC rule will hit it (NE.group/groupBy/groupBy1/groupWith, ...).
//!
//! FIX (landed, branch `fix/ne-group-dict`): in `attemptSpecFallback`
//!   (`haskell/src/Tidepool/Resolve.hs`), when the generic parent's value
//!   arity exceeds the specialization's (a dictionary was specialized away),
//!   do NOT emit the bare alias. Instead `reconstructSpecAlias` builds an
//!   eta-expanded wrapper `\@spTvs v1..vn -> genId @Tys dict1..dictk v1..vn`:
//!   the type args come from unifying the generic's result type with the
//!   specialization's (`tcMatchTy`), and each erased class constraint's
//!   concrete dictionary is resolved via `lookupInstEnv` on the EPS global
//!   instance env (for `NE.group`: `Foldable []` → unique `$fFoldableList`).
//!   The wrapper's new free var (the dfun) is fed back into the resolver
//!   worklist so it resolves too. Gated strictly on the arity mismatch, so
//!   arity-matched aliases (the common path) are untouched.

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

fn eval_with_imports(code: &str, imports: &str, helpers: &str) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let pre = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let src = tidepool_mcp::template_haskell(&pre, &stack, code, imports, helpers, None, None);
    let effects_dir = tidepool_mcp::ensure_effects_module(&decls).expect("write effects module");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let hs = root.join("haskell/lib");
    let lib = root.join(".tidepool/lib");
    let include = [hs.as_path(), lib.as_path(), effects_dir.as_path()];
    let mut d = NullDispatcher;
    match compile_and_run(&src, "result", &include, &mut d, &()) {
        Ok(v) => Ok(v.to_json()),
        Err(e) => Err(format!("{e}")),
    }
}

fn run(code: &str, imports: &str, helpers: &str) -> Result<serde_json::Value, String> {
    let code = code.to_string();
    let imports = imports.to_string();
    let helpers = helpers.to_string();
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            tidepool_codegen::signal_safety::install();
            eval_with_imports(&code, &imports, &helpers)
        })
        .unwrap()
        .join()
        .map_err(|_| "thread panicked (HARD crash / uncaught signal)".to_string())?
}

/// `NE.group [1,1,2,3,3,3]` → `[[1,1],[2],[3,3,3]]` (via `NE.toList`).
/// Was a CASE TRAP (dropped `Foldable` dict); fixed by the dict-reconstructing
/// spec-fallback alias — see the module header for the root cause + fix.
#[test]
fn ne_group_round_trips() {
    let got = run(
        "pure (toJSON (map NE.toList (NE.group [1,1,2,3,3,3::Int])))",
        "qualified Data.List.NonEmpty as NE",
        "",
    );
    match got {
        Ok(v) => assert_eq!(v, serde_json::json!([[1, 1], [2], [3, 3, 3]]), "wrong value"),
        Err(e) => panic!("NE.group crashed: {e}"),
    }
}

/// Sibling of the same CLASS: `NE.groupBy` is rewritten to the same
/// `groupBy_$sgroupBy` spec binding, so it exercises the identical
/// dict-reconstruction path.
#[test]
fn ne_group_by_round_trips() {
    let got = run(
        "pure (toJSON (map NE.toList (NE.groupBy (\\a b -> a == b) [1,1,2,3,3,3::Int])))",
        "qualified Data.List.NonEmpty as NE",
        "",
    );
    match got {
        Ok(v) => assert_eq!(v, serde_json::json!([[1, 1], [2], [3, 3, 3]]), "wrong value"),
        Err(e) => panic!("NE.groupBy crashed: {e}"),
    }
}

/// `NE.groupWith` (keyed grouping) over the same input.
#[test]
fn ne_group_with_round_trips() {
    let got = run(
        "pure (toJSON (map NE.toList (NE.groupWith id [1,1,2,3,3,3::Int])))",
        "qualified Data.List.NonEmpty as NE",
        "",
    );
    match got {
        Ok(v) => assert_eq!(v, serde_json::json!([[1, 1], [2], [3, 3, 3]]), "wrong value"),
        Err(e) => panic!("NE.groupWith crashed: {e}"),
    }
}
