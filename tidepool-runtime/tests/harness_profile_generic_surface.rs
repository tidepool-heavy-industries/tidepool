//! The harness compilation profile.
//!
//! Proves, through the REAL `tidepool-extract` binary (not a hand-wired
//! stand-in), that a harness-shaped module:
//!
//!   1. compiles with `--harness-profile` and NO `{-# LANGUAGE ... #-}` block
//!      of its own (the standard extension set arrives via one LANGUAGE
//!      pragma line SPLICED onto a scratch copy of the source — see
//!      `Main.spliceHarnessProfilePragma` in `haskell/app/Main.hs` — never a
//!      GHC flag: a compilation-request cache keys on rendered source bytes,
//!      not CLI flags, so a flags-based profile would be invisible to any
//!      future cached caller);
//!   2. actually exercises the collision hazard the wave exists to fix: it
//!      derives `Generic` on a local type and round-trips it through
//!      `Tidepool.Harness.Prelude`'s `gFrom`/`gTo`, with `Tidepool.Prelude`'s
//!      wholesale `Control.Lens` re-export (and its own `from`/`to`) in
//!      scope the whole time.
//!
//! A companion test compiles the identical source WITHOUT the flag and
//! expects it to FAIL — proof the flag is load-bearing, not a no-op the
//! module would have compiled under anyway.
//!
//! Requires a worktree extract binary (`cabal build tidepool-extract-bin`,
//! then `TIDEPOOL_EXTRACT` pointed at it, or run inside `nix develop`). Skips
//! cleanly when the extractor is unreachable, matching every other
//! extract-facing test in this crate (see `generic_deriving_337.rs`).

use std::path::PathBuf;
use std::process::Command;

use tidepool_testing::eval_harness::{effects_include, extract_available, prelude_path};

/// A harness-shaped module: no LANGUAGE pragma block, imports
/// `Tidepool.Harness.Prelude` (which re-exports `Tidepool.Prelude` wholesale,
/// including its `Control.Lens` `from`/`to`), derives `Generic` on a local
/// type, and round-trips it via the safe-named `gFrom`/`gTo` — the exact
/// shape the generic-codec spike hit the `from`/`to` ambiguity through.
const FIXTURE_SOURCE: &str = "\
module HarnessProfileFixture where

import Tidepool.Harness.Prelude

data Animal = Cat | Dog | Bird
  deriving (Show, Generic)

roundTripAnimal :: Animal -> Animal
roundTripAnimal = gTo . gFrom

data Greeting = Greeting { greetTo :: Text }
  deriving (Show, Generic)

loop :: Harness Text
loop = do
  d <- runLLMTurn @Text \"pick a subject\"
  pure (greetTo (Greeting d) <> \" says hi to \" <> show (roundTripAnimal Cat))
";

/// Write `FIXTURE_SOURCE` into a fresh temp dir under its required filename
/// (GHC derives the module name from the file's basename) and return the dir.
fn write_fixture() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    std::fs::write(dir.path().join("HarnessProfileFixture.hs"), FIXTURE_SOURCE)
        .expect("write fixture source");
    dir
}

/// Shell out to the real `tidepool-extract` binary directly (not
/// `tidepool_runtime::compile_haskell`, which has no `--harness-profile`
/// pass-through) with `--target loop`, optionally passing `--harness-profile`.
/// Returns whether the compile succeeded, plus stdout+stderr for diagnosis.
fn run_extract(harness_profile: bool) -> (bool, String) {
    let src_dir = write_fixture();
    let out_dir = tempfile::TempDir::new().expect("create output dir");
    let extract_bin =
        std::env::var("TIDEPOOL_EXTRACT").unwrap_or_else(|_| "tidepool-extract".to_string());

    let mut cmd = Command::new(&extract_bin);
    cmd.arg(src_dir.path().join("HarnessProfileFixture.hs"));
    cmd.arg("--output-dir").arg(out_dir.path());
    cmd.arg("--target").arg("loop");
    if harness_profile {
        cmd.arg("--harness-profile");
    }
    let prelude: PathBuf = prelude_path();
    let effects = effects_include();
    cmd.arg("--include").arg(&prelude);
    for dir in &effects {
        cmd.arg("--include").arg(dir);
    }

    let output = cmd.output().expect("spawn tidepool-extract");
    let combined = format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    (output.status.success(), combined)
}

/// `--harness-profile` on: the pragma-free fixture compiles clean through the
/// real extract, `Generic` is in scope, and `gFrom`/`gTo` resolve unambiguously
/// against `Tidepool.Prelude`'s wholesale `Control.Lens` re-export.
#[test]
fn harness_profile_compiles_pragma_free_generic_module() {
    assert!(
        !FIXTURE_SOURCE.contains("LANGUAGE"),
        "the fixture itself must carry no LANGUAGE pragma block — that's the \
         whole claim under test"
    );
    if !extract_available() {
        eprintln!("skipping: tidepool-extract unavailable (set TIDEPOOL_EXTRACT / nix develop)");
        return;
    }
    let (ok, log) = run_extract(true);
    assert!(
        ok,
        "harness-profile compile of a pragma-free Generic-deriving module must succeed:\n{log}"
    );
    assert!(
        !log.to_ascii_lowercase().contains("ambiguous occurrence"),
        "the from/to ambiguity from the generic-codec spike must be unreachable \
         through Tidepool.Harness.Prelude's gFrom/gTo:\n{log}"
    );
}

/// The SAME source, WITHOUT the flag, fails — proof the profile is actually
/// supplying the extensions the module needs (NoImplicitPrelude,
/// OverloadedStrings, DeriveGeneric, TypeApplications, ...), not a no-op.
#[test]
fn without_harness_profile_the_same_pragma_free_module_fails() {
    if !extract_available() {
        eprintln!("skipping: tidepool-extract unavailable (set TIDEPOOL_EXTRACT / nix develop)");
        return;
    }
    let (ok, log) = run_extract(false);
    assert!(
        !ok,
        "a pragma-free module must NOT compile without --harness-profile \
         (if it does, the flag isn't load-bearing):\n{log}"
    );
}
