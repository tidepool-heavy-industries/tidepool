//! Acceptance for the outer-row Subagent seam: the AUTHORED loop calls
//! `spawnAgent @CuratorReceipt`, the driver
//! services the resulting `Subagent` suspension through its driver-owned
//! `SubagentHandler` (suspension-serviced — the outer session's handled
//! prefix stays EMPTY on the shared machine), and the typed receipt crosses
//! back into the resumed continuation and out through durable `State`.
//!
//! Backend tier: `MockBackend` — no live model, no tokens, ever. The real
//! saga still runs end to end (worktree allocated from a real temporary git
//! repo, binding table written, payload decoded by the Haskell-side
//! `FromJSON`).
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod support;

use serde_json::json;
use tidepool_agent::backend::mock::MockBackend;
use tidepool_agent::backend::{AgentBackend, AgentBackendFactory};
use tidepool_agent::seam::{
    AgentBackendError, BackendThreadId, CycleResultPayload, CycleSpec, ThreadSpec, ToolReply,
    TurnEvent,
};
use tidepool_handlers::SubagentHandler;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::DynModelProvider;
use tidepool_harness::replay::ReplayProvider;
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls, Harness, LogObserver, SelfHarnessDriver,
};
use tidepool_worktree::testing::TestRepo;

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn fixtures_dir() -> std::path::PathBuf {
    repo_root().join("tidepool-harness/tests/fixtures")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "outer-subagent".into(),
        extract_fingerprint: "outer-subagent".into(),
        harness_version: "test".into(),
    }
}

fn concurrent_header() -> LogHeader {
    LogHeader {
        prelude_hash: "outer-subagent-concurrent".into(),
        extract_fingerprint: "outer-subagent-concurrent".into(),
        harness_version: "test".into(),
    }
}

fn hylo_concurrent_header() -> LogHeader {
    LogHeader {
        prelude_hash: "outer-subagent-hylo-concurrent".into(),
        extract_fingerprint: "outer-subagent-hylo-concurrent".into(),
        harness_version: "test".into(),
    }
}

/// The full round trip: authored `loop` → `spawnAgent` → Subagent suspension
/// → driver-owned handler (real saga, mock backend) → typed
/// `CuratorReceipt` decode → resumed continuation → durable `State`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outer_loop_spawn_agent_round_trips_through_the_driver() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    // The nested answerer config is required by the driver's constructor but
    // never exercised here — the fixture loop opens no model holes.
    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(Vec::new()));
    let writer = tidepool_harness::log::LogWriter::create(
        support::unique_temp_log_path("outer-subagent"),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));

    // The curator's target: a real (temporary) git repo — the memory store's
    // stand-in — plus registry/worktree/binding roots OUTSIDE it.
    let store = TestRepo::init().expect("git init the memory store");
    store
        .writer()
        .commit_file("MEMORY.md", "seed digest\n", "seed the store")
        .expect("seed commit");
    let roots = tempfile::TempDir::new().expect("substrate roots");
    let backend = MockBackend::completing(CycleResultPayload::Structured(serde_json::json!({
        "digest": "one memory: the operator prefers typed options",
        "summary": "filed 1 intention",
    })));
    let handler = SubagentHandler::new(
        roots.path().join("registry"),
        roots.path().join("worktrees"),
        roots.path().join("bindings"),
        store.path().to_path_buf(),
        Box::new(backend),
    )
    .expect("subagent handler opens");
    driver.set_subagent_handler(handler);

    let source = load_harness_source(&fixtures_dir().join("SubagentHarness.hs"))
        .expect("fixture harness loads");
    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("one cycle: loop spawns, driver services, receipt crosses back");

    let state = &outcome.state_json;
    assert_eq!(
        state.get("runs").and_then(|v| v.as_i64()),
        Some(1),
        "the loop completed exactly one spawn, got {state:?}"
    );
    assert_eq!(
        state.get("lastDigest").and_then(|v| v.as_str()),
        Some("one memory: the operator prefers typed options"),
        "the typed receipt's digest field must cross into durable state, got {state:?}"
    );
    assert_eq!(
        state.get("lastError").and_then(|v| v.as_str()),
        Some(""),
        "the spawn must succeed (no rendered SpawnError), got {state:?}"
    );
}

