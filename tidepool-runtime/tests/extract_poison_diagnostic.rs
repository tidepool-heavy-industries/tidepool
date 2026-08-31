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

use tidepool_extract_cmd::{ExtractCmd, ResolvedExtractBin};
use tidepool_testing::eval_harness::{prelude_path, require_extract};

struct PoisonFixture {
    _root: tempfile::TempDir,
    main: std::path::PathBuf,
    deps: std::path::PathBuf,
    out: std::path::PathBuf,
}

fn poison_fixture() -> PoisonFixture {
    let root = tempfile::tempdir().expect("tempdir");
    let deps = root.path().join("deps");
    let out = root.path().join("out");
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
    let main = root.path().join("Test.hs");
    std::fs::write(
        &main,
        "{-# LANGUAGE NoImplicitPrelude #-}\n\
         module Test where\n\
         import Tidepool.Prelude\n\
         import qualified Dep\n\
         main :: Int\n\
         main = Dep.helper 41\n",
    )
    .expect("write Test.hs");

    PoisonFixture {
        _root: root,
        main,
        deps,
        out,
    }
}

fn request_for(fixture: &PoisonFixture, extract: &str) -> ExtractCmd {
    let mut request = ExtractCmd::with_bin(ResolvedExtractBin::assume_resolved(extract));
    request
        .input(&fixture.main)
        .output_dir(&fixture.out)
        .target("main")
        .include(prelude_path())
        .include(&fixture.deps);
    request
}

#[test]
fn poisoned_external_is_named_on_stderr_at_extract_time() {
    require_extract();
    let extract = std::env::var("TIDEPOOL_EXTRACT").expect("require_extract checked this");

    let fixture = poison_fixture();
    let request = request_for(&fixture, &extract);
    let output = Command::new(&extract)
        .args(request.worker_argv())
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

    // A poison is LAZY: the extraction still succeeds and still emits a
    // program. That contract is what makes the self-describing sentinel
    // meaningful — a poisoned program is one that runs until the poison is
    // forced, not one that fails to build.
    assert!(
        output.status.success(),
        "a lazy poison must not fail the extraction; stderr:\n{stderr}"
    );

    // The program itself says which external it replaced: the emitted node is
    // `0x45 << 56 | slot << 8 | 4`, and meta.cbor's `poisoned` table maps that
    // slot back to the qualified name. Neither half is a side channel — this
    // is the end-to-end proof of item 20(b)'s fix.
    let expr_bytes = std::fs::read(fixture.out.join("main.cbor")).expect("read main.cbor");
    let expr = tidepool_repr::serial::read_cbor(&expr_bytes).expect("decode main.cbor");
    let slots: Vec<u64> = expr
        .nodes
        .iter()
        .filter_map(|frame| match frame {
            tidepool_repr::frame::CoreFrame::Var(v) => v.sentinel(),
            _ => None,
        })
        .filter(|s| s.kind == 4)
        .map(|s| s.slot)
        .collect();
    assert!(
        !slots.is_empty(),
        "the poisoned program must carry a kind=4 sentinel; nodes: {:?}",
        expr.nodes
    );
    assert!(
        slots.iter().all(|s| *s != 0),
        "a poison sentinel must carry a NON-ZERO identity slot (got {slots:?}) — \
         slot 0 is the anonymous, pre-fix encoding"
    );

    let meta_bytes = std::fs::read(fixture.out.join("meta.cbor")).expect("read meta.cbor");
    let (_, warnings) =
        tidepool_repr::serial::read_metadata(&meta_bytes).expect("decode meta.cbor");
    for slot in &slots {
        let name = warnings
            .poisoned
            .iter()
            .find(|(s, _)| s == slot)
            .map(|(_, n)| n.as_str());
        assert_eq!(
            name,
            Some("Dep.helper"),
            "meta.cbor's `poisoned` table must name slot {slot}; table: {:?}",
            warnings.poisoned
        );
    }
}

/// The same fixture WITHOUT fault injection extracts clean — the diagnostic
/// only fires when something was actually poisoned, so ordinary evals stay
/// quiet on stderr.
#[test]
fn healthy_extraction_prints_no_poison_diagnostic() {
    require_extract();
    let extract = std::env::var("TIDEPOOL_EXTRACT").expect("require_extract checked this");

    let fixture = poison_fixture();
    let request = request_for(&fixture, &extract);
    let output = Command::new(&extract)
        .args(request.worker_argv())
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
