//! Structural proof of the harness/agent effect-stack split: the OUTER
//! self-iterating-harness session compiles against `Eff '[RunLLMTurn, AskUser]`
//! and the nested ANSWERER turn compiles against `Eff '[AskUser, Fork, ReadState,
//! Finalize]` ([`answerer_decls`]) — disjoint ROWS (`Fork`/`AskUser` are
//! answerer-only; `RunLLMTurn`/`Ask` are not in the answerer's row), not a
//! single shared stack pinned by convention. `type M` is built from a
//! compile's ROW alone and stays exactly this narrow.
//!
//! **`RunLLMTurn` is the one exception to "absent ⇒ not in scope" (extract-wave
//! item 0b).** Its GADT + `Member`-polymorphic helpers are NAMEABLE in every
//! compile (`EngineConfig`'s vocabulary policy widens `row ∪ {RunLLMTurn}`),
//! so `runLLMTurn` resolves and typechecks up to the `Member RunLLMTurn effs`
//! constraint — which then fails to SOLVE against a row that doesn't carry
//! it, a comprehensible `Member` error, not "not in scope". Every OTHER
//! effect (`Ask`, base effects, …) keeps the old behavior: absent from a
//! compile's decl list means UNDECLARED there, so calling it is a GHC "not in
//! scope" error, a stronger wall than a solvable-elsewhere type mismatch.
//!
//! The answerer forks via its OWN `Fork` effect (`Tidepool.Fork`'s
//! `fork`/`forkAll` head-swap to `forkSited`/`forkAllSited`), not through
//! `RunLLMTurn` — so `runLLMTurn` is NOT in its row (a bare `runLLMTurn` call
//! there is a compile error). A forked CHILD compiles against a fork-free leaf
//! row (`[AskUser, Finalize]`), so it cannot itself fork — depth-one is
//! structural.
//!
//! These tests compile a turn's Haskell straight through `tidepool-extract`
//! (the same path a live turn takes) and assert on the compile OUTCOME.
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH (run inside
//! `nix develop`; see `haskell/CLAUDE.md`).

mod support;

use tidepool_harness::engine::{self, template_turn_for, CompiledTurn, EngineConfig};
use tidepool_harness::selfharness::answerer_decls;
use tidepool_runtime::CompileError;

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
) -> Result<CompiledTurn, CompileError> {
    let cfg = EngineConfig::from_decls(decls, prelude_dir(), None).expect("engine config");
    let target = cfg.turn_target(None).expect("turn target");
    let source = template_turn_for(&cfg.decls, &target.stack, code, imports, "");
    engine::compile_turn(
        &cfg.extract_bin,
        &source,
        "result",
        &target.include,
        tidepool_harness::timing::NO_NODE,
        tidepool_harness::timing::NO_ROUND,
    )
}

/// Like [`compile_against`], but with the row's `Finalize` entry instantiated
/// at `finalize_ty` — `Finalize` is type-indexed, so a block calling `finalize
/// @T` needs the row to name `T` (`Finalize NoAnswer`, the default, is
/// uninhabited by design).
fn compile_pinned(
    decls: Vec<tidepool_mcp::EffectDecl>,
    code: &str,
    imports: &str,
    finalize_ty: &str,
) -> Result<CompiledTurn, CompileError> {
    let cfg = EngineConfig::from_decls(decls, prelude_dir(), None).expect("engine config");
    let target = cfg
        .turn_target(Some((finalize_ty, &[])))
        .expect("turn target");
    let source = template_turn_for(&cfg.decls, &target.stack, code, imports, "");
    engine::compile_turn(
        &cfg.extract_bin,
        &source,
        "result",
        &target.include,
        tidepool_harness::timing::NO_NODE,
        tidepool_harness::timing::NO_ROUND,
    )
}