/// Without a wired handler, a Subagent suspension fails LOUDLY with the
/// wiring instruction — never a hang, never a silent drop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outer_spawn_without_handler_errors_legibly() {
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
        support::unique_temp_log_path("outer-subagent-nh"),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));

    let source = load_harness_source(&fixtures_dir().join("SubagentHarness.hs"))
        .expect("fixture harness loads");
    let err = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect_err("a Subagent suspension with no handler must error");
    let msg = err.to_string();
    assert!(
        msg.contains("set_subagent_handler"),
        "the error names the wiring seam, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// CONCURRENT outer-row Subagent servicing
// (`CONCURRENT_SIBLINGS_SPIKE_FINDINGS.md`'s smallest driver change): two
// green threads that each `spawnAgent` from the AUTHORED outer `loop` must
// overlap in wall time, not drive one fully to completion (spawn + await)
// before the other's already-ready request is even looked at.
//
// `DelayingBackend` wraps `MockBackend` with a deliberate per-cycle sleep in
// `start_turn` — the real backend work `SubagentSpawnAsync` detaches onto its
// own thread (see `SelfHarnessDriver::service_outer_subagent`'s doc for why
// that is what actually overlaps, not the driver-owned handler's own
// dispatch, which still serializes on one lock). `max_concurrent()` is the
// same overlap receipt `answerer_async_fork.rs`'s
// `async_fork_overlap_two_children_drive_concurrently` uses for Fork, applied
// here to Subagent. Kept in THIS file (rather than a new `tests/*.rs` binary)
// so `scripts/battery-shard.sh`'s documented shard groups don't need a new
// entry for one additional scenario against the same seam.

/// Wraps [`MockBackend`] with a deliberate per-cycle sleep in `start_turn`,
/// and tracks how many `DelayingBackend`s are inside that sleep AT ONCE
/// (`concurrent`/`max_concurrent`) — the overlap receipt, mirroring
/// `ReplayProvider::with_hold`'s `max_concurrent()` for the Fork acceptance.
struct DelayingBackend {
    inner: MockBackend,
    delay: Duration,
    concurrent: Arc<AtomicUsize>,
    max_concurrent: Arc<AtomicUsize>,
}

impl AgentBackend for DelayingBackend {
    fn start_thread(&mut self, spec: &ThreadSpec) -> Result<BackendThreadId, AgentBackendError> {
        self.inner.start_thread(spec)
    }

    fn start_turn(
        &mut self,
        thread: &BackendThreadId,
        spec: &CycleSpec,
    ) -> Result<TurnEvent, AgentBackendError> {
        let now = self.concurrent.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_concurrent.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        let result = self.inner.start_turn(thread, spec);
        self.concurrent.fetch_sub(1, Ordering::SeqCst);
        result
    }

    fn resume(&mut self, reply: ToolReply) -> Result<TurnEvent, AgentBackendError> {
        self.inner.resume(reply)
    }
}

/// One [`DelayingBackend`] per cycle, each completing with the next queued
/// payload in order — `SubagentHandler::subagent_spawn_async` mints one
/// backend per admitted cycle (concurrent cycles never share one), so a
/// factory (not a single pre-built backend) is what lets two cycles exist at
/// once at all.
struct DelayingBackendFactory {
    delay: Duration,
    concurrent: Arc<AtomicUsize>,
    max_concurrent: Arc<AtomicUsize>,
    payloads: Arc<Mutex<VecDeque<serde_json::Value>>>,
}

impl AgentBackendFactory for DelayingBackendFactory {
    fn create(&mut self) -> Result<Box<dyn AgentBackend + Send>, AgentBackendError> {
        let payload = self
            .payloads
            .lock()
            .unwrap()
            .pop_front()
            .expect("one queued payload per admitted cycle");
        Ok(Box::new(DelayingBackend {
            inner: MockBackend::completing(CycleResultPayload::Structured(payload)),
            delay: self.delay,
            concurrent: self.concurrent.clone(),
            max_concurrent: self.max_concurrent.clone(),
        }))
    }
}

/// The overlap acceptance case: an authored `loop` that starts TWO
/// `spawnAgent` calls as green threads (`async`) before `wait`-ing either —
/// both reach their own `Subagent` suspension at the same logical moment
/// (`TwoConcurrentSubagentHarness.hs`). Before the batching fix, the driver's
/// single-item ready-queue pop served thread A's `spawnAsync` THEN its own
/// `awaitAgent` (a real ~150ms sleep, held under `Self::subagent`'s lock)
/// fully to completion before even looking at thread B's already-ready
/// `spawnAsync` request — total wall time ~2×delay. After the fix, both
/// threads' `spawnAsync` calls are batched and driven concurrently, each
/// kicking off its own background cycle thread — those two cycles' `sleep`s
/// run in parallel, so `max_concurrent() > 1` and the total wall time
/// approaches ONE delay, not two.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outer_loop_two_concurrent_spawn_agent_calls_overlap_in_wall_time() {
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
        support::unique_temp_log_path("outer-subagent-concurrent"),
        &concurrent_header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));

    let store = TestRepo::init().expect("git init the memory store");
    store
        .writer()
        .commit_file("MEMORY.md", "seed digest\n", "seed the store")
        .expect("seed commit");
    let roots = tempfile::TempDir::new().expect("substrate roots");

    let delay = Duration::from_millis(150);
    let concurrent = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));
    let payloads = Arc::new(Mutex::new(VecDeque::from(vec![
        json!({"digest": "digest-a", "summary": "a"}),
        json!({"digest": "digest-b", "summary": "b"}),
    ])));
    let factory = DelayingBackendFactory {
        delay,
        concurrent: concurrent.clone(),
        max_concurrent: max_concurrent.clone(),
        payloads,
    };
    let handler = SubagentHandler::with_backends(
        roots.path().join("registry"),
        roots.path().join("worktrees"),
        roots.path().join("bindings"),
        store.path().to_path_buf(),
        Box::new(factory),
    )
    .expect("subagent handler opens");
    driver.set_subagent_handler(handler);

    let source = load_harness_source(&fixtures_dir().join("TwoConcurrentSubagentHarness.hs"))
        .expect("fixture harness loads");

    // No wall-clock assertion: a real `git worktree add` per cycle dominates
    // total time on this box (observed several seconds, swamping the
    // millisecond-scale `delay`), so elapsed time is not a reliable overlap
    // signal here — `max_concurrent()` (whether both backend `start_turn`
    // sleeps were EVER simultaneously in flight) is the direct, non-flaky
    // receipt, same discipline `answerer_async_fork.rs`'s
    // `async_fork_overlap_two_children_drive_concurrently` uses for Fork.
    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("one cycle: loop spawns two concurrent agents, both receipts cross back");

    assert!(
        max_concurrent.load(Ordering::SeqCst) >= 2,
        "both cycles' backend `start_turn` sleeps must have been in flight at the \
         same time (max_concurrent() == {}) — a scheduler that still drives Subagent \
         requests one at a time would never exceed 1",
        max_concurrent.load(Ordering::SeqCst)
    );

    let state = &outcome.state_json;
    assert_eq!(
        state.get("runs").and_then(|v| v.as_i64()),
        Some(1),
        "the loop completed exactly one round, got {state:?}"
    );
    assert_eq!(
        state.get("digestA").and_then(|v| v.as_str()),
        Some("digest-a"),
        "child A's own receipt must land on the RIGHT handle, got {state:?}"
    );
    assert_eq!(
        state.get("digestB").and_then(|v| v.as_str()),
        Some("digest-b"),
        "child B's own receipt must land on the RIGHT handle, got {state:?}"
    );
    assert_eq!(
        state.get("lastError").and_then(|v| v.as_str()),
        Some(""),
        "both spawns must succeed (no rendered SpawnError), got {state:?}"
    );
}

