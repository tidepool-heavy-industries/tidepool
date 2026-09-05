//! THE SMALLEST KNOWN REPRODUCER of the tenure-then-resume rooting gap —
//! see `tidepool-harness/tests/fixtures/MinimalWatchListHarness.hs`'s module
//! doc for the full bisection writeup and what it does and does not
//! resolve. `#[ignore]`d and documented, NOT a sanctioned red — same
//! standard as `nested_async_repro.rs`/`node_mailboxes.rs`.
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH.

use std::sync::Arc;

use crate::support;

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

/// Always reports no movement — mirrors `node_mailboxes.rs`'s substitution,
/// so this proves the mailbox/async path without depending on a real
/// `WorktreeMonitor` baseline.
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

/// Was `#[ignore]`d pending the tenure-then-resume fix — see
/// `MinimalWatchListHarness.hs`'s module doc for the bisection and
/// `nested_async_repro.rs` for the full mechanism writeup. Fix:
/// `tidepool-runtime/tests/tenure_resume_gc_repro.rs`'s module doc.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn minimal_watch_list_round_trips() {
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
        std::env::temp_dir().join(format!("minimal-watch-list-{}.jsonl", std::process::id())),
        &LogHeader {
            prelude_hash: "minimal-watch-list".into(),
            extract_fingerprint: "minimal-watch-list".into(),
            harness_version: "test".into(),
        },
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_console_handler(ConsoleHandler);
    driver.set_event_handler(RepoEventHandler::with_source(
        Box::new(NoOpSource),
        EventConfig::default(),
    ));

    let source = load_harness_source(&fixtures_dir().join("MinimalWatchListHarness.hs"))
        .expect("fixture harness loads");
    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("a bare list captured by an async'd closure must survive tenure + resume");

    let state = &outcome.state_json;
    assert_eq!(state.get("runs").and_then(|v| v.as_i64()), Some(1));
}
