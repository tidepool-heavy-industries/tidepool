//! Explicit multi-target extraction (`engine::compile_turns`, extract's
//! `--targets` mode — `haskell/app/Main.hs`'s `runMultiTargetClosed`; see
//! `plans/post-restart/extract-wave/boot/03-targets-prereq.md`). These pin
//! the two requirements that distinguish this mode from `--all-closed`
//! (whose correct behaviour is the opposite of both):
//!
//!   - `multi_target_fails_on_any_bad_target`: a two-target request where ONE
//!     target is bogus must FAIL the whole extraction, not silently emit the
//!     good one.
//!   - `multi_target_asks_stay_distinct`: two targets with DIFFERENT
//!     `runLLMTurn` call sites keep their sites separate end-to-end through
//!     the Rust reader (`engine::compile_turns`), never merged into one
//!     ambiguous sidecar.
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH (run inside
//! `nix develop`; see `haskell/CLAUDE.md`).

use tidepool_harness::engine::{self, EngineConfig};
use tidepool_runtime::CompileError;

fn extract_available() -> bool {
    tidepool_testing::eval_harness::extract_available()
}

fn prelude_dir() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| std::path::PathBuf::from("haskell/lib"))
}

/// A module with TWO independent top-level targets, each suspending on its
/// OWN `runLLMTurn` call at a DIFFERENT answer type — `targetA` at `Int`,
/// `targetB` at `Text`. Deliberately hand-assembled from `build_preamble`
/// rather than routed through `engine::template_turn_for` (which emits
/// exactly ONE `result` binding via its shared `__user`/`__anchor` names):
/// two independent top-level bindings are exactly what this mode's
/// `--targets a,b` exists to translate from a SINGLE GHC session, and a
/// second `template_turn_for` call would collide on those shared names if
/// naively concatenated.
fn two_target_source() -> (EngineConfig, String) {
    let cfg = EngineConfig::from_decls(vec![tidepool_mcp::runllmturn_decl()], prelude_dir(), None)
        .expect("engine config");
    let mut source = tidepool_mcp::build_preamble(&cfg.decls, false);
    source.push_str(
        "\n\
         targetA :: M Value\n\
         targetA = do\n\
         \x20 r <- (runLLMTurn @Int \"siteA\" :: M Int)\n\
         \x20 pure (toJSON r)\n\
         \n\
         targetB :: M Value\n\
         targetB = do\n\
         \x20 r <- (runLLMTurn @Text \"siteB\" :: M Text)\n\
         \x20 pure (toJSON r)\n",
    );
    (cfg, source)
}

/// **fail-on-any-bad-target.** `compile_turns` over `["targetA", "nonexistentTarget"]`
/// must return `Err` — the extract's `--targets` mode fails the WHOLE spawn
/// (`runMultiTargetClosed` calls `translateTargetClosed` directly, with no
/// per-target `try`) rather than silently emitting `targetA.cbor` and staying
/// quiet about the missing one. Proven against the SAME source that compiles
/// cleanly for `targetA` alone (below), so the failure is attributable to the
/// bogus name, not an unrelated environment problem.
#[test]
fn multi_target_fails_on_any_bad_target() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let (cfg, source) = two_target_source();
    let target = cfg.turn_target(None).expect("turn target");

    // Control: targetA alone compiles cleanly against this exact source —
    // establishes the bogus name below is what causes the failure.
    let good = engine::compile_turns(
        &cfg.extract_bin,
        &source,
        &["targetA"],
        &target.include,
        tidepool_harness::timing::NO_NODE,
        tidepool_harness::timing::NO_ROUND,
    );
    // `CompiledTurn` doesn't implement `Debug` (it carries a JIT-relevant
    // `CoreExpr`/`DataConTable`, not meant for dumping) — match on the error
    // side only, which does derive `Debug`.
    if let Err(e) = &good {
        panic!("control: targetA alone should compile cleanly, got {e:?}");
    }

    let result = engine::compile_turns(
        &cfg.extract_bin,
        &source,
        &["targetA", "nonexistentTarget"],
        &target.include,
        tidepool_harness::timing::NO_NODE,
        tidepool_harness::timing::NO_ROUND,
    );
    match result {
        // A bogus `--targets` name is a GHC "not in scope" error, read
        // through the same structured diagnostics contract as any other
        // extract failure (architecture review finding 3: the harness's
        // former opaque `CompileError::Extract(String)` is gone — this is
        // strictly better fidelity, real spans instead of a raw dump).
        Err(CompileError::Diagnostics(_)) => {}
        Err(other) => {
            panic!("expected CompileError::Diagnostics for a bogus target, got {other:?}")
        }
        Ok(turns) => panic!(
            "a two-target request with one bogus target must FAIL the whole \
             extraction, not silently emit the good one — got Ok({:?})",
            turns.keys().collect::<Vec<_>>()
        ),
    }
}

/// **per-target asks stay distinct.** `targetA`'s `runLLMTurn @Int` site and
/// `targetB`'s `runLLMTurn @Text` site must NOT collapse into one shared
/// sidecar: each target's own `CompiledTurn::asks` records only its own site,
/// at its own type, end-to-end through `engine::compile_turns`. Both targets'
/// site counters independently start at 0 (a fresh `TransState` per
/// `translateModuleClosed` call, `Translate.hs`), so this also proves the two
/// targets' `asks.json` sidecars are read from genuinely SEPARATE files/maps
/// — a shared-map bug would have target B's site 0 silently overwrite (or be
/// overwritten by) target A's, and this test would see one type instead of
/// two.
#[test]
fn multi_target_asks_stay_distinct() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }
    let (cfg, source) = two_target_source();
    let target = cfg.turn_target(None).expect("turn target");

    let turns = engine::compile_turns(
        &cfg.extract_bin,
        &source,
        &["targetA", "targetB"],
        &target.include,
        tidepool_harness::timing::NO_NODE,
        tidepool_harness::timing::NO_ROUND,
    )
    .expect("two genuinely-valid targets should compile together");

    assert_eq!(
        turns.len(),
        2,
        "expected exactly one CompiledTurn per requested target"
    );
    let asks_a = &turns.get("targetA").expect("targetA present").asks;
    let asks_b = &turns.get("targetB").expect("targetB present").asks;

    assert_eq!(
        asks_a.len(),
        1,
        "targetA should record its one runLLMTurn site"
    );
    assert_eq!(
        asks_b.len(),
        1,
        "targetB should record its one runLLMTurn site"
    );

    let ty_a = asks_a
        .type_of(0)
        .expect("targetA's site 0 should be recorded");
    let ty_b = asks_b
        .type_of(0)
        .expect("targetB's site 0 should be recorded");

    assert!(
        ty_a.contains("Int"),
        "targetA's site should be typed Int, got {ty_a:?}"
    );
    assert!(
        ty_b.contains("Text"),
        "targetB's site should be typed Text, got {ty_b:?}"
    );
    assert_ne!(
        ty_a, ty_b,
        "targetA and targetB's DIFFERENT runLLMTurn sites must not collapse \
         to the same recorded type — that would mean one target's asks \
         sidecar silently won over the other's"
    );
}
