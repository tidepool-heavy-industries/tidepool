//! Pins item 20's defect (b): an unresolved external that the translator
//! POISONS (the lazy `0x45` kind=4 sentinel) must be named LOUDLY on the
//! extract's stderr — never baked silently. Before this diagnostic existed,
//! the only symptom was a runtime `TypeMetadata` trap with no symbol name.
//!
//! The poison condition is manufactured with the E6 mis-tiering fault
//! injection (`TIDEPOOL_TEST_FORCE_VALIDATION_ONLY=<module>`, documented in
//! `haskell/CLAUDE.md`): denying a dependency module `core2core` leaves its
//! bindings without usable unfoldings, so the target's reference to it
//! resolves to a poison sentinel — exactly the shape the hs-boot clobber
//! (defect a) produced organically. This is also the "deliberate
//! detection-power test" that knob's doc promises.
//!
//! Needs `TIDEPOOL_EXTRACT` (run inside `nix develop`).

use std::process::Command;

use tidepool_testing::eval_harness::{prelude_path, require_extract};

#[test]
fn poisoned_external_is_named_on_stderr_at_extract_time() {
    require_extract();
    let extract = std::env::var("TIDEPOOL_EXTRACT").expect("require_extract checked this");

    let dir = tempfile::tempdir().expect("tempdir");
    let deps = dir.path().join("deps");
    let out = dir.path().join("out");
    std::fs::create_dir_all(&deps).expect("mkdir deps");
    std::fs::create_dir_all(&out).expect("mkdir out");

    std::fs::write(
        deps.join("Dep.hs"),
        "{-# LANGUAGE NoImplicitPrelude #-}\n\
         module Dep where\n\
         import Tidepool.Prelude\n\
         helper :: Int -> Int\n\
         helper n = n + 1\n\
         {-# NOINLINE helper #-}\n",
    )
    .expect("write Dep.hs");
    let main_path = dir.path().join("Test.hs");
    std::fs::write(
        &main_path,
        "{-# LANGUAGE NoImplicitPrelude #-}\n\
         module Test where\n\
         import Tidepool.Prelude\n\
         import qualified Dep\n\
         main :: Int\n\
         main = Dep.helper 41\n",
    )
    .expect("write Test.hs");

    let output = Command::new(&extract)
        .arg(&main_path)
        .arg("--output-dir")
        .arg(&out)
        .arg("--target")
        .arg("main")
        .arg("--include")
        .arg(prelude_path())
        .arg("--include")
        .arg(&deps)
        .env("TIDEPOOL_TEST_FORCE_VALIDATION_ONLY", "Dep")
        .output()
        .expect("run tidepool-extract");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("POISONED"),
        "a poisoned unresolved external must be announced on stderr; got:\n{stderr}"
    );
    assert!(
        stderr.contains("Dep.helper"),
        "the diagnostic must NAME the poisoned symbol (Dep.helper); got:\n{stderr}"
    );
}

/// The same fixture WITHOUT fault injection extracts clean — the diagnostic
/// only fires when something was actually poisoned, so ordinary evals stay
/// quiet on stderr.
#[test]
fn healthy_extraction_prints_no_poison_diagnostic() {
    require_extract();
    let extract = std::env::var("TIDEPOOL_EXTRACT").expect("require_extract checked this");

    let dir = tempfile::tempdir().expect("tempdir");
    let deps = dir.path().join("deps");
    let out = dir.path().join("out");
    std::fs::create_dir_all(&deps).expect("mkdir deps");
    std::fs::create_dir_all(&out).expect("mkdir out");

    std::fs::write(
        deps.join("Dep.hs"),
        "{-# LANGUAGE NoImplicitPrelude #-}\n\
         module Dep where\n\
         import Tidepool.Prelude\n\
         helper :: Int -> Int\n\
         helper n = n + 1\n\
         {-# NOINLINE helper #-}\n",
    )
    .expect("write Dep.hs");
    let main_path = dir.path().join("Test.hs");
    std::fs::write(
        &main_path,
        "{-# LANGUAGE NoImplicitPrelude #-}\n\
         module Test where\n\
         import Tidepool.Prelude\n\
         import qualified Dep\n\
         main :: Int\n\
         main = Dep.helper 41\n",
    )
    .expect("write Test.hs");

    let output = Command::new(&extract)
        .arg(&main_path)
        .arg("--output-dir")
        .arg(&out)
        .arg("--target")
        .arg("main")
        .arg("--include")
        .arg(prelude_path())
        .arg("--include")
        .arg(&deps)
        .output()
        .expect("run tidepool-extract");

    assert!(
        output.status.success(),
        "healthy extraction must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("POISONED"),
        "a clean extraction must not announce poisons; got:\n{stderr}"
    );
}
