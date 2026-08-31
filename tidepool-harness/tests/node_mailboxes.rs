//! `Tidepool.Node` capability mailboxes, including a burst that arrives
//! before its receiver subscribes.
//!
//! # What was blocked, and what is not
//!
//! The wave-2 SURFACE is green and proven elsewhere: `Tidepool.Node` compiles
//! into the dogfood row, `waitEvent` composes into a `nextEvent` select
//! (asserted in the `outer_effects` bundle), the mailbox verbs and their
//! per-key coalescing are unit-tested in `tidepool-handlers`'
//! `MailboxTable`, and the driver's non-blocking `RepoEventAwait`
//! servicing is exercised by that same bundle's deadline wait.
//!
//! What WAS blocked (now fixed) was END-TO-END `forkNode`: a node body whose
//! closure captures a `NodeCtx` (whose `inbox` field is itself a closure)
//! tripped
//!
//! ```text
//! AsyncSpawnWith spawner resume failed: turn run failed:
//!   heap bridge error: unexpected heap tag: 255
//! ```
//!
//! ONE `forkNode`, ONE `sendUp`, no burst, no nesting reproduced it. That was
//! the finding that renamed the GC hunt from "nested async" to
//! "tenure-then-resume": nesting was not required, closure-graph depth and
//! allocation volume were what separated passing from failing. Scenarios 1
//! and 2 of this test (plain message, silent deadline) now pass cleanly.
//!
//! # Why standalone rather than in the `outer_effects` bundle
//!
//! Crash-class. A bundled crash destroys its siblings' diagnosis, and these
//! scenarios bundled would take S1-L1's outer-row assertions and wave 1's
//! entire green-thread acceptance down with them — which is precisely what
//! the root `CLAUDE.md` discipline keeps crash-class fixtures out of family
//! bundles for.
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH.

use std::sync::Arc;

mod support;

use tidepool_bridge_effects::{EvRepositoryEvent, WtWorktreeId};
use tidepool_handlers::{
    ConsoleHandler, EventConfig, EventError, ObservationSource, RepoEventHandler,
};
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::DynModelProvider;
use tidepool_harness::replay::ReplayProvider;
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls, Harness, LogObserver, SelfHarnessDriver,
};

/// Always reports no movement — mailbox observations are published directly
/// into the registry by the driver, never discovered by reconciliation.
struct NoOpSource;

impl ObservationSource for NoOpSource {
    fn observe(&mut self, _worktree: &WtWorktreeId) -> Result<Vec<EvRepositoryEvent>, EventError> {
        Ok(Vec::new())
    }
}

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn fixtures_dir() -> std::path::PathBuf {
    repo_root().join("tidepool-harness/tests/fixtures")
}

/// A parent select-loops over {message, deadline} across three scenarios: a
/// child that sends, a silent child that lets the deadline win, and a burst
/// that must coalesce to its LAST payload.
///
/// The burst deliberately sends before its parent subscribes. It proves that
/// the capability mailbox retains the keyed latest value until `received`
/// consumes it, instead of requiring a subscription to have already existed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parent_selects_over_message_and_deadline() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(Vec::new()));
    let writer = tidepool_harness::log::LogWriter::create(
        support::unique_temp_log_path("node-mailboxes"),
        &LogHeader {
            prelude_hash: "node-mailboxes".into(),
            extract_fingerprint: "node-mailboxes".into(),
            harness_version: "test".into(),
        },
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_console_handler(ConsoleHandler);
    // Same acceptance-harness substitution `outer_effects.rs` uses: reports no
    // git movement, so this proves the mailbox/select path without depending
    // on a real `WorktreeMonitor` baseline.
    driver.set_event_handler(RepoEventHandler::with_source(
        Box::new(NoOpSource),
        EventConfig::default(),
    ));

    let source = load_harness_source(&fixtures_dir().join("NodeMailboxHarness.hs"))
        .expect("fixture harness loads");
    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("forkNode / sendUp / received must round-trip through the driver");

    let state = &outcome.state_json;
    assert_eq!(
        state.get("nodeMessage").and_then(|v| v.as_i64()),
        Some(777),
        "the parent's select must observe the child's sendUp before the deadline, got {state:?}"
    );
    assert_eq!(
        state.get("nodeSilentTick").and_then(|v| v.as_bool()),
        Some(true),
        "a silent child must let the deadline branch win, got {state:?}"
    );
    assert_eq!(
        state.get("nodeBurstPayload").and_then(|v| v.as_i64()),
        Some(3),
        "a same-key burst must be observed ONCE carrying the LAST payload, got {state:?}"
    );
}
