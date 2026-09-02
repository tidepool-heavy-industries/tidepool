//! Direct acceptance tests for the shared declaration plane used by the
//! self-harness. They cover row-polymorphic, inferred, invalid-capability, and
//! pure declarations without provider/replay machinery.
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).

mod support;

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

/// The declaration plane compiles signatures verbatim. The per-window `M`
/// alias is intentionally absent, so authored code must state a stable
/// row-polymorphic contract instead of relying on source rewriting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_window_m_alias_is_rejected_without_rewriting() {
    support::require_extract();
    let cfg = EngineConfig::from_decls(typed_request_agent_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let mut plane = open_plane(&cfg, "m-form");

    let error = plane
        .define("probeStateM :: M Int\nprobeStateM = getStateJson >> pure 0")
        .expect_err("the declaration plane must not erase an invalid signature");
    assert!(
        error.to_string().contains("M"),
        "the GHC diagnostic should identify the unavailable alias: {error}"
    );
}

/// An unannotated declaration — no signature at all — is a third baseline
/// shape: GHC infers its type from the body.
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

/// A row-polymorphic helper validates against Core's stable vocabulary, while
/// a later declaration that pins a concrete row without the required effect
/// fails at the use site with an ordinary unsolved-`Member` error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn helper_missing_from_a_later_narrow_row_fails_at_use_not_define() {
    support::require_extract();
    let cfg = EngineConfig::from_decls(typed_request_agent_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let mut plane = open_plane(&cfg, "use-site-member-failure");

    // Defines cleanly: Core carries ReadState regardless of any one row.
    plane
        .define(
            "needsReadState :: forall effs. Member ReadState effs => Eff effs Value\n\
             needsReadState = getStateJson",
        )
        .expect("a Member-constrained declaration must validate at define time");

    // A later declaration pinning a CONCRETE row that omits ReadState must
    // fail HERE, at this use site — not retroactively at the first define.
    let err = plane
        .define("pinnedNarrowRow :: Eff '[Fork] Value\npinnedNarrowRow = needsReadState")
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
