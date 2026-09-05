//! Scope-tree decl-plane integration test.
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
//! The second test takes the same claims through `PersistentSession`, whose
//! `mint_scope` seeds a child's decl tip from its PARENT's tip at MINT time —
//! the mechanism that keeps a sibling defining in between out of a scope that
//! has not defined yet.
//!
//! Requires the in-`nix develop` toolchain + a worktree-built extract binary.
//! Run, e.g.:
//! ```text
//! cd haskell && cabal build tidepool-extract-bin
//! TIDEPOOL_EXTRACT=$(cabal list-bin tidepool-extract-bin) \
//!   nix develop ..#default -c cargo test -p tidepool-runtime --test session session_decl_scope_tree::
//! ```

use std::path::{Path, PathBuf};

use tidepool_codegen::scope::{ScopeId, ScopeTree};
use tidepool_repr::Generation;
use tidepool_repr::SessionId;
use tidepool_runtime::session::{ModuleEnv, PersistentSession, SessionError, SessionLib};
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
    // Seed each sibling's decl tip from its parent's, exactly as
    // `PersistentSession::mint_scope` does — a scope `SessionLib` has never
    // been told about resolves to `Generation(0)`, the EMPTY environment, and
    // never to the log's global tip. Driving the pair by hand here means
    // doing by hand what owning the tree would do; the end-to-end form through
    // `mint_scope` is the second test below.
    lib.seed_scope(left, root_tip_before);
    lib.seed_scope(right, root_tip_before);

    // Both siblings therefore start from gen 2 (ROOT's `helper`) as their
    // parent — the sibling-scope worked example from the design doc.
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

/// The `Ask` union tag; nothing in this suite suspends.
/// Nursery size for the (never bootstrapped) resident machine — a decl-only
/// test never reaches the JIT, since `PersistentSession` boots lazily.
const NURSERY: usize = 1 << 16;

