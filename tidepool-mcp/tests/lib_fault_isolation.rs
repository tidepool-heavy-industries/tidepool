//! Issue #322: a broken `.tidepool/lib` module must not brick every eval.
//!
//! The eval preamble does `import Library`, and `Library` re-exports every
//! sibling verb module — so one module that fails to compile takes down EVERY
//! eval (including the `writeFile` that would repair it) until a host-side edit.
//! `isolate_lib_layer` compile-probes the facade and, on breakage, prepends a
//! sanitized `Library.hs` that re-exports only the modules that still compile.
//!
//! This test drives the REAL eval preamble against a lib dir whose `Bad` module
//! is deliberately broken, and asserts:
//!   1. without isolation, `import Library` fails (the brick), and
//!   2. with isolation, a healthy eval (`pure goodVerb`) still compiles + runs,
//!      and the broken module is named in the brick note.
//!
//! Needs the with-packages GHC + `tidepool-extract` (on PATH or `TIDEPOOL_EXTRACT`).
//! Skips (passes) when the toolchain is absent so a bare `cargo test` on a
//! checkout without it does not fail.

use std::path::{Path, PathBuf};
use tidepool_effect::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_runtime::compile_and_run;

struct MockDispatcher;
impl DispatchEffect<()> for MockDispatcher {
    fn dispatch(
        &mut self,
        tag: u64,
        _request: &Value,
        _cx: &tidepool_effect::EffectContext<'_, ()>,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        Err(tidepool_effect::error::EffectError::UnhandledEffect { tag })
    }
}

/// True if `tidepool-extract` is resolvable (env override or on `PATH`).
fn extract_available() -> bool {
    if std::env::var_os("TIDEPOOL_EXTRACT").is_some() {
        return true;
    }
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("tidepool-extract").exists()))
        .unwrap_or(false)
}

fn prelude_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("haskell/lib")
}

/// A self-contained lib dir: `Library` re-exports a healthy `Good` and a broken
/// `Bad`. Dropped on scope-exit.
struct LibFixture(PathBuf);
impl LibFixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "tidepool-libiso-fixture-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        std::fs::write(
            dir.join("Good.hs"),
            "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}\n\
             module Good where\n\
             import Tidepool.Prelude hiding (error)\n\
             goodVerb :: Int\n\
             goodVerb = 42\n",
        )
        .unwrap();
        // References a name that is not in scope → fails to compile.
        std::fs::write(
            dir.join("Bad.hs"),
            "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}\n\
             module Bad where\n\
             import Tidepool.Prelude hiding (error)\n\
             badVerb :: Int\n\
             badVerb = nonexistentSymbolXyz\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("Library.hs"),
            "module Library ( module Good, module Bad ) where\n\
             import Good\n\
             import Bad\n",
        )
        .unwrap();
        LibFixture(dir)
    }
}
impl Drop for LibFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn eval_good_verb(include: &[&Path]) -> Result<serde_json::Value, String> {
    let decls = tidepool_mcp::standard_decls();
    let preamble = tidepool_mcp::build_preamble(&decls, true);
    let stack = tidepool_mcp::build_effect_stack_type(&decls);
    let code = "pure goodVerb";
    let source = tidepool_mcp::template_haskell(&preamble, &stack, code, "", "", None, None);
    let mut d = MockDispatcher;
    match compile_and_run(&source, "result", include, &mut d, &()) {
        Ok(v) => Ok(v.to_json()),
        Err(e) => Err(format!("{e}")),
    }
}

#[test]
fn broken_lib_module_is_contained_and_healthy_eval_still_runs() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (no toolchain)");
        return;
    }

    let fixture = LibFixture::new();
    let lib_dir = fixture.0.clone();
    let prelude = prelude_dir();
    let effects_dir =
        tidepool_mcp::ensure_effects_module(&tidepool_mcp::standard_decls()).expect("effects");

    let base_include: Vec<PathBuf> = vec![prelude.clone(), effects_dir.clone(), lib_dir.clone()];
    let base_refs: Vec<&Path> = base_include.iter().map(PathBuf::as_path).collect();

    // 1. Baseline: the broken `Bad` module bricks `import Library` — the eval
    //    fails even though it only uses `goodVerb`.
    let bricked = eval_good_verb(&base_refs);
    assert!(
        bricked.is_err(),
        "sanity: a broken Library re-export should brick the eval, got {bricked:?}"
    );

    // 2. Fault-isolate: the facade is broken, so we get a sanitized Library +
    //    a note naming the culprit.
    let layer = tidepool_mcp::isolate_lib_layer(std::slice::from_ref(&lib_dir), &base_include);
    assert!(
        !layer.prepend_include.is_empty(),
        "a broken lib module must yield a sanitized-facade include prefix"
    );
    let note = layer
        .brick_note
        .as_deref()
        .expect("brick note should name the excluded module");
    assert!(
        note.contains("Bad"),
        "brick note must name the broken module `Bad`: {note}"
    );

    // 3. With the isolated include, the same healthy eval compiles + runs.
    let isolated: Vec<PathBuf> = layer
        .prepend_include
        .iter()
        .cloned()
        .chain(base_include.iter().cloned())
        .collect();
    let isolated_refs: Vec<&Path> = isolated.iter().map(PathBuf::as_path).collect();
    let ok = eval_good_verb(&isolated_refs);
    assert_eq!(
        ok.ok(),
        Some(serde_json::json!(42)),
        "healthy eval must still run once the broken module is excluded"
    );
}

#[test]
fn healthy_lib_layer_is_a_noop() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract not available (no toolchain)");
        return;
    }

    // A lib dir where every re-export compiles → no isolation, no note.
    let dir = std::env::temp_dir().join(format!(
        "tidepool-libiso-healthy-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Good.hs"),
        "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings #-}\n\
         module Good where\n\
         import Tidepool.Prelude hiding (error)\n\
         goodVerb :: Int\n\
         goodVerb = 42\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Library.hs"),
        "module Library ( module Good ) where\nimport Good\n",
    )
    .unwrap();

    let prelude = prelude_dir();
    let effects_dir =
        tidepool_mcp::ensure_effects_module(&tidepool_mcp::standard_decls()).expect("effects");
    let base_include: Vec<PathBuf> = vec![prelude, effects_dir, dir.clone()];

    let layer = tidepool_mcp::isolate_lib_layer(std::slice::from_ref(&dir), &base_include);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        layer.prepend_include.is_empty() && layer.brick_note.is_none(),
        "a fully-healthy lib layer must be a no-op (no prefix, no note)"
    );
}
