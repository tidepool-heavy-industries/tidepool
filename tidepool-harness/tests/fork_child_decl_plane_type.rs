//! Crash-class regression: a fork child whose answer type was declared on
//! the SESSION's decl plane (a model-authored `type X = ...`/`data X = ...`,
//! not an author module type) used to kill the entire harness process.
//!
//! Live repro (2026-08-24 verification round,
//! `dogfood-iso/verification-report.md`): the answerer declared `type
//! KyotoResearch = Text` on its own decl plane (gen 1), then compiled a
//! do-block with `fork @KyotoResearch "brief"`. The forced child ran one
//! model round, then its OWN turn's compile died with
//! `EngineError::Setup("Not in scope: type constructor or class
//! 'KyotoResearch'")` — the child was retired as "mechanism failure", the
//! loop answerer was retired with it, and the process exited
//! (`Error: Agent(Engine(Setup(...)))` on stderr).
//!
//! Root cause, TWO parts (both fixed here):
//!
//! 1. **Rust side.** `EngineConfig::turn_target`'s pinned-`Finalize <T>`-row
//!    probe compile (`validate_finalize_row`) validated against
//!    `EngineConfig::validation_include()` alone — a STATIC, config-level
//!    include set (prelude/project-lib/Core) fixed at construction, with no
//!    notion of any particular node's session. A model-declared decl-plane
//!    type lives in the SESSION's own dynamic `lib_include_dir()`, never in
//!    that static set — even though the SAME type resolves fine in the
//!    turn's own BODY compile (`Harness::live_turn_context`'s
//!    `session_include`, already correct pre-fix). Fixed:
//!    `Harness::run_block`/`run_multi_item_block` now read the node's
//!    session bind context BEFORE calling `turn_target` and thread it
//!    through `EngineConfig::turn_target_with_extra_validation_include`, as
//!    both a plain search-path entry AND a
//!    `tidepool_runtime::SessionInject` (`--session-root`/`--inject-val`) —
//!    the decl module itself may import whatever `Val.G<g>` was live when it
//!    was declared (`PersistentSession::define_scoped_in` splices that in
//!    unconditionally), so the probe needs the SAME session-value-injection
//!    capability the real turn compile already had.
//! 2. **Haskell side.** `Tidepool.Translate.modulesOfType` (extract) resolved
//!    a TYPE SYNONYM's defining module by walking the type via
//!    `tyConsOfType`, which looks THROUGH synonyms — so `type KyotoResearch
//!    = Int` reported `GHC.Types` (`Int`'s home) instead of the decl-plane
//!    module `KyotoResearch` itself lives in, even though `renderType`
//!    (unexpanding `ppr`) correctly rendered the surface name "KyotoResearch"
//!    into the prompt/row. Fixed: `modulesOfType` now ALSO unions in the
//!    type's own head TyCon via a raw, non-expanding pattern match.
//!
//! Driven through the production entry point
//! (`SelfHarnessDriver::run_one_loop_iteration`) against the shipped
//! `ForkChildGuiHarness.hs` fixture (an ordinary `runLLMTurn @Int` hole whose
//! answerer forks one child), scripted record-replay, zero live calls — the
//! same discipline as `answerer_async_fork.rs`.
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run; see `haskell/CLAUDE.md`).

mod support;

use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_harness::engine::{EngineConfig, EngineError};
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls, Event, Harness, Observer, SelfHarnessDriver,
};

#[derive(Default)]
struct CapturingObserver {
    events: Mutex<Vec<Event>>,
}

impl Observer for CapturingObserver {
    fn on_event(&self, event: &Event) {
        self.events.lock().push(event.clone());
    }
}

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> std::path::PathBuf {
    repo_root().join("haskell/lib")
}

fn fixtures_dir() -> std::path::PathBuf {
    repo_root().join("tidepool-harness/tests/fixtures")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "fork-child-decl-plane-type".into(),
        extract_fingerprint: "fork-child-decl-plane-type".into(),
        harness_version: "test".into(),
    }
}

