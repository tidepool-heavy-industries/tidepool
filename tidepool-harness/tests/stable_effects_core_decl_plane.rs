//! STABLE-EFFECTS-CORE acceptance: the shared decl plane's validation
//! surface directly — no `SelfHarnessDriver`/replay/model machinery at all.
//!
//! `SelfHarnessDriver::open_outer_plane` (private) opens a `SessionLib` with
//! `tidepool_mcp::pure_decl_module_env()`, validated against
//! `EngineConfig::validation_include()`. These tests replicate exactly that
//! construction and drive `SessionLib::define`/`define_scoped_in` directly,
//! proving the actual mechanism this branch changes:
//!
//! - a declaration written `Member <Eff> effs => ... -> Eff effs T` VALIDATES
//!   (Core is on the validation include) and PERSISTS (a LATER declaration
//!   can call it, in the SAME plane instance a real cross-window plane would
//!   be) — the original stable-effects-core payoff;
//! - a declaration spelling the per-window `M` alias now GENERALIZES instead
//!   of failing: `SessionLib`/`render_module` strips the M-mentioning
//!   signature before this plane compiles it (the shim is still excluded —
//!   `M` never resolves here), letting GHC infer the same
//!   `Member <Eff> effs => ... -> Eff effs T` shape a hand-written
//!   row-polymorphic signature would have — so `M`-spelled and
//!   `Member`-spelled declarations now persist identically, in a later
//!   window AND in a different scope of the same tree;
//! - an unannotated declaration (no signature at all) validates the same way
//!   — this was already true, pinned here as the third baseline shape;
//! - a helper whose Member constraint a LATER, concretely-row-pinned
//!   declaration cannot satisfy fails AT THAT USE SITE with an ordinary
//!   unsolved-`Member` error, never at the helper's own define time — the
//!   row itself still does not cross plane compiles, only its declaration
//!   text does;
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

use tidepool_codegen::scope::ScopeId;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::typed_request_agent_decls;
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
    let cfg = EngineConfig::from_decls(typed_request_agent_decls(), prelude_dir(), None)
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

/// `M` now carries forward cleanly: a declaration spelling the per-window
/// `M` alias validates (its signature is stripped before this plane
/// compiles it, so GHC infers the general type from the body — `M` itself
/// still never resolves here, the shim stays excluded) and PERSISTS exactly
/// like a hand-written `Member`-form declaration — a later declaration on
/// the SAME plane instance can call it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m_form_effectful_helper_validates_and_persists() {
    support::require_extract();
    let cfg = EngineConfig::from_decls(typed_request_agent_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let mut plane = open_plane(&cfg, "m-form");

    plane
        .define("probeStateM :: M Int\nprobeStateM = getStateJson >> pure 0")
        .expect(
            "an M-typed declaration must now validate — its M-mentioning signature is \
             stripped before this plane compiles it, letting GHC infer the general type",
        );

    // A LATER declaration calling the first, on the SAME plane instance —
    // the exact property `member_form_effectful_helper_validates_and_a_later_decl_calls_it`
    // pins for the hand-written Member form, now true for the M-spelled form too.
    plane
        .define("probeStateMDoubled :: M Int\nprobeStateMDoubled = fmap (* 2) probeStateM")
        .expect("a later M-typed declaration must see `probeStateM` as a live name");
}

