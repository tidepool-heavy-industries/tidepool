//! STABLE-EFFECTS-CORE acceptance: the shared decl plane's validation
//! surface directly — no `SelfHarnessDriver`/replay/model machinery at all.
//!
//! `SelfHarnessDriver::open_outer_plane` (private) opens a `SessionLib` with
//! `tidepool_mcp::pure_decl_module_env()`, validated against
//! `EngineConfig::validation_include()`. These tests replicate exactly that
//! construction and drive `SessionLib::define` directly, proving the actual
//! mechanism this branch changes:
//!
//! - a declaration written `Member <Eff> effs => ... -> Eff effs T` now
//!   VALIDATES (Core is on the validation include) and PERSISTS (a LATER
//!   declaration can call it, in the SAME plane instance a real cross-window
//!   plane would be) — the payoff feature;
//! - a declaration spelling the per-window `M` alias still FAILS validation
//!   (the shim is deliberately excluded) — the narrowed, not eliminated,
//!   structural guard;
//! - an ordinary pure declaration (no effect surface at all) is unaffected.
//!
//! A driver-cycle-level version of the first acceptance was attempted in
//! `selfharness_fn_finalize_spike.rs` and removed — it hit a PRE-EXISTING
//! failure reproduced on a clean baseline checkout, unrelated to this
//! branch's changes (see that file's note). This file is deliberately
//! independent of that pathway.
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).

mod support;

use tidepool_harness::answerer_decls;
use tidepool_harness::engine::EngineConfig;
use tidepool_runtime::session::SessionLib;

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> std::path::PathBuf {
    repo_root().join("haskell/lib")
}

/// Open a decl plane exactly the way `SelfHarnessDriver::open_outer_plane`
/// does, rooted at a fresh scratch dir so parallel tests never collide.
fn open_plane(cfg: &EngineConfig, label: &str) -> SessionLib {
    let root = std::env::temp_dir().join(format!(
        "stable-effects-core-decl-plane-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    SessionLib::open(
        tidepool_repr::SessionId(0),
        &root,
        tidepool_mcp::pure_decl_module_env(),
    )
    .expect("open decl plane")
    .with_validation_include(cfg.validation_include())
}

/// The payoff feature: a `Member`-constrained effectful declaration
/// validates on the plane, and a LATER declaration can call it — genuine
/// cross-declaration persistence, the exact shape a later real turn's
/// compile would also see (both draw on the SAME accumulated plane module).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn member_form_effectful_helper_validates_and_a_later_decl_calls_it() {
    support::require_extract();
    let cfg = EngineConfig::from_decls(answerer_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let mut plane = open_plane(&cfg, "member-form");

    plane
        .define(
            "probeState :: forall effs. Member ReadState effs => Eff effs Int\n\
             probeState = do\n  \
             v <- getStateJson\n  \
             pure (case v ^? key \"counter\" . _Int of { Just n -> n; Nothing -> -1 })",
        )
        .expect(
            "a Member-constrained effectful declaration must validate now that Core \
             (stable, not the per-window M shim) is on the validation include",
        );

    // A LATER declaration calling the first — proves the plane's own
    // cumulative module makes `probeState` a live name, not just "compiled
    // once and discarded": exactly the property a later turn/window depends
    // on.
    plane
        .define(
            "probeStateDoubled :: forall effs. Member ReadState effs => Eff effs Int\n\
             probeStateDoubled = fmap (* 2) probeState",
        )
        .expect("a later declaration must see `probeState` as a live name on the same plane");
}

/// The narrowed guard still fires: a declaration spelling the per-window `M`
/// alias (rather than a `Member`-polymorphic signature) fails validation,
/// because the shim — the only place `M` is declared — is deliberately
/// excluded from the plane's validation include. Not a regression: this is
/// the guard NARROWING, not disappearing (row capability enforcement is
/// untouched).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m_form_effectful_helper_still_fails_validation() {
    support::require_extract();
    let cfg = EngineConfig::from_decls(answerer_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let mut plane = open_plane(&cfg, "m-form");

    let err = plane
        .define("probeStateM :: M Int\nprobeStateM = getStateJson >> pure 0")
        .expect_err(
            "an M-typed declaration must still fail validation — the shim (where M lives) \
             is not on the plane's include path",
        );
    let msg = format!("{err}");
    assert!(
        msg.contains('M') || msg.to_lowercase().contains("not in scope"),
        "expected an ordinary GHC not-in-scope error naming `M`, got: {msg}"
    );
}

/// An ordinary PURE declaration (no effect surface at all) is unaffected by
/// any of this — the baseline the whole plane mechanism must never regress.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pure_helper_still_validates_and_persists() {
    support::require_extract();
    let cfg = EngineConfig::from_decls(answerer_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let mut plane = open_plane(&cfg, "pure-form");

    plane
        .define("bumpTwice :: Int -> Int\nbumpTwice n = n + 2")
        .expect("a plain pure declaration must still validate");
    plane
        .define("bumpFour :: Int -> Int\nbumpFour = bumpTwice . bumpTwice")
        .expect("a later pure declaration must still see an earlier one");
}
