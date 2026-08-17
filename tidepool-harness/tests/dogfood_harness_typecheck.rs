//! The authored dogfood harnesses in `harness-dogfooding/` typecheck against
//! today's generated effect surface.
//!
//! THE HAZARD. These are Haskell sources the driver loads at RUNTIME, not
//! cargo targets — nothing else in the workspace compiles them, so `cargo
//! check`, clippy, and the whole Rust suite are all blind to them. An authored
//! harness can therefore drift arbitrarily far from the effect surface it
//! calls (a verb that no longer exists, a record field renamed, a LOCKED
//! signature quietly changed, a sum arm the generic-JSON derive rejects) and
//! stay green indefinitely. `examples/harness/` is covered by ~8 driver tests;
//! without this file the `harness-dogfooding/` harnesses are covered by none.
//!
//! COST. Two extract compiles, one per harness, because the two need
//! DIFFERENT effect rows and a row is what a compile is parameterized by —
//! there is no bundling that avoids the second. Both live in the GHC-heavy
//! tier (`.config/nextest.toml`'s default-filter excludes
//! `package(tidepool-harness) & kind(test)` wholesale).
//!
//! SCOPE. Typecheck only. These probes force GHC through `render` and `loop`
//! — plus `resumeLoop` and the pure resume decisions for a harness that
//! declares them; they do not drive a model, spawn an agent, or touch a
//! repository.

mod support;

use std::path::PathBuf;
use tidepool_harness::compile::compile_turn;
use tidepool_harness::engine::EngineConfig;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

/// Compile a probe module that imports the harness at `harness_dir` against
/// `decls`, forcing GHC through both locked entry points (`render` and
/// `loop`). Importing the module is already enough to typecheck every
/// declaration in it — GHC compiles a module whole — but naming both keeps
/// the probe honest about which contract is under test.
///
/// `extra_imports` and `extra_decls` are spliced into the probe's import block
/// and body respectively, for a harness that declares MORE than the universal
/// contract (dev-tree's `resumeLoop` and its pure resume decisions).
fn typecheck(
    harness_dir: &str,
    decls: Vec<tidepool_mcp::EffectDecl>,
    extra_imports: &str,
    extra_decls: &str,
) {
    support::require_extract();
    let _cache_guard = support::isolate_cache();
    let cfg = EngineConfig::from_decls(
        decls,
        repo_root().join("haskell/lib"),
        Some(repo_root().join(harness_dir)),
    )
    .expect("engine config for the dogfood row");
    let source = format!(
        concat!(
            "{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, ",
            "FlexibleContexts, GADTs, ScopedTypeVariables, TypeApplications, LambdaCase, ",
            "RecordWildCards, OverloadedRecordDot, QuasiQuotes, DeriveGeneric, DeriveAnyClass #-}}\n",
            "module Probe where\n",
            "import Tidepool.Prelude hiding (render)\n",
            "import Tidepool.Effects\n",
            "import Harness\n",
            "{extra_imports}",
            "__probe :: M Text\n",
            "__probe = do {{ st <- loop initialState; pure (render st) }}\n",
            "{extra_decls}",
        ),
        extra_imports = extra_imports,
        extra_decls = extra_decls,
    );
    if let Err(e) = compile_turn(&cfg.extract_bin, &source, "__probe", &cfg.include, 0, 0) {
        panic!("{harness_dir} does not typecheck against today's surface:\n{e}");
    }
}

/// The companion's row is the driver's own outer session: `[RunLLMTurn,
/// AskUser, Worktree, Subagent]` (mirrors `selfharness::driver::outer_decls`
/// — interposed effects FIRST so the suspend threshold stays 0; Worktree is
/// Subagent's hard companion). Its `loop` calls only `runLLMTurn` today —
/// conversation happens through the answerer's own `askUser` forms — but the
/// probe compiles against the full outer row the driver actually serves.
#[test]
fn companion_typechecks() {
    typecheck(
        "harness-dogfooding/companion",
        vec![
            tidepool_mcp::runllmturn_decl(),
            tidepool_mcp::askuser_decl(),
            tidepool_mcp::worktree_decl(),
            tidepool_mcp::subagent_decl(),
        ],
        "",
        "",
    );
}

/// dev-tree is the executable design target of S1-L1: its row now IS the
/// driver's widened outer session (`[RunLLMTurn, AskUser, Console, Worktree,
/// RepoEvent, Exec, Subagent, Journal]` — mirrors
/// `selfharness::driver::outer_decls`, interposed effects FIRST so the
/// suspend threshold stays 0). Typechecking against that exact row is what
/// turns this from "the file names only landed API" into "this compiles
/// against what the driver actually serves".
///
/// dev-tree also declares the OPT-IN second entry point (PRD 20 S1-L5's
/// `resumeLoop :: ResumeFold -> State -> Harness State`), so the probe names
/// both entries: a fresh boot compiles `loop`, a boot with a folded journal
/// compiles `resumeLoop`, and the driver picks between them. Naming
/// `resumeLoop` at its exact declared signature is what makes a drift in
/// either half a compile failure here rather than a boot refusal in
/// production.
///
/// The two pure resume decisions are named at their signatures for the same
/// reason the coalgebra's pure policy slots are pure: they decide from the
/// fold alone, with no agent and no git anywhere in the path, so they are
/// callable directly.
#[test]
fn dev_tree_typechecks() {
    typecheck(
        "harness-dogfooding/dev-tree",
        vec![
            tidepool_mcp::runllmturn_decl(),
            tidepool_mcp::askuser_decl(),
            tidepool_mcp::console_decl(),
            tidepool_mcp::worktree_decl(),
            tidepool_mcp::event_decl(),
            tidepool_mcp::exec_decl(),
            tidepool_mcp::subagent_decl(),
            tidepool_mcp::journal_decl(),
        ],
        "import Tidepool.Resume (ResumeFold, emptyResume)\n",
        concat!(
            "__resumeProbe :: M Text\n",
            "__resumeProbe = do { st <- resumeLoop emptyResume initialState; pure (render st) }\n",
            "__resumeDecision :: ResumeFold -> Text -> DevPlan -> ResumePlan\n",
            "__resumeDecision = resumePlanFor\n",
            "__amendmentNewest :: Maybe Int -> Maybe Int -> Maybe Int -> Bool\n",
            "__amendmentNewest = amendmentIsNewest\n",
        ),
    );
}