/// [`m_form_effectful_helper_validates_and_persists`]'s cross-window claim,
/// pinned two ways: a LATER declaration on the same (ROOT) scope, and a
/// declaration in a DIFFERENT scope of the same tree — the shape a real
/// child window minted off this plane would see (`PersistentSession::mint_scope`
/// seeds a child's decl-plane tip from its parent's at mint time; this test
/// reproduces that seeding directly via `seed_scope` without the session
/// machinery, since the decl plane is the only thing under test here).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m_form_effectful_helper_visible_in_a_later_window_and_a_sibling_scope() {
    support::require_extract();
    let cfg = EngineConfig::from_decls(typed_request_agent_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let mut plane = open_plane(&cfg, "m-form-cross-window");

    plane
        .define("announceM :: M Text\nannounceM = fmap renderJsonCounter getStateJson\n  where renderJsonCounter v = case v ^? key \"counter\" . _Int of { Just n -> show n; Nothing -> \"none\" }")
        .expect("M-typed declaration must validate on the plane");

    // A later window on the SAME (ROOT) scope.
    plane
        .define("announceMTwice :: M Text\nannounceMTwice = fmap (<> \"!\") announceM")
        .expect("a later window on the same scope must see `announceM`");

    // A different window of the same TREE: a child scope minted after
    // `announceM` was declared, seeded from ROOT's tip.
    let child = ScopeId(1);
    plane.seed_scope(child, plane.scope_tip(ScopeId::ROOT));
    plane
        .define_scoped_in(
            child,
            "announceMFromChild :: M Text\nannounceMFromChild = fmap (<> \"?\") announceM",
        )
        .expect("a child scope minted after `announceM` was declared must see it too");
}

/// An unannotated declaration — no signature at all — is a third baseline
/// shape, unaffected by the M-generalization machinery (there is no
/// signature to strip): GHC infers its type from the body exactly as it
/// already did.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unannotated_effectful_helper_still_validates_and_persists() {
    support::require_extract();
    let cfg = EngineConfig::from_decls(typed_request_agent_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let mut plane = open_plane(&cfg, "unannotated-form");

    plane
        .define("probeUnannotated = getStateJson")
        .expect("an unannotated effectful declaration must validate");
    plane
        .define("probeUnannotatedAgain = fmap (const ()) probeUnannotated")
        .expect("a later declaration must see the unannotated one");
}

/// The consequence generalization must NOT paper over: the row itself still
/// does not cross a plane compile. A helper validated here (Core carries the
/// WHOLE vocabulary, independent of any one turn's row) fails only when a
/// LATER declaration pins a CONCRETE row that genuinely lacks the effect it
/// needs — an ordinary unsolved-`Member` error at THAT use site, never at
/// the helper's own define time. This is the comprehensible failure mode
/// `eval_prep`'s docs describe; generalizing the signature must not turn it
/// into a silent success or a define-time refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn helper_missing_from_a_later_narrow_row_fails_at_use_not_define() {
    support::require_extract();
    let cfg = EngineConfig::from_decls(typed_request_agent_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let mut plane = open_plane(&cfg, "use-site-member-failure");

    // Defines cleanly: Core carries ReadState regardless of any one row.
    plane
        .define("needsReadStateM :: M Value\nneedsReadStateM = getStateJson")
        .expect("an M-typed declaration needing ReadState must still validate at define time");

    // A later declaration pinning a CONCRETE row that omits ReadState must
    // fail HERE, at this use site — not retroactively at the first define.
    let err = plane
        .define("pinnedNarrowRow :: Eff '[Fork] Value\npinnedNarrowRow = needsReadStateM")
        .expect_err(
            "a concretely-row-pinned caller lacking ReadState must fail to unify the \
             Member constraint",
        );
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("member") || msg.contains("no instance"),
        "expected an unsolved-Member error naming the missing effect, got: {err}"
    );
}

/// An ordinary PURE declaration (no effect surface at all) is unaffected by
/// any of this — the baseline the whole plane mechanism must never regress.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pure_helper_still_validates_and_persists() {
    support::require_extract();
    let cfg = EngineConfig::from_decls(typed_request_agent_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let mut plane = open_plane(&cfg, "pure-form");

    plane
        .define("bumpTwice :: Int -> Int\nbumpTwice n = n + 2")
        .expect("a plain pure declaration must still validate");
    plane
        .define("bumpFour :: Int -> Int\nbumpFour = bumpTwice . bumpTwice")
        .expect("a later pure declaration must still see an earlier one");
}
