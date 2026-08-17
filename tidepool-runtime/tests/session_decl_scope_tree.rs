//! Scope-tree decl-plane integration test (PRD 21 lane C2 Wave A2,
//! `plans/self-iterating-harness/21-c2-scope-trees.md` §1.2).
//!
//! Drives the REAL compile path, same shape as `session_decl_accum.rs`, but
//! exercises `SessionLib`'s `_in(scope)` API against a genuine
//! `tidepool_codegen::scope::ScopeTree`: a `helper` defined at ROOT, two
//! sibling scopes minted off ROOT, and each sibling defining its OWN
//! same-named `helper` with a different body.
//!
//! Proves, via real GHC compiles (not by reading rendered source):
//! (a) a ROOT-only decl stays callable from BOTH children;
//! (b) each child's own `helper` resolves to ITS OWN body;
//! (c) neither sibling's `helper` is visible from the other (each probe only
//!     ever imports its own scope's tip module, so there is no compilation
//!     path to the sibling's body — a leaked chain would show up either as a
//!     GHC "ambiguous occurrence" or as the WRONG numeric result);
//! (d) ROOT's tip and ROOT's visible decl heads are unchanged by either
//!     child's define.
//!
//! Requires the in-`nix develop` toolchain + a worktree-built extract binary.
//! Run, e.g.:
//! ```text
//! cd haskell && cabal build tidepool-extract-bin
//! TIDEPOOL_EXTRACT=$(cabal list-bin tidepool-extract-bin) \
//!   nix develop ..#default -c cargo test -p tidepool-runtime --test session_decl_scope_tree
//! ```

use std::path::{Path, PathBuf};

use tidepool_codegen::scope::{ScopeId, ScopeTree};
use tidepool_repr::Generation;
use tidepool_repr::SessionId;
use tidepool_runtime::session::{ModuleEnv, SessionLib};
use tidepool_runtime::{compile_and_run_pure_salted, paths};
use tidepool_testing::eval_harness;

/// Confirm the extract toolchain is reachable and return the lib include dir.
fn setup() -> PathBuf {
    eval_harness::require_extract();
    let lib = eval_harness::prelude_path();
    assert!(lib.exists(), "haskell/lib include dir must exist");
    lib
}

/// Build a probe module that imports the session library at generation `gen`
/// and binds `result` to `expr`.
fn probe(gen: Generation, ty: &str, expr: &str) -> String {
    format!(
        "{{-# LANGUAGE OverloadedStrings, ScopedTypeVariables, LambdaCase #-}}\n\
         module Probe where\n\
         import qualified Tidepool.Data.Text as T\n\
         import Tidepool.Session.Lib.G{gen}\n\
         result :: {ty}\n\
         result = {expr}\n"
    )
}

/// Compile+run a probe against the session, returning the JSON result.
fn run_probe(lib_dir: &Path, session_dir: &Path, salt: &str, src: &str) -> serde_json::Value {
    let include = [session_dir, lib_dir];
    let result = compile_and_run_pure_salted(src, "result", &include, Some(salt))
        .unwrap_or_else(|e| panic!("probe failed to compile/run:\n{src}\n--- error ---\n{e}"));
    (&result).into()
}

#[test]
fn sibling_scopes_shadow_independently_without_touching_root() {
    let lib_dir = setup();
    let cache_root = tempfile::tempdir().unwrap();
    // SAFETY: set before any compile/thread activity in this test process.
    unsafe { std::env::set_var("XDG_CACHE_HOME", cache_root.path()) };
    assert!(paths::cache_dir().starts_with(cache_root.path()));

    let session_root = tempfile::tempdir().unwrap();
    let mut lib = SessionLib::open(
        SessionId(43),
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .expect("open session")
    .with_validation_include(vec![lib_dir.clone()]);

    // ---- ROOT: a decl every scope should keep seeing, plus the name both
    // children will independently shadow. ----
    let g_base = lib.define("base x = x + 1000").expect("define base");
    assert_eq!(g_base, Generation(1));
    let g_helper_root = lib.define("helper x = x").expect("define root helper");
    assert_eq!(g_helper_root, Generation(2));

    let root_heads_before: Vec<(String, u64)> = lib.current_decl_heads();
    let root_tip_before = lib.scope_tip(ScopeId::ROOT);
    assert_eq!(root_tip_before, Generation(2));

    // ---- mint two sibling scopes off ROOT (the ScopeTree PersistentSession
    // will own in the later integration wave; here it's driven directly). ----
    let mut tree = ScopeTree::new();
    let left = tree.mint_child(ScopeId::ROOT).expect("root is live");
    let right = tree.mint_child(ScopeId::ROOT).expect("root is live");
    assert_ne!(left, right);

    // Each sibling, on FIRST use, adopts the log's current tip (gen 2, ROOT's
    // `helper`) as its parent — the sibling-scope worked example from the
    // design doc.
    let g_left = lib
        .define_scoped_in(left, "helper x = x + 1")
        .expect("define left helper");
    let g_right = lib
        .define_scoped_in(right, "helper x = x * 2")
        .expect("define right helper");
    assert_ne!(g_left, g_right, "each sibling mints its OWN generation");

    // (d) ROOT is untouched by either child's define.
    assert_eq!(
        lib.scope_tip(ScopeId::ROOT),
        root_tip_before,
        "a child define must never advance ROOT's tip"
    );
    assert_eq!(
        lib.current_decl_heads(),
        root_heads_before,
        "a child define must never change ROOT's visible heads"
    );

    // (a) `base` (defined only at ROOT, never shadowed) is callable from BOTH
    // children — real GHC compiles against each child's own tip module.
    let r = run_probe(
        &lib_dir,
        lib.include_dir(),
        &lib.cache_salt(),
        &probe(g_left, "Int", "base 1"),
    );
    assert_eq!(r, serde_json::json!(1001), "ROOT's `base` reaches left");
    let r = run_probe(
        &lib_dir,
        lib.include_dir(),
        &lib.cache_salt(),
        &probe(g_right, "Int", "base 1"),
    );
    assert_eq!(r, serde_json::json!(1001), "ROOT's `base` reaches right");

    // (b) + (c) each child's `helper` resolves to ITS OWN body. Each probe
    // imports ONLY its own scope's tip module — there is no import path to
    // the sibling's body, so a distinct, correct numeric result here is the
    // real (GHC-driven) proof that the siblings never merged.
    let r = run_probe(
        &lib_dir,
        lib.include_dir(),
        &lib.cache_salt(),
        &probe(g_left, "Int", "helper 5"),
    );
    assert_eq!(r, serde_json::json!(6), "left's own helper (x + 1)");
    let r = run_probe(
        &lib_dir,
        lib.include_dir(),
        &lib.cache_salt(),
        &probe(g_right, "Int", "helper 5"),
    );
    assert_eq!(r, serde_json::json!(10), "right's own helper (x * 2)");

    // ROOT's own `helper` is still independently resolvable at its own gen —
    // the tree didn't retroactively rewrite ROOT's rendered module either.
    let r = run_probe(
        &lib_dir,
        lib.include_dir(),
        &lib.cache_salt(),
        &probe(g_helper_root, "Int", "helper 5"),
    );
    assert_eq!(r, serde_json::json!(5), "ROOT's own helper (identity)");
}
