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
//! SCOPE. Typecheck only. These probes force GHC through `render` and `loop`;
//! they do not drive a model, spawn an agent, or touch a repository.

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
fn typecheck(harness_dir: &str, decls: Vec<tidepool_mcp::EffectDecl>) {
    support::require_extract();
    let _cache_guard = support::isolate_cache();
    let cfg = EngineConfig::from_decls(
        decls,
        repo_root().join("haskell/lib"),
        Some(repo_root().join(harness_dir)),
    )
    .expect("engine config for the dogfood row");
    let source = concat!(
        "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, ",
        "FlexibleContexts, GADTs, ScopedTypeVariables, TypeApplications, LambdaCase, ",
        "RecordWildCards, OverloadedRecordDot, QuasiQuotes, DeriveGeneric, DeriveAnyClass #-}\n",
        "module Probe where\n",
        "import Tidepool.Prelude hiding (render)\n",
        "import Tidepool.Effects\n",
        "import Harness\n",
        "__probe :: M Text\n",
        "__probe = do { st <- loop initialState; pure (render st) }\n",
    );
    if let Err(e) = compile_turn(&cfg.extract_bin, source, "__probe", &cfg.include, 0, 0) {
        panic!("{harness_dir} does not typecheck against today's surface:\n{e}");
    }
}

/// The companion's row is the driver's own outer session: `[RunLLMTurn,
/// AskUser]` (mirrors `selfharness::driver::outer_decls`). Its `loop` calls
/// only `runLLMTurn` — conversation happens through the answerer's own
/// `askUser` forms — but the probe compiles against the full outer row the
/// driver actually serves.
#[test]
fn companion_typechecks() {
    typecheck(
        "harness-dogfooding/companion",
        vec![
            tidepool_mcp::runllmturn_decl(),
            tidepool_mcp::askuser_decl(),
        ],
    );
}

/// dev-tree is a FORWARD dogfood: every name it calls exists today, but its
/// row is wider than the driver's v1 outer session composes. This test pins
/// the half that is real — that the file names only landed API — by handing it
/// the row it documents in its own module haddock. When the driver's outer row
/// widens, this test's decl list is what it widens to.
#[test]
fn dev_tree_typechecks() {
    typecheck(
        "harness-dogfooding/dev-tree",
        vec![
            tidepool_mcp::console_decl(),
            tidepool_mcp::worktree_decl(),
            tidepool_mcp::event_decl(),
            tidepool_mcp::subagent_decl(),
            tidepool_mcp::runllmturn_decl(),
        ],
    );
}