fn dump_events(observer: &CapturingObserver) -> String {
    observer
        .events
        .lock()
        .iter()
        .map(|ev| format!("{ev:?}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn code(block: &str) -> RecordedReply {
    RecordedReply {
        content: format!("```haskell\n{block}\n```"),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

fn build_driver(
    replies: Vec<RecordedReply>,
    log_label: &str,
) -> (SelfHarnessDriver, Arc<CapturingObserver>) {
    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        prelude_dir(),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let log_path = std::env::temp_dir().join(format!("{log_label}-{}.jsonl", std::process::id()));
    let writer =
        tidepool_harness::log::LogWriter::create(&log_path, &header()).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let observer = Arc::new(CapturingObserver::default());
    (SelfHarnessDriver::new(agent, observer.clone()), observer)
}

/// Assertion 1a: a `type` ALIAS declared on the session decl plane resolves
/// as a fork child's answer type end-to-end — was process-fatal at tip.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_child_answer_type_declared_as_decl_plane_alias_resolves() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        // Round 1 (loop answerer): decl-only — a single item, no bind/expr
        // after it, so `run_block`'s single-item Decl path commits it and
        // the round is "complete but not finalize", nudging for a real
        // answer on the same hole (same idiom as
        // `selfharness_decl_plane_replay.rs`'s `decl_reply`).
        code("type KyotoResearch = Int"),
        // Round 2 (loop answerer): fork ONE child at the decl-plane type,
        // then finalize with its answer — one compiled block, no extra
        // model round on resume (same shape `fork_child_gui`-style tests
        // use for a plain, non-`async` fork).
        code(
            "import Tidepool.Fork (fork)\n\n\
             do\n\
             \x20 n <- fork @KyotoResearch \"explore\"\n\
             \x20 finalize @Int n :: M ()",
        ),
        // Round 3 (the fork child's own turn): finalize against the SAME
        // decl-plane type. This is the turn whose compile died at tip —
        // `Finalize KyotoResearch`'s row-validation probe could not see the
        // session's own decl-plane include dir.
        code("finalize @KyotoResearch (99 :: Int) :: M ()"),
    ];

    let (mut driver, observer) = build_driver(replies, "fork-child-decl-alias");
    let source = load_harness_source(&fixtures_dir().join("ForkChildGuiHarness.hs"))
        .expect("fork-child-gui fixture loads");

    let outcome = match driver.run_one_loop_iteration(&source, None).await {
        Ok(o) => o,
        Err(e) => panic!("cycle failed: {e}\n\nevents:\n{}", dump_events(&observer)),
    };

    assert_eq!(
        outcome.state_json.get("lastValue").and_then(|v| v.as_i64()),
        Some(99),
        "the parent must resume with the fork child's finalized decl-plane-typed answer: {:?}",
        outcome.state_json
    );
}

/// Assertion 1b: same as above, for a `data` declaration instead of a `type`
/// alias — the two decl-plane shapes STEPS asks to cover separately (a
/// `data` decl needs its own defining module + constructors resolvable,
/// unlike a transparent alias).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_child_answer_type_declared_as_decl_plane_data_resolves() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        code(
            "import Tidepool.Aeson (FromJSON, ToJSON)\n\
             import GHC.Generics (Generic)\n\n\
             data KyotoResult = KyotoResult Int\n\
             \x20 deriving (Generic, ToJSON, FromJSON, Show)",
        ),
        code(
            "import Tidepool.Fork (fork)\n\n\
             do\n\
             \x20 KyotoResult n <- fork @KyotoResult \"explore\"\n\
             \x20 finalize @Int n :: M ()",
        ),
        code("finalize @KyotoResult (KyotoResult 99) :: M ()"),
    ];

    let (mut driver, observer) = build_driver(replies, "fork-child-decl-data");
    let source = load_harness_source(&fixtures_dir().join("ForkChildGuiHarness.hs"))
        .expect("fork-child-gui fixture loads");

    let outcome = match driver.run_one_loop_iteration(&source, None).await {
        Ok(o) => o,
        Err(e) => panic!("cycle failed: {e}\n\nevents:\n{}", dump_events(&observer)),
    };

    assert_eq!(
        outcome.state_json.get("lastValue").and_then(|v| v.as_i64()),
        Some(99),
        "the parent must resume with the fork child's finalized decl-plane `data`-typed \
         answer: {:?}",
        outcome.state_json
    );
}