/// The same overlap receipt as
/// `outer_loop_two_concurrent_spawn_agent_calls_overlap_in_wall_time`, but
/// through `Tidepool.Swarm.hyloConcurrentM` itself
/// (`HyloConcurrentSubagentHarness.hs`): a two-leaf plan tree, unfolded by a
/// trivial coalgebra and folded by an algebra whose LEAF case is where each
/// `spawnAgent` cycle lives, driven via
/// `hyloConcurrentM mapConcurrently planAlg planCoalg RootSeed` instead of a
/// hand-written pair of `async`/`wait` calls. Proves the combinator itself —
/// not just `Tidepool.Async` underneath it — actually overlaps its children's
/// Subagent cycles when run through the real driver (the fast-tier property
/// suite in `haskell/test-swarm/SwarmSpec.hs` already proves plan-order
/// reassembly under a bare `Identity`; this is the wall-clock half of the
/// same combinator).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hylo_concurrent_m_two_leaf_subagent_spawns_overlap_in_wall_time() {
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
        support::unique_temp_log_path("outer-subagent-hylo-concurrent"),
        &hylo_concurrent_header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));

    let store = TestRepo::init().expect("git init the memory store");
    store
        .writer()
        .commit_file("MEMORY.md", "seed digest\n", "seed the store")
        .expect("seed commit");
    let roots = tempfile::TempDir::new().expect("substrate roots");

    let delay = Duration::from_millis(150);
    let concurrent = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));
    let payloads = Arc::new(Mutex::new(VecDeque::from(vec![
        json!({"digest": "digest-a", "summary": "a"}),
        json!({"digest": "digest-b", "summary": "b"}),
    ])));
    let factory = DelayingBackendFactory {
        delay,
        concurrent: concurrent.clone(),
        max_concurrent: max_concurrent.clone(),
        payloads,
    };
    let handler = SubagentHandler::with_backends(
        roots.path().join("registry"),
        roots.path().join("worktrees"),
        roots.path().join("bindings"),
        store.path().to_path_buf(),
        Box::new(factory),
    )
    .expect("subagent handler opens");
    driver.set_subagent_handler(handler);

    let source = load_harness_source(&fixtures_dir().join("HyloConcurrentSubagentHarness.hs"))
        .expect("fixture harness loads");

    let outcome = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("one cycle: hyloConcurrentM drives two leaves, both receipts cross back");

    assert!(
        max_concurrent.load(Ordering::SeqCst) >= 2,
        "both leaves' backend `start_turn` sleeps must have been in flight at the \
         same time (max_concurrent() == {}) — hyloConcurrentM driving its children \
         one at a time would never exceed 1",
        max_concurrent.load(Ordering::SeqCst)
    );

    let state = &outcome.state_json;
    assert_eq!(
        state.get("runs").and_then(|v| v.as_i64()),
        Some(1),
        "the loop completed exactly one round, got {state:?}"
    );
    assert_eq!(
        state.get("digestA").and_then(|v| v.as_str()),
        Some("digest-a"),
        "leaf A's own receipt must land on the RIGHT (plan-order) handle, got {state:?}"
    );
    assert_eq!(
        state.get("digestB").and_then(|v| v.as_str()),
        Some("digest-b"),
        "leaf B's own receipt must land on the RIGHT (plan-order) handle, got {state:?}"
    );
    assert_eq!(
        state.get("lastError").and_then(|v| v.as_str()),
        Some(""),
        "both spawns must succeed (no rendered SpawnError), got {state:?}"
    );
}