/// THE answerer-side structural guarantee (extract-wave item 0b): `runLLMTurn
/// @T` does NOT typecheck against the answerer's `Eff '[AskUser, Fork, ReadState,
/// Finalize]` ROW — `RunLLMTurn` is not in it. But `RunLLMTurn` IS in the
/// answerer compile's VOCABULARY now (nameable everywhere), so the failure
/// mode changed: it is no longer "not in scope" (undeclared) but a `Member`
/// error (declared, but the row can't supply the constraint) — a
/// comprehensible capability-boundary error, not a typo-shaped one. The
/// answerer's parallel-delegation surface is the `Fork` effect (`fork`/
/// `forkAll`), not `runLLMTurn`, so an answerer still cannot suspend an
/// in-context model turn — it forks (driver-serviced, depth-one) or
/// finalizes.
#[test]
fn run_llm_turn_is_a_member_error_not_a_scope_error_in_the_answerer_stack() {
    support::require_extract();

    let result = compile_against(answerer_decls(), "(runLLMTurn @Int \"go\" :: M Int)", "");
    let err = match result {
        Ok(_) => panic!(
            "runLLMTurn compiled against the answerer stack '[AskUser, Fork, ReadState, Finalize] \
             — RunLLMTurn must not be IN THE ROW there (the answerer forks, it does not \
             suspend an in-context model turn)"
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("Member") && err.contains("RunLLMTurn"),
        "expected a GHC Member error naming RunLLMTurn (nameable, but not in \
         the answerer's row), got:\n{err}"
    );
    let lower = err.to_lowercase();
    assert!(
        !lower.contains("not in scope") && !lower.contains("variable not in scope"),
        "runLLMTurn is NAMEABLE in the answerer compile now (extract-wave item \
         0b) — a scope error here means the vocabulary widening regressed, \
         got:\n{err}"
    );
}

/// The POSITIVE control for item 0b: `Tidepool.Harness` (which re-exports `M`,
/// `RunLLMTurn`, and `runLLMTurn` from `Tidepool.Effects`) is IMPORTABLE
/// against the answerer stack — the import alone proves `RunLLMTurn` has a
/// real GADT in the answerer's generated `Tidepool.Effects`, independent of
/// whether the eval code actually calls a `RunLLMTurn` verb (this one calls
/// `askUserRaw`, an ordinary in-row verb).
#[test]
fn tidepool_harness_module_is_importable_in_the_answerer_stack() {
    support::require_extract();

    let result = compile_against(
        answerer_decls(),
        "askUserRaw (toJSON (0 :: Int))",
        "Tidepool.Harness",
    );
    assert!(
        result.is_ok(),
        "importing Tidepool.Harness (which names RunLLMTurn/runLLMTurn) must \
         typecheck against the answerer stack now that RunLLMTurn is nameable \
         everywhere (extract-wave item 0b), got:\n{:?}",
        result.err()
    );
}

/// A forked CHILD compiles against the fork-free leaf row `[AskUser, Finalize]`
/// (`Harness::child_cfg` drops `Fork`/`RunLLMTurn` from the parent row), so a
/// `forkAll` in a child block is a GHC "not in scope" error — depth-one is
/// structural, not a runtime guard. `finalize` still compiles there (a child
/// answers directly).
#[test]
fn fork_child_leaf_row_cannot_fork() {
    support::require_extract();

    // The leaf child row: the answerer row minus the fork-spawning effects.
    let leaf = vec![tidepool_mcp::askuser_decl(), tidepool_mcp::finalize_decl()];

    let forked = compile_against(
        leaf.clone(),
        "(forkAll @Int [\"pick a number\"] :: M [Int])",
        "Tidepool.Fork (forkAll)",
    );
    let err = match forked {
        Ok(_) => panic!(
            "forkAll compiled against the fork-child leaf row '[AskUser, Finalize] — \
             a fork child must NOT be able to fork (depth-one is structural)"
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("not in scope") || err.contains("forkAll") || err.contains("Fork"),
        "expected a GHC not-in-scope error for forkAll on the leaf row, got:\n{err}"
    );

    // Positive control: finalize still compiles on the leaf row — a child
    // answers its own brief directly.
    let fin = compile_pinned(leaf, "(finalize @Int 1 :: M ())", "", "Int");
    assert!(
        fin.is_ok(),
        "finalize must compile against the fork-child leaf row '[AskUser, Finalize], \
         got:\n{:?}",
        fin.err()
    );
}

/// `finalize @T x` — the answerer's terminal answer path — still compiles
/// against the answerer stack.
#[test]
fn finalize_compiles_in_the_answerer_stack() {
    support::require_extract();

    let result = compile_pinned(
        answerer_decls(),
        "(finalize @Int (41 + 1) :: M ())",
        "",
        "Int",
    );
    assert!(
        result.is_ok(),
        "finalize @Int must compile against the answerer stack '[AskUser, Fork, \
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
    support::require_extract();

    let result = compile_against(answerer_decls(), "askUserRaw (toJSON (0 :: Int))", "");
    assert!(
        result.is_ok(),
        "askUserRaw (an AskUser verb) must compile against the answerer stack \
         '[AskUser, Fork, ReadState, Finalize] (AskUser is in the row), got:\n{:?}",
        result.err()
    );
}

/// `forkAll @T` (`Tidepool.Fork`) compiles against the answerer stack — the
/// primitive the answerer's framing advertises for parallel sub-answerer
/// delegation. It head-swaps to the `Fork` GADT's `forkAllSited`, which is
/// exactly what `Fork` is in the row for. (The singular `fork` sibling rides
/// the same arm and is exercised by `acceptance_fork`.)
#[test]
fn fork_all_compiles_in_the_answerer_stack() {
    support::require_extract();

    let result = compile_against(
        answerer_decls(),
        "(forkAll @Int [\"pick a number\"] :: M [Int])",
        "Tidepool.Fork (forkAll)",
    );
    assert!(
        result.is_ok(),
        "forkAll @Int must compile against the answerer stack '[AskUser, Fork, \
         Finalize] (Fork is in the row), got:\n{:?}",
        result.err()
    );
}

/// The general Agent's `Ask` effect verb — `ask` — does NOT typecheck against
/// the answerer's `Eff '[AskUser, Fork, ReadState, Finalize]` stack: `Ask` is a
/// DIFFERENT effect (still present on the general Agent stack, suspending to
/// the calling LLM agent) and is not declared in this narrower compile at all.
/// A base effect (`Fs`/`Exec`/…) is rejected the same way — none are in the
/// row — which is the capability boundary the scoped stack exists to enforce.
#[test]
fn ask_is_a_compile_error_in_the_answerer_stack() {
    support::require_extract();

    let result = compile_against(answerer_decls(), "(ask SStr \"x\" :: M Value)", "");
    let err = match result {
        Ok(_) => panic!(
            "ask compiled against the answerer stack '[AskUser, Fork, ReadState, Finalize] \
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
/// `Console`/`KV`/`Fs`/`Lsp`/`Http`/`Exec`/`Git`/`Time`/`Llm`) does NOT
/// typecheck against `Eff '[AskUser, Fork, ReadState, Finalize]`. This is the whole
/// point of the scoped stack — the answerer structurally cannot hit the
/// network, run a shell command, or read files, because those verbs are
/// UNDECLARED in its compile, not merely unreachable.
#[test]
fn base_effect_is_a_compile_error_in_the_answerer_stack() {
    support::require_extract();

    let result = compile_against(answerer_decls(), "(httpGet \"http://x\" :: M Value)", "");
    let err = match result {
        Ok(_) => panic!(
            "httpGet (a base Http effect) compiled against the answerer stack \
             '[AskUser, Fork, ReadState, Finalize] — the capability boundary is BROKEN \
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
    support::require_extract();

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
    support::require_extract();

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
    support::require_extract();

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
