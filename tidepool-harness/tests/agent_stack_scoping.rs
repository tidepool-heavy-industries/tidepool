//! Structural proof of the harness/agent effect-stack split: the OUTER
//! self-iterating-harness session compiles against `Eff '[RunLLMTurn]`
//! ([`tidepool_harness::selfharness::driver`]'s private `outer_decls`) and
//! the nested ANSWERER turn compiles against `Eff '[Ask, Finalize]`
//! ([`answerer_decls`]) — two DISJOINT decl lists, not a shared stack pinned
//! by convention. `tidepool_mcp::effects_module_source` only emits an
//! effect's GADT + Member-polymorphic helpers for decls actually passed into
//! a given compile, so an effect absent from a compile's decl list is
//! UNDECLARED there, not merely unreachable — calling it is a GHC "not in
//! scope" error, a stronger wall than a solvable-elsewhere type mismatch.
//!
//! This is the clean mechanism the harness/agent structural split needed:
//! `runLLMTurn`/`finalize` (siteid-plugin) are `Member <Eff> effs =>`
//! polymorphic rather than hardcoded to one closed `M`, so scoping which
//! decls a turn compiles against is sufficient on its own — no ambient
//! single-effect-row pinning (the discarded wave1-structural mechanism) is
//! needed to keep `examples/harness/Harness.hs`'s `loop` (which DOES need
//! `RunLLMTurn`) resolvable: its answer types now live in a sibling
//! `HarnessTypes` module the answerer imports instead (see that module's
//! haddock).
//!
//! These tests compile a turn's Haskell straight through `tidepool-extract`
//! (the same path a live turn takes) and assert on the compile OUTCOME.
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH (run inside
//! `nix develop`; see `haskell/CLAUDE.md`).

use tidepool_harness::compile;
use tidepool_harness::engine::{template_turn_for, EngineConfig};
use tidepool_harness::selfharness::answerer_decls;

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn prelude_dir() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| std::path::PathBuf::from("haskell/lib"))
}

/// The self-iterating harness's OUTER decl list — mirrors the private
/// `outer_decls` in `tidepool_harness::selfharness::driver` (kept private
/// there; duplicated here rather than made `pub` purely for test reach,
/// since it's a one-line literal: `Eff '[RunLLMTurn]`, no base effects,
/// 02-runtime.md LOCKED).
fn harness_only_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![tidepool_mcp::runllmturn_decl()]
}

fn compile_against(
    decls: Vec<tidepool_mcp::EffectDecl>,
    code: &str,
    imports: &str,
) -> Result<compile::CompiledTurn, compile::CompileError> {
    let cfg = EngineConfig::from_decls(decls, prelude_dir(), None).expect("engine config");
    let source = template_turn_for(&cfg.decls, &cfg, code, imports, "");
    compile::compile_turn(&cfg.extract_bin, &source, "result", &cfg.include)
}

/// THE answerer-side structural guarantee: `runLLMTurn @T` does not
/// typecheck against the answerer's `Eff '[Ask, Finalize]` stack, because
/// `RunLLMTurn` is not declared in that compile at all — no recursive
/// model-spawning is a property of what the compile is given to work with,
/// not of the prompt/framing.
#[test]
fn run_llm_turn_is_a_compile_error_in_the_answerer_stack() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }

    let result = compile_against(
        answerer_decls(),
        "(runLLMTurn @Int \"no recursion here\" :: M Int)",
        "",
    );

    let err = match result {
        Ok(_) => panic!(
            "runLLMTurn compiled against the answerer stack '[Ask, Finalize] — the \
             structural scoping is BROKEN (RunLLMTurn must be undeclared there)"
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("runLLMTurn") && err.contains("not in scope"),
        "expected a GHC not-in-scope error naming runLLMTurn, got:\n{err}"
    );
}

/// The other half: `finalize @T x` — the answerer's terminal answer path —
/// DOES compile against `Eff '[Ask, Finalize]`, so excluding `RunLLMTurn`
/// did not collateral-damage the answer path.
#[test]
fn finalize_compiles_in_the_answerer_stack() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }

    let result = compile_against(answerer_decls(), "(finalize @Int (41 + 1) :: M ())", "");
    assert!(
        result.is_ok(),
        "finalize @Int must compile against the answerer stack '[Ask, Finalize] \
         (Finalize is in the row), got:\n{:?}",
        result.err()
    );
}

/// The gui path — `dialogAsk` over `Ask` — also compiles against the
/// answerer stack (`Ask` is in the row). Proves the answerer keeps its full
/// intended surface (gather operator input, then finalize).
#[test]
fn dialog_ask_compiles_in_the_answerer_stack() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }

    let result = compile_against(
        answerer_decls(),
        "dialogAsk (textIn \"one rough spot?\" True)",
        "Tidepool.Ui",
    );
    assert!(
        result.is_ok(),
        "dialogAsk (an Ask verb) must compile against the answerer stack \
         '[Ask, Finalize] (Ask is in the row), got:\n{:?}",
        result.err()
    );
}

/// The SYMMETRIC harness-side guarantee: `ask`/`finalize` do not typecheck
/// against the outer harness's `Eff '[RunLLMTurn]`-only stack, because
/// `Ask`/`Finalize` are not declared in that compile at all.
#[test]
fn ask_is_a_compile_error_in_the_harness_stack() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }

    let result = compile_against(harness_only_decls(), "(ask SStr \"x\" :: M Value)", "");
    let err = match result {
        Ok(_) => panic!(
            "ask compiled against the harness stack '[RunLLMTurn] — the structural \
             scoping is BROKEN (Ask must be undeclared there)"
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("ask") && err.contains("not in scope"),
        "expected a GHC not-in-scope error naming ask, got:\n{err}"
    );
}

/// `finalize` is likewise rejected in the harness-only stack.
#[test]
fn finalize_is_a_compile_error_in_the_harness_stack() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }

    let result = compile_against(harness_only_decls(), "(finalize @Int 1 :: M ())", "");
    let err = match result {
        Ok(_) => panic!(
            "finalize compiled against the harness stack '[RunLLMTurn] — the structural \
             scoping is BROKEN (Finalize must be undeclared there)"
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("finalize") && err.contains("not in scope"),
        "expected a GHC not-in-scope error naming finalize, got:\n{err}"
    );
}

/// `runLLMTurn` DOES compile against the harness-only stack — the positive
/// control, so the reject tests above are proven against a real asymmetry
/// rather than a broken compile setup.
#[test]
fn run_llm_turn_compiles_in_the_harness_stack() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }

    let result = compile_against(
        harness_only_decls(),
        "(runLLMTurn @Int \"go\" :: M Int)",
        "",
    );
    assert!(
        result.is_ok(),
        "runLLMTurn must compile against the harness stack '[RunLLMTurn] \
         (RunLLMTurn is in the row), got:\n{:?}",
        result.err()
    );
}
