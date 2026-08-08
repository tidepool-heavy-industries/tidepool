//! Mid-loop compaction coverage for the self-iterating harness, on the
//! SIMPLIFIED mechanism (compaction is ONE plain summarize turn on the
//! existing answerer session):
//!
//!   - the summarize turn's model call counts against the per-loop
//!     inference-call cap (it cannot escape the runaway guard).
//!   - a cycle that fires a mid-loop compaction commits its own state AND its
//!     own summary into the SAME checkpoint generation, so a fresh driver
//!     restored from that checkpoint gets both back together.
//!   - `Event::CompactionTrigger` carries a payload (summary + pre/post
//!     context size + node), emitted after the summary exists.
//!
//! The in-place-relief + high-water coverage lives in
//! `selfharness_compaction.rs`.
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH — run inside
//! `nix develop` (see `haskell/CLAUDE.md`).

use std::sync::{Arc, Mutex};

mod support;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{
    DynModelProvider, ModelProvider, ProviderError, Role, StreamSink, TurnRequest, TurnResponse,
    Usage,
};
use tidepool_harness::{
    answerer_decls, load_harness_source, Event, Harness, HarnessSource, LogObserver, NodeId,
    Observer, SelfHarnessDriver,
};

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf()
}

fn prelude_dir() -> std::path::PathBuf {
    repo_root().join("haskell/lib")
}

fn fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "selfharness-compaction-fixes".into(),
        extract_fingerprint: "selfharness-compaction-fixes".into(),
        harness_version: "test".into(),
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "selfharness-compaction-fixes-{}-{}",
        std::process::id(),
        name
    ));
    // Fresh durable dir per test run (called once per test — a restart-
    // durability test's original and "restart" driver share the returned dir).
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn is_summarize_prompt(user: &str) -> bool {
    user.contains("Summarize EVERYTHING above")
}

const SUMMARY_SENTINEL: &str = "DISTILLED-loop-summary-sentinel";

/// A provider that answers every `runLLMTurn` hole with a finalize whose
/// single-turn input crosses the (low) threshold, and answers a summarize turn
/// with a plain-text sentinel. `pre_input` is the finalize turn's input.
struct CompactingProvider {
    pre_input: u64,
}

impl ModelProvider for CompactingProvider {
    async fn complete(
        &self,
        req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let latest_user = req
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
            .map(|m| m.content.clone())
            .unwrap_or_default();

        if is_summarize_prompt(&latest_user) {
            return Ok(TurnResponse {
                text: SUMMARY_SENTINEL.to_string(),
                usage: Usage {
                    input_tokens: 30,
                    output_tokens: 5,
                },
                reasoning: None,
            });
        }

        let answer = if latest_user.contains("SECOND-HOLE") {
            "blue"
        } else {
            "apple"
        };
        Ok(TurnResponse {
            text: format!("```haskell\n(finalize @Text (\"{answer}\" :: Text) :: M ())\n```"),
            usage: Usage {
                input_tokens: self.pre_input,
                output_tokens: 50,
            },
            reasoning: None,
        })
    }
}

/// Capturing observer: records the payload of every `CompactionTrigger`.
#[derive(Default)]
struct CaptureObserver {
    triggers: Arc<Mutex<Vec<(NodeId, String, u64, u64)>>>,
}

impl Observer for CaptureObserver {
    fn on_event(&self, event: &Event) {
        if let Event::CompactionTrigger {
            node,
            summary,
            pre_input_tokens,
            post_input_tokens,
        } = event
        {
            self.triggers.lock().unwrap().push((
                *node,
                summary.clone(),
                *pre_input_tokens,
                *post_input_tokens,
            ));
        }
    }
}

static NEXT_LOG_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Build a driver whose checkpoint lives in `durable_dir` — passing the SAME
/// `durable_dir` to two `make_driver` calls simulates a restart against the
/// same on-disk checkpoint. The event LOG, by contrast, is a fresh unique
/// file per call (`LogWriter::create` refuses an existing file), so a
/// restart driver does not collide with the first's log.
fn make_driver(
    durable_dir: &std::path::Path,
    pre_input: u64,
    observer: Arc<dyn Observer>,
) -> (SelfHarnessDriver, HarnessSource) {
    let provider: Arc<dyn DynModelProvider> = Arc::new(CompactingProvider { pre_input });
    let mut agent_cfg =
        EngineConfig::from_decls(answerer_decls(), prelude_dir(), Some(fixtures_dir()))
            .expect("answerer engine config");
    agent_cfg.context_window_tokens = Some(1000);
    let log_id = NEXT_LOG_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let log_dir = std::env::temp_dir().join(format!(
        "selfharness-compaction-fixes-log-{}-{}",
        std::process::id(),
        log_id
    ));
    std::fs::create_dir_all(&log_dir).expect("log dir");
    let writer = tidepool_harness::log::LogWriter::create(log_dir.join("log.jsonl"), &header())
        .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));
    let mut driver = SelfHarnessDriver::new(agent, observer);
    driver.set_checkpoint_path(durable_dir.join("checkpoint.json"));
    driver.set_compaction_threshold_percent(50);
    let source = load_harness_source(&fixtures_dir().join("CompactionHarness.hs"))
        .expect("compaction harness source loads");
    (driver, source)
}