/// PARENT-TIP SEEDING AT MINT TIME (`PersistentSession::mint_scope`), end to
/// end through real GHC compiles.
///
/// `SessionLib` keys decl tips by `ScopeId`. If a scope's tip were only
/// established at its FIRST DEFINE, the scope would adopt whatever the decl
/// log's tip happened to be at that moment — so a SIBLING that defined in the
/// interval between this scope's mint and its first use would leak into it.
/// `mint_scope` closes that by seeding the child's tip from its PARENT's tip
/// when the scope is born (`lib.seed_scope(child, lib.scope_tip(parent))`).
///
/// The ordering below is the whole point: mint A, mint B, A defines, and only
/// THEN is B used for the first time. Under first-use seeding B's tip would be
/// A's generation and B would see A's `helper`; under mint-time seeding it is
/// still ROOT's. Both the tip and a compiled probe's numeric result are
/// asserted, so the claim rests on what GHC actually resolved, not on the
/// bookkeeping alone.
///
/// This is the `PersistentSession` counterpart to
/// `sibling_scopes_shadow_independently_without_touching_root` above, which
/// drives a bare `SessionLib` + `ScopeTree` and therefore never exercises
/// `mint_scope`'s seeding at all.
#[test]
fn a_sibling_define_between_mint_and_first_use_does_not_leak() {
    let lib_dir = setup();
    let cache_root = tempfile::tempdir().unwrap();
    // SAFETY: set before any compile/thread activity in this test process
    // (nextest runs every test in its own process, so this never races the
    // sibling test's own tempdir).
    unsafe { std::env::set_var("XDG_CACHE_HOME", cache_root.path()) };
    assert!(paths::cache_dir().starts_with(cache_root.path()));

    let session_root = tempfile::tempdir().unwrap();
    let lib = SessionLib::open(
        SessionId(44),
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .expect("open session")
    .with_validation_include(vec![lib_dir.clone()]);
    // Driven through `PersistentSession` on purpose: `mint_scope`'s seeding IS
    // the code under test, and it lives here because this is the only type
    // that owns the `ScopeTree` and therefore knows a scope's parent.
    let mut core = PersistentSession::new(Some(lib), NURSERY);

    // ---- (1) ROOT defines the name both children will shadow. ----
    let g_root = core
        .define_scoped(&["helper x = x + 100"])
        .expect("define root helper");
    assert_eq!(g_root, Generation(1), "ROOT's define is the first turn");
    let root_tip = core.lib().scope_tip(ScopeId::ROOT);
    assert_eq!(root_tip, g_root, "ROOT's tip is its own define");
    let root_heads_before: Vec<(String, u64)> = core.lib().current_decl_heads();

    // ---- (2) BOTH siblings are minted BEFORE EITHER defines anything. This
    // is the interleaving the hazard needs: B exists, with no turns of its
    // own, while A pushes one. ----
    let a = core.mint_scope(ScopeId::ROOT).expect("ROOT is live");
    let b = core.mint_scope(ScopeId::ROOT).expect("ROOT is live");
    assert_ne!(a, b, "two mints yield two distinct scopes");
    assert_eq!(
        core.lib().scope_tip(a),
        root_tip,
        "A inherits ROOT's tip at MINT time, not at first define"
    );
    assert_eq!(
        core.lib().scope_tip(b),
        root_tip,
        "B inherits ROOT's tip at MINT time, not at first define"
    );

    // ---- (3) A defines; the decl log's global tip is now A's generation. ----
    let g_a = core
        .define_scoped_in(a, &["helper x = x + 1"])
        .expect("define A helper");
    assert_eq!(g_a, Generation(2), "A's define mints the second turn");
    assert_eq!(core.lib().scope_tip(a), g_a, "A's tip is A's own define");

    // ---- (4) THE REGRESSION: B's FIRST USE, after A defined. ----
    assert_eq!(
        core.lib().scope_tip(b),
        root_tip,
        "B's first use must still see ROOT's tip — A's define must not have \
         become B's parent generation"
    );
    let r = run_probe(
        &lib_dir,
        core.lib().include_dir(),
        &core.lib().cache_salt(),
        &probe(core.lib().scope_tip(b), "Int", "helper 5"),
    );
    assert_eq!(
        r,
        serde_json::json!(105),
        "B compiles against ROOT's helper (x + 100), NOT A's (x + 1)"
    );

    // ---- (5) B then defines its own; B's tip advances to B's turn only. ----
    let g_b = core
        .define_scoped_in(b, &["helper x = x * 2"])
        .expect("define B helper");
    assert_eq!(g_b, Generation(3), "B's define mints the third turn");
    assert_ne!(g_a, g_b, "each sibling mints its OWN generation");
    assert_eq!(core.lib().scope_tip(b), g_b, "B's tip is B's own define");
    let r = run_probe(
        &lib_dir,
        core.lib().include_dir(),
        &core.lib().cache_salt(),
        &probe(g_b, "Int", "helper 5"),
    );
    assert_eq!(
        r,
        serde_json::json!(10),
        "B's own helper (x * 2) after B defines"
    );

    // ---- (6) ROOT gained neither child's declaration. ----
    assert_eq!(
        core.lib().scope_tip(ScopeId::ROOT),
        root_tip,
        "neither child's define may advance ROOT's tip"
    );
    assert_eq!(
        core.lib().current_decl_heads(),
        root_heads_before,
        "neither child's define may change ROOT's visible heads"
    );
    let r = run_probe(
        &lib_dir,
        core.lib().include_dir(),
        &core.lib().cache_salt(),
        &probe(root_tip, "Int", "helper 5"),
    );
    assert_eq!(
        r,
        serde_json::json!(105),
        "ROOT's helper (x + 100) is unchanged by either child"
    );

    // ---- (7) B's define did not disturb A. ----
    let r = run_probe(
        &lib_dir,
        core.lib().include_dir(),
        &core.lib().cache_salt(),
        &probe(g_a, "Int", "helper 5"),
    );
    assert_eq!(
        r,
        serde_json::json!(6),
        "A's own helper (x + 1) survives B's later define"
    );
}

/// A fresh actor scope starts from an empty declaration view. Its descendants
/// inherit declarations authored inside that actor, but neither direction can
/// observe declarations from the session root.
#[test]
fn isolated_scope_has_an_empty_independent_declaration_chain() {
    let lib_dir = setup();
    let cache_root = tempfile::tempdir().unwrap();
    // SAFETY: nextest runs this test in its own process and this is set before
    // the first compile.
    unsafe { std::env::set_var("XDG_CACHE_HOME", cache_root.path()) };

    let session_root = tempfile::tempdir().unwrap();
    let lib = SessionLib::open(
        SessionId(46),
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .expect("open session")
    .with_validation_include(vec![lib_dir.clone()]);
    let mut core = PersistentSession::new(Some(lib), NURSERY);

    let root_generation = core
        .define_scoped(&["rootOnly = 41 :: Int"])
        .expect("define at session root");
    let isolated = core.mint_isolated_scope();
    assert_eq!(
        core.lib().scope_tip(isolated),
        Generation(0),
        "an isolated root is not seeded from the ambient declaration tip"
    );
    assert_eq!(core.current_lib_module_in(isolated), None);

    let actor_generation = core
        .define_scoped_in(isolated, &["actorOnly = 42 :: Int"])
        .expect("define in isolated actor scope");
    let actor_child = core
        .mint_scope(isolated)
        .expect("isolated actor scope is live");
    assert_eq!(core.lib().scope_tip(actor_child), actor_generation);

    let actor_result = run_probe(
        &lib_dir,
        core.lib().include_dir(),
        &core.lib().cache_salt(),
        &probe(actor_generation, "Int", "actorOnly"),
    );
    assert_eq!(actor_result, serde_json::json!(42));
    let root_result = run_probe(
        &lib_dir,
        core.lib().include_dir(),
        &core.lib().cache_salt(),
        &probe(root_generation, "Int", "rootOnly"),
    );
    assert_eq!(root_result, serde_json::json!(41));

    assert_eq!(
        core.lib().scope_tip(ScopeId::ROOT),
        root_generation,
        "actor declarations never advance the ambient root"
    );
}

/// `define_scoped_in`/`retract_in` on a dead scope (never minted, or already
/// retired) must reject with a typed [`SessionError::DeadScope`] rather than
/// silently accumulating decl-plane state under a `ScopeId` no session-owned
/// `ScopeTree` will ever walk to again — the same liveness precondition the
/// 2026-08-19 review asked for at the value-plane mount seam, applied to the
/// decl plane. The liveness check runs BEFORE any GHC work (no compile, no
/// `TIDEPOOL_EXTRACT`, no `setup()`), so this needs no extract toolchain.
#[test]
fn define_and_retract_scoped_in_reject_a_dead_scope_without_touching_the_log() {
    let session_root = tempfile::tempdir().unwrap();
    let lib = SessionLib::open(
        SessionId(45),
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .expect("open session");
    let mut core = PersistentSession::new(Some(lib), NURSERY);

    let child = core.mint_scope(ScopeId::ROOT).expect("ROOT is live");
    core.retire_scope(child);

    let generation_before = core.lib().generation();

    let result = core.define_scoped_in(child, &["helper x = x"]);
    assert!(
        matches!(result, Err(SessionError::DeadScope(s)) if s == child),
        "expected a typed DeadScope error, got {result:?}"
    );

    let never_minted = ScopeId(999_999);
    let result = core.define_scoped_in(never_minted, &["helper x = x"]);
    assert!(
        matches!(result, Err(SessionError::DeadScope(s)) if s == never_minted),
        "expected a typed DeadScope error, got {result:?}"
    );

    let result = core.retract_in(child, "helper");
    assert!(
        matches!(result, Err(SessionError::DeadScope(s)) if s == child),
        "expected a typed DeadScope error, got {result:?}"
    );

    assert_eq!(
        core.lib().generation(),
        generation_before,
        "no turn was ever pushed to the shared decl log — the liveness \
         check runs before any GHC work, so a rejected define never reaches \
         run_turn at all"
    );

    // ROOT stays byte-identical: always live, never refused.
    let g = core
        .define_scoped_in(ScopeId::ROOT, &[])
        .expect("ROOT is always live (and an empty batch is a no-op)");
    assert_eq!(g, generation_before, "an empty batch bumps nothing");
}
