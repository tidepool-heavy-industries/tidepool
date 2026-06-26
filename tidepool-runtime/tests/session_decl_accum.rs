//! Lane A — declaration-accumulation integration test (plan §5.0 VERIFY).
//!
//! Drives the REAL compile path over successive turns with a growing session
//! include: each turn either appends a declaration (`SessionLib::define`, which
//! sources binder names from GHC and regenerates the gen-versioned module) or
//! compiles+runs an expression against the accumulated `Tidepool.Session.Lib.G<g>`
//! module through `compile_and_run_pure`.
//!
//! Proves: functions accumulate and shadow latest-wins; a redefined `data` type
//! coexists with its older shape via selective re-export (no GHC
//! conflicting-export error); both shapes stay independently resolvable.
//!
//! Requires the in-`nix develop` toolchain + a worktree-built extract binary.
//! Run, e.g.:
//! ```text
//! cd haskell && cabal build tidepool-extract-bin
//! TIDEPOOL_EXTRACT=$(cabal list-bin tidepool-extract-bin) \
//!   nix develop ..#default -c cargo test -p tidepool-runtime --test session_decl_accum
//! ```

use std::path::{Path, PathBuf};

use serial_test::serial;
use tidepool_repr::Generation;
use tidepool_runtime::session::{ModuleEnv, SessionLib};
use tidepool_runtime::{compile_and_run_pure_salted, paths};
use tidepool_repr::SessionId;

/// Locate the extract binary (env override, the worktree symlink, or the
/// dist-newstyle build) and confirm GHC is on PATH. Returns the lib include dir
/// on success, or `None` to skip (toolchain unavailable).
fn setup() -> Option<PathBuf> {
    // GHC is needed by tidepool-extract; only available inside `nix develop`.
    let ghc_ok = std::process::Command::new("ghc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ghc_ok {
        eprintln!("Skipping: GHC not available (run inside `nix develop`)");
        return None;
    }

    let haskell = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("haskell");

    if std::env::var("TIDEPOOL_EXTRACT").is_err() {
        // Prefer the worktree symlink; fall back to the dist-newstyle build.
        let symlink = haskell.join("tidepool-extract");
        let dist = haskell
            .join("dist-newstyle/build/x86_64-linux/ghc-9.12.2/tidepool-harness-0.1.0.0")
            .join("x/tidepool-extract-bin/build/tidepool-extract-bin/tidepool-extract-bin");
        if symlink.exists() {
            std::env::set_var("TIDEPOOL_EXTRACT", &symlink);
        } else if dist.exists() {
            std::env::set_var("TIDEPOOL_EXTRACT", &dist);
        } else {
            eprintln!("Skipping: no tidepool-extract binary (set TIDEPOOL_EXTRACT)");
            return None;
        }
    }

    let lib = haskell.join("lib");
    assert!(lib.exists(), "haskell/lib include dir must exist");
    Some(lib)
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

/// Compile+run a probe against the session, returning the JSON result. The
/// session's `(session, generation)` salt is threaded into the cache key (the
/// salt is load-bearing, not just plumbing — it isolates sessions/gens).
fn run_probe(lib_dir: &Path, session_dir: &Path, salt: &str, src: &str) -> serde_json::Value {
    let include = [session_dir, lib_dir];
    let result = compile_and_run_pure_salted(src, "result", &include, Some(salt))
        .unwrap_or_else(|e| panic!("probe failed to compile/run:\n{src}\n--- error ---\n{e}"));
    (&result).into()
}

#[test]
#[serial]
fn declarations_accumulate_and_types_coexist() {
    let Some(lib_dir) = setup() else { return };
    // Force a clean cache so a stale entry from a prior run can't mask a bug —
    // the growing include changes the key, but be explicit for the proof.
    let _ = std::fs::remove_dir_all(paths::cache_dir());

    let session_root = tempfile::tempdir().unwrap();
    let mut lib = SessionLib::open(
        SessionId(42),
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .expect("open session");

    // ---- turn 1: define `slug` ----
    let g1 = lib
        .define("slug t = T.toLower (T.replace \" \" \"-\" t)")
        .expect("define slug");
    assert_eq!(g1, Generation(1));

    // ---- turn 2: evaluate `slug "a b"` against G1 -> "a-b" ----
    let r = run_probe(
        &lib_dir,
        lib.include_dir(),
        &lib.cache_salt(),
        &probe(g1, "T.Text", "slug \"a b\""),
    );
    assert_eq!(r, serde_json::json!("a-b"), "turn 2: slug lowercases+hyphens");

    // ---- turn 3: redefine `slug` (latest-wins shadowing) ----
    let g3 = lib.define("slug t = T.toUpper t").expect("redefine slug");
    assert_eq!(g3, Generation(2));

    // ---- turn 4: evaluate against G2 -> the NEW slug ("A B") ----
    let r = run_probe(
        &lib_dir,
        lib.include_dir(),
        &lib.cache_salt(),
        &probe(g3, "T.Text", "slug \"a b\""),
    );
    assert_eq!(r, serde_json::json!("A B"), "turn 4: redefined slug wins");

    // ---- turn 5: define a data type, then reshape it in a later gen ----
    let g_old = lib.define("data Foo = A | B").expect("define Foo v1");
    assert_eq!(g_old, Generation(3));
    let g_new = lib.define("data Foo = X | A | B").expect("reshape Foo");
    assert_eq!(g_new, Generation(4));

    // New shape: construct `X` (only in the reshaped Foo) and match it. That the
    // chain G4 -> (import G3 hiding Foo(..)) compiles at all proves there is NO
    // conflicting-export error.
    let r = run_probe(
        &lib_dir,
        lib.include_dir(),
        &lib.cache_salt(),
        &probe(
            g_new,
            "Int",
            "case X of { X -> 1; A -> 2; B -> 3 }",
        ),
    );
    assert_eq!(r, serde_json::json!(1), "turn 5: reshaped Foo is in scope");

    // Old shape stays independently resolvable: import G3 directly and match its
    // `A`/`B` (the older Foo). Both gen modules coexist.
    let r = run_probe(
        &lib_dir,
        lib.include_dir(),
        &lib.cache_salt(),
        &probe(g_old, "Int", "case A of { A -> 10; B -> 20 }"),
    );
    assert_eq!(r, serde_json::json!(10), "turn 5: old Foo shape still resolvable");

    // The accumulated functions are still reachable from the newest gen too.
    let r = run_probe(
        &lib_dir,
        lib.include_dir(),
        &lib.cache_salt(),
        &probe(g_new, "T.Text", "slug \"x y\""),
    );
    assert_eq!(r, serde_json::json!("X Y"), "newest gen re-exports `slug`");
}
