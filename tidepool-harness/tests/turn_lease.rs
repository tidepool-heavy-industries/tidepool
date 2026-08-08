//! A per-node turn lease serializes `drive_turn`'s snapshot -> provider
//! await -> log append -> resident run -> outcome publish span against a
//! second concurrent call on the SAME node (GHC-tier: forces a real node and
//! drives real compiles through the real Harness, zero live model calls via
//! scripted providers).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, Event, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{
    DynModelProvider, ModelProvider, ProviderError, Role, StreamSink, TurnRequest, TurnResponse,
    Usage,
};
use tidepool_harness::tree::NodeState;
use tidepool_harness::{Harness, HarnessError, TurnOutcome};

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn prelude_dir() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| std::path::PathBuf::from("haskell/lib"))
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "turn-lease".into(),
        extract_fingerprint: "turn-lease".into(),
        harness_version: "test".into(),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 12,
        output_tokens: 4,
    }
}

const OK_REPLY: &str = "```haskell\npure (toJSON (0 :: Int))\n```";

/// A provider whose FIRST call parks: it signals `started` (so the test
/// knows the in-flight `drive_turn` has passed lease acquisition and is now
/// blocked on the provider, still holding the lease), then waits on
/// `proceed` before returning a canned completing reply. Every later call
/// answers immediately with the same reply.
struct GatedProvider {
    calls: AtomicU32,
    started: Arc<tokio::sync::Notify>,
    proceed: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl ModelProvider for GatedProvider {
    async fn complete(
        &self,
        _req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let idx = self.calls.fetch_add(1, Ordering::SeqCst);
        if idx == 0 {
            self.started.notify_one();
            let rx = self
                .proceed
                .lock()
                .unwrap()
                .take()
                .expect("proceed receiver taken exactly once");
            rx.await.expect("proceed sender dropped without firing");
        }
        Ok(TurnResponse {
            text: OK_REPLY.to_string(),
            usage: usage(),
            reasoning: None,
        })
    }
}

/// A provider whose FIRST call fails; every later call completes.
struct FailFirstProvider {
    calls: AtomicU32,
}

impl ModelProvider for FailFirstProvider {
    async fn complete(
        &self,
        _req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let idx = self.calls.fetch_add(1, Ordering::SeqCst);
        if idx == 0 {
            return Err(ProviderError::Api("induced first-call failure".into()));
        }
        Ok(TurnResponse {
            text: OK_REPLY.to_string(),
            usage: usage(),
            reasoning: None,
        })
    }
}

fn assistant_turn_deltas(
    log_path: &std::path::Path,
    node: tidepool_harness::tree::NodeId,
) -> usize {
    let (_h, events) = LogReader::open(log_path).expect("open log");
    events
        .map(|r| r.expect("well-formed record").event)
        .filter(|e| {
            matches!(
                e,
                Event::TurnDelta { node: n, role: Role::Assistant, .. } if *n == node
            )
        })
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_drive_turn_on_one_node_serializes() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("turn-lease.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let started = Arc::new(tokio::sync::Notify::new());
    let (proceed_tx, proceed_rx) = tokio::sync::oneshot::channel();
    let provider = Arc::new(GatedProvider {
        calls: AtomicU32::new(0),
        started: started.clone(),
        proceed: std::sync::Mutex::new(Some(proceed_rx)),
    });
    let provider_dyn: Arc<dyn DynModelProvider> = provider.clone();
    let harness = Arc::new(Harness::new(writer, cfg, provider_dyn).expect("harness boots"));

    let root = harness.create_root("lease root", "Begin.").unwrap();
    harness.force(root, Actor::Operator).unwrap();

    // Task A: drive_turn acquires the lease, then parks on the gated
    // provider — still holding the lease.
    let h_a = harness.clone();
    let task_a = tokio::spawn(async move { h_a.drive_turn(root).await });

    started.notified().await;

    // Task A is confirmed in flight (past lease acquisition, blocked on the
    // provider). A second drive_turn on the SAME node must fail fast rather
    // than race the transcript/turn_seq snapshot.
    let second = harness.drive_turn(root).await;
    assert!(
        matches!(second, Err(HarnessError::TurnInFlight(n)) if n == root),
        "expected TurnInFlight while the first turn is in flight"
    );

    // Let task A's turn complete.
    proceed_tx.send(()).unwrap();
    let first = task_a.await.expect("task A did not panic");
    assert!(
        matches!(first, Ok(TurnOutcome::Completed { .. })),
        "expected the in-flight turn to complete"
    );

    assert_eq!(harness.tree().state(root), Some(NodeState::Done));
    assert_eq!(
        assistant_turn_deltas(&log_path, root),
        1,
        "turn_seq must have advanced by exactly one — one TurnDelta logged, not two"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_turn_releases_its_lease() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let writer = LogWriter::create(dir.path().join("turn-lease-fail.jsonl"), &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let provider = Arc::new(FailFirstProvider {
        calls: AtomicU32::new(0),
    });
    let provider_dyn: Arc<dyn DynModelProvider> = provider.clone();
    let harness = Arc::new(Harness::new(writer, cfg, provider_dyn).expect("harness boots"));

    let root = harness.create_root("lease-fail root", "Begin.").unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let first = harness.drive_turn(root).await;
    assert!(first.is_err(), "the first (induced) turn must fail");

    // A following turn on the same node must succeed — the lease was
    // released on the error path, not stranded held.
    let second = harness
        .drive_turn(root)
        .await
        .expect("the lease must have been released after the failed first turn");
    assert!(matches!(second, TurnOutcome::Completed { .. }));
}