/// Assertion 2 (the CONTRACT half): a fork naming a type that never resolves
/// anywhere (a model mistake — no decl round ever declared it, no author
/// module exports it) must NOT be able to take the process down either. The
/// fork call itself fails to compile in the PARENT's own turn — an ordinary
/// GHC "not in scope" the parent's existing corrective-retry loop already
/// feeds back — so this pins that the decl-plane fix above does not
/// accidentally widen what a fork call can get away with naming, and that
/// the answerer survives and gets another round with a corrective naming
/// the bad type, exactly like any other compile mistake.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_naming_a_never_declared_type_corrects_the_parent_and_survives() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        // Round 1: fork a type that was never declared anywhere — a GHC
        // "not in scope" compile error in the LOOP ANSWERER's own turn, fed
        // back as a corrective (the round doesn't advance the hole).
        code(
            "import Tidepool.Fork (fork)\n\n\
             do\n\
             \x20 n <- fork @NoSuchKyotoType \"explore\"\n\
             \x20 finalize @Int n :: M ()",
        ),
        // Round 2: the answerer recovers and finalizes plainly.
        code("finalize @Int 7 :: M ()"),
    ];

    let (mut driver, observer) = build_driver(replies, "fork-unresolvable-type");
    let source = load_harness_source(&fixtures_dir().join("ForkChildGuiHarness.hs"))
        .expect("fork-child-gui fixture loads");

    let outcome = match driver.run_one_loop_iteration(&source, None).await {
        Ok(o) => o,
        Err(e) => panic!("cycle failed: {e}\n\nevents:\n{}", dump_events(&observer)),
    };

    assert_eq!(
        outcome.state_json.get("lastValue").and_then(|v| v.as_i64()),
        Some(7),
        "the answerer must get another round after the bad fork and finalize normally: {:?}",
        outcome.state_json
    );
}

/// Unit-level pin for the detection pattern the driver's reclassification
/// relies on (`SelfHarnessDriver::drive_agent_session_to_finalize`'s
/// `Err(HarnessError::Engine(EngineError::Setup(msg))) if msg.contains(...)`
/// arm): a pinned `Finalize <T>` row whose type has NO resolving import at
/// all fails `turn_target` with a `Setup` error naming the type via "Not in
/// scope" — the exact shape the driver keys on to fold a child's setup
/// failure as an `InvocationExit` instead of a fatal `DriverError`, while
/// leaving every OTHER `Setup` failure (which never names the pinned type)
/// on the fatal side. Compile-level, no model in the loop — the same idiom
/// `finalize_type_pinning.rs`'s `pinned_finalize_needs_the_type_in_scope`
/// uses for the author-type case.
#[test]
fn unresolvable_pinned_type_fails_with_the_not_in_scope_plus_type_name_shape() {
    support::require_extract();
    let cfg = EngineConfig::from_decls(typed_request_agent_decls(), prelude_dir(), None)
        .expect("answerer engine config");
    let err = cfg
        .turn_target(Some(("KyotoResearch", &[])))
        .expect_err("a pinned type with no resolving import anywhere cannot validate");
    let msg = match err {
        EngineError::Setup(msg) => msg,
        other => panic!("expected EngineError::Setup, got: {other}"),
    };
    assert!(
        msg.contains("Not in scope") && msg.contains("KyotoResearch"),
        "the driver's reclassification keys on exactly this shape — got: {msg}"
    );
    // A genuinely different kind of `turn_target` failure (a materialize-IO
    // error, say) would NOT contain the pinned type's name — asserting the
    // detection guard's other half is sound is out of scope for a
    // compile-level test (it needs a real IO failure to construct), so it
    // is documented here instead: `SelfHarnessDriver`'s reclassification arm
    // additionally requires `msg.contains(ty_label)`, which a non-type-
    // resolution `Setup` message structurally cannot satisfy unless the
    // pinned type's name happens to collide with unrelated diagnostic text
    // — deliberately narrow, so it never blurs into a genuine mechanism
    // failure (extract binary resolution, cache IO, table assembly).
}
