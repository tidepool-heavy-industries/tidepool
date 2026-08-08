//! Structural proof of the harness/agent effect-stack split: the OUTER
//! self-iterating-harness session compiles against `Eff '[RunLLMTurn]`
//! ([`tidepool_harness::selfharness::driver`]'s private `outer_decls`) and
//! the nested ANSWERER turn compiles against `Eff '[AskUser, RunLLMTurn,
//! Finalize]` ([`answerer_decls`]) — decl lists that share `RunLLMTurn` (the
//! fork surface, see below) but are otherwise disjoint (`AskUser` is
//! answerer-only, `Ask` is general-Agent-only), not a single shared stack
//! pinned by convention. `tidepool_mcp::effects_module_source` only emits an
//! effect's GADT + Member-polymorphic helpers for decls actually passed into a
//! given compile, so an effect absent from a compile's decl list is UNDECLARED
//! there, not merely unreachable — calling it is a GHC "not in scope" error, a
//! stronger wall than a solvable-elsewhere type mismatch.
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
//! `RunLLMTurn` is back in the answerer's row (self-iterating-harness fork
//! widen) because `fork`/`forkAll` (`Tidepool.Fork`) extract-head-swap to the
//! same `RunLLMTurn` GADT every other `runLLMTurn` call compiles to — see
//! [`answerer_decls`]'s doc for the full account of why this is controlled,
//! bounded recursion rather than a reopening of unrestricted model-spawning.
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

/// THE answerer-side structural guarantee, UPDATED for the fork widen:
/// `runLLMTurn @T` now DOES typecheck against the answerer's `Eff '[Ask,
/// RunLLMTurn, Finalize]` stack — `RunLLMTurn` is declared there specifically
/// so `fork`/`forkAll` (which head-swap to it) compile. This is no longer a
/// "no recursive model-spawning" wall; that guarantee now lives at the
/// DRIVER level (only a `Fork`-routed suspension is serviced, via
/// `answer_fanout`/`answer_fork`; a bare in-context `runLLMTurn` resume is
/// not something the answerer's framing ever prompts for, though it would
/// compile).
#[test]
fn run_llm_turn_compiles_in_the_answerer_stack() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }

    let result = compile_against(answerer_decls(), "(runLLMTurn @Int \"go\" :: M Int)", "");
    assert!(
        result.is_ok(),
        "runLLMTurn must compile against the answerer stack '[Ask, RunLLMTurn, \
         Finalize] (RunLLMTurn is in the row, for fork/forkAll), got:\n{:?}",
        result.err()
    );
}

/// `finalize @T x` — the answerer's terminal answer path — still compiles
/// against the answerer stack.
#[test]
fn finalize_compiles_in_the_answerer_stack() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }

    let result = compile_against(answerer_decls(), "(finalize @Int (41 + 1) :: M ())", "");
    assert!(
        result.is_ok(),
        "finalize @Int must compile against the answerer stack '[Ask, RunLLMTurn, \
         Finalize] (Finalize is in the row), got:\n{:?}",
        result.err()
    );
}

/// The gui path — `askUserRaw` over `AskUser` — also compiles against the
/// answerer stack (`AskUser` is in the row). Proves the answerer keeps its
/// intended surface (present a typed form to a human operator, fork out to
/// sub-answerers, then finalize).
#[test]
fn askuser_raw_compiles_in_the_answerer_stack() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }

    let result = compile_against(answerer_decls(), "askUserRaw (toJSON (0 :: Int))", "");
    assert!(
        result.is_ok(),
        "askUserRaw (an AskUser verb) must compile against the answerer stack \
         '[AskUser, RunLLMTurn, Finalize] (AskUser is in the row), got:\n{:?}",
        result.err()
    );
}

/// `forkAll @T` (`Tidepool.Fork`) also compiles against the answerer stack —
/// the primitive the answerer's framing advertises for parallel
/// sub-answerer delegation. It head-swaps to the `RunLLMTurn` GADT, so this is
/// exactly what `RunLLMTurn` is in the row for. (The singular `fork` sibling
/// rides the same arm and is exercised by `acceptance_fork`.)
#[test]
fn fork_all_compiles_in_the_answerer_stack() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }

    let result = compile_against(
        answerer_decls(),
        "(forkAll @Int [\"pick a number\"] :: M [Int])",
        "Tidepool.Fork (forkAll)",
    );
    assert!(
        result.is_ok(),
        "forkAll @Int must compile against the answerer stack '[AskUser, RunLLMTurn, \
         Finalize] (RunLLMTurn is in the row), got:\n{:?}",
        result.err()
    );
}

/// The general Agent's `Ask` effect verb — `ask` — does NOT typecheck against
/// the answerer's `Eff '[AskUser, RunLLMTurn, Finalize]` stack: `Ask` is a
/// DIFFERENT effect (still present on the general Agent stack, suspending to
/// the calling LLM agent) and is not declared in this narrower compile at all.
/// A base effect (`Fs`/`Exec`/…) is rejected the same way — none are in the
/// row — which is the capability boundary the scoped stack exists to enforce.
#[test]
fn ask_is_a_compile_error_in_the_answerer_stack() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }

    let result = compile_against(answerer_decls(), "(ask SStr \"x\" :: M Value)", "");
    let err = match result {
        Ok(_) => panic!(
            "ask compiled against the answerer stack '[AskUser, RunLLMTurn, Finalize] \
             — the structural scoping is BROKEN (Ask must be undeclared there)"
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("ask") && err.contains("not in scope"),
        "expected a GHC not-in-scope error naming ask, got:\n{err}"
    );
}

/// The capability boundary proper: a BASE effect verb (`httpGet`, the `Http`
/// effect — representative of the nine base effects the answerer row drops:
/// `Console`/`KV`/`Fs`/`Lsp`/`Http`/`Exec`/`Git`/`Time`/`Meta`) does NOT
/// typecheck against `Eff '[AskUser, RunLLMTurn, Finalize]`. This is the whole
/// point of the scoped stack — the answerer structurally cannot hit the
/// network, run a shell command, or read files, because those verbs are
/// UNDECLARED in its compile, not merely unreachable.
#[test]
fn base_effect_is_a_compile_error_in_the_answerer_stack() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT)");
        return;
    }

    let result = compile_against(answerer_decls(), "(httpGet \"http://x\" :: M Value)", "");
    let err = match result {
        Ok(_) => panic!(
            "httpGet (a base Http effect) compiled against the answerer stack \
             '[AskUser, RunLLMTurn, Finalize] — the capability boundary is BROKEN \
             (base effects must be undeclared there)"
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("httpGet") && err.contains("not in scope"),
        "expected a GHC not-in-scope error naming httpGet, got:\n{err}"
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