/// The summarize turn is a model call subject to the per-loop
/// inference-call cap. With the cap set to exactly the number of answerer
/// rounds before the first compaction (1 finalize round), the compaction
/// check's own increment trips the cap — the summarize turn is counted
/// against it and does not escape the runaway guard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c2_summarize_turn_counts_against_inference_cap() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, nix develop)");
        return;
    }
    let _cache_guard = support::isolate_cache();

    let (mut driver, source) = make_driver(&scratch("c2"), 600, Arc::new(LogObserver));
    // The first hole finalizes in ONE answerer round (call #1). Cap = 1: after
    // that call, `loop_inference_calls == 1`, so the compaction summarize turn's
    // own cap check (`1 >= 1`) fires the "during compaction" hard-stop — which
    // is only reachable if compaction is gated by the SAME cap as answerer
    // rounds.
    driver.set_loop_inference_call_cap(1);

    let err = driver
        .run_one_cycle(&source, None)
        .expect_err("cap=1 must hard-stop when compaction tries its own inference call");
    let msg = format!("{err}");
    assert!(
        msg.contains("during compaction"),
        "the cap must be hit BY THE COMPACTION summarize turn (proving it is counted), got: {msg}"
    );
}

/// A cycle that fires a mid-loop compaction and completes commits ITS OWN
/// state and ITS OWN summary together, as one checkpoint generation — a
/// fresh driver restored from that same checkpoint path gets both back
/// together, never the summary alone (the in-memory-only `last_compaction`
/// would otherwise roll back to `None` on a fresh driver, and the state
/// would otherwise never have been committed by `run_one_cycle` at all).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_commit_pairs_state_and_compaction_from_one_cycle() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, nix develop)");
        return;
    }
    let _cache_guard = support::isolate_cache();

    // Both drivers share the SAME checkpoint path — that IS the restart.
    let durable = scratch("c3");
    let (mut driver1, source) = make_driver(&durable, 600, Arc::new(LogObserver));
    let outcome = driver1
        .run_one_cycle(&source, None)
        .expect("cycle with a mid-loop compaction");
    assert!(
        outcome
            .compaction
            .as_deref()
            .is_some_and(|s| s.contains(SUMMARY_SENTINEL)),
        "driver 1 must have compacted and produced the summary"
    );
    // Driver 1 is dropped here — its in-memory state is gone; only what it
    // committed to the checkpoint survives.
    drop(driver1);

    // Simulate a restart: a brand-new driver over the same checkpoint path
    // (its own fresh event log).
    let (mut driver2, source2) = make_driver(&durable, 600, Arc::new(LogObserver));
    assert_eq!(
        driver2.last_compaction(),
        None,
        "a fresh driver starts with no in-memory compaction"
    );

    let restored_state = driver2
        .restore(&source2)
        .expect("restore reloads the committed checkpoint")
        .expect("driver 1's completed cycle committed a checkpoint");
    assert_eq!(
        restored_state, outcome.state_json,
        "the restored state must be exactly driver 1's own committed state"
    );
    assert!(
        driver2
            .last_compaction()
            .is_some_and(|s| s.contains(SUMMARY_SENTINEL)),
        "the restored summary must be driver 1's own compaction, got: {:?}",
        driver2.last_compaction()
    );
}

/// `Event::CompactionTrigger` carries the summary + pre/post context size
/// + node, emitted after the summary exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c4_compaction_trigger_event_carries_payload() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, nix develop)");
        return;
    }
    let _cache_guard = support::isolate_cache();

    let observer = Arc::new(CaptureObserver::default());
    let triggers = observer.triggers.clone();
    let (mut driver, source) = make_driver(&scratch("c4"), 600, observer);

    driver
        .run_one_cycle(&source, None)
        .expect("cycle with a mid-loop compaction");

    let captured = triggers.lock().unwrap();
    // At least one compaction fired (the two-hole fixture re-crosses the
    // threshold after each hole's finalize turn resets the window, so BOTH
    // holes can trip it — the count is not load-bearing here; the PAYLOAD is).
    assert!(
        !captured.is_empty(),
        "a compaction must have fired this cycle"
    );
    let (_node, summary, pre, _post) = &captured[0];
    assert!(
        summary.contains(SUMMARY_SENTINEL),
        "the event must carry the actual summary text, got: {summary:?}"
    );
    assert!(
        *pre >= 500,
        "pre_input_tokens must be the context size that crossed the 500 threshold, got {pre}"
    );
    // The event names a node (the per-loop answerer) — the payload's node field
    // is populated (a valid NodeId, id 0 included). Summary + pre-size are the
    // fields the distillation substrate (the jsonl transcript) needs.
}
