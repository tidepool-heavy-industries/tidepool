//! Acceptance coverage for the self-iterating harness: a MULTI-cycle
//! run of the reference generic-assistant harness
//! (`examples/harness/Harness.hs`) through the production entry point
//! (`SelfHarnessDriver::run_one_loop_iteration`, called repeatedly — the same
//! per-cycle building block [`run_loop`](tidepool_harness::SelfHarnessDriver::run_loop)
//! uses forever), threading each cycle's `LoopIterationOutcome::state_json` into the
//! next as `prior_state` exactly like `run_loop` does. Asserts `State`
//! (`mode`/`notes`/`lastDecision`) accumulates ACROSS repeated loop
//! boundaries, not just across one, and that the driver's own iteration
//! count (a runtime fact, not part of `State`) advances alongside it.
//! Needs `TIDEPOOL_EXTRACT` and the
//! with-packages GHC on PATH — run inside `nix develop` (see
//! `haskell/CLAUDE.md`).

use std::sync::Arc;

use parking_lot::Mutex;

mod support;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::LogHeader;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls, Harness, LogObserver, SelfHarnessDriver,
};

fn repo_root() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> std::path::PathBuf {
    repo_root().join("haskell/lib")
}

fn examples_harness_dir() -> std::path::PathBuf {
    repo_root().join("examples/harness")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "acceptance-selfharness".into(),
        extract_fingerprint: "acceptance-selfharness".into(),
        harness_version: "test".into(),
    }
}

/// One recorded `finalize @Decision (...)` reply — a fenced Haskell block an
/// answerer turn would emit, importing the reference harness's `Decision`/
/// `Confidence` constructors.
fn decision_reply(action: &str, rationale: &str, confidence: &str) -> RecordedReply {
    let content = format!(
        "```haskell\nimport HarnessTypes (Decision (..), Confidence (..))\n\n\
         (finalize @Decision (Decision {{ action = \"{action}\", rationale = \"{rationale}\", \
         confidence = {confidence} }}) :: M ())\n```"
    );
    RecordedReply {
        content,
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

/// TWO render -> loop -> runLLMTurn @Decision -> finalize -> render
/// cycles, each `run_one_loop_iteration` call threading the prior cycle's
/// `state_json` into the next as `prior_state` — the exact shape
/// `SelfHarnessDriver::run_loop`'s production loop uses, just bounded to
/// two iterations instead of forever. Asserts `State` keeps accumulating
/// across repeated loop boundaries: `loopCount` increments every cycle,
/// `mode` advances `Observing -> Deciding -> Acting`, `lastDecision` tracks
/// the latest replayed `Decision`, `notes` accumulates (most-recent-first),
/// and each cycle's post-loop `render` reflects the new `State`.
///
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selfharness_multi_cycle_state_accumulates_across_loop_boundaries() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let replies = vec![
        decision_reply("observe", "first loop", "Medium"),
        decision_reply("decide", "second loop", "High"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!(
            "acceptance-selfharness-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let expected = [
        ("Deciding", 1i64, "observe", "Medium"),
        ("Acting", 2i64, "decide", "High"),
    ];

    let mut prior_state = None;
    for (i, (expected_mode, expected_loop_count, expected_action, expected_confidence)) in
        expected.iter().enumerate()
    {
        let outcome = driver
            .run_one_loop_iteration(&source, prior_state.as_ref())
            .await
            .unwrap_or_else(|e| panic!("cycle {i} failed: {e}"));

        let state = &outcome.state_json;
        assert_eq!(
            state.get("mode").and_then(|v| v.as_str()),
            Some(*expected_mode),
            "cycle {i}: mode mismatch, got {state:?}"
        );
        assert_eq!(
            driver.iteration(),
            *expected_loop_count as u64,
            "cycle {i}: the driver's iteration count must increment every cycle"
        );

        let decision = state
            .get("lastDecision")
            .and_then(|v| v.as_object())
            .unwrap_or_else(|| {
                panic!("cycle {i}: lastDecision must be a Just Decision, got {state:?}")
            });
        assert_eq!(
            decision.get("action").and_then(|v| v.as_str()),
            Some(*expected_action),
            "cycle {i}: lastDecision.action mismatch, got {decision:?}"
        );
        assert_eq!(
            decision.get("confidence").and_then(|v| v.as_str()),
            Some(*expected_confidence),
            "cycle {i}: lastDecision.confidence mismatch, got {decision:?}"
        );

        // notes accumulate most-recent-first (`take 5 (action d : notes st)`,
        // Harness.hs) — every action decided so far, in reverse order.
        let notes: Vec<&str> = state
            .get("notes")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("cycle {i}: notes must be an array, got {state:?}"))
            .iter()
            .map(|n| n.as_str().expect("note is a string"))
            .collect();
        let expected_notes: Vec<&str> = expected[..=i]
            .iter()
            .rev()
            .map(|(_, _, action, _)| *action)
            .collect();
        assert_eq!(
            notes, expected_notes,
            "cycle {i}: notes must accumulate most-recent-first"
        );

        // The post-loop render reflects the NEW state — the updated
        // State reaches the next render, every cycle, not just the first.
        assert!(
            outcome
                .prompt_after
                .contains(&format!("{expected_loop_count}")),
            "cycle {i}: post-loop render should show loop count {expected_loop_count}, got:\n{}",
            outcome.prompt_after
        );
        assert!(
            outcome.prompt_after.contains(expected_confidence),
            "cycle {i}: post-loop render should show the new decision's confidence, got:\n{}",
            outcome.prompt_after
        );

        prior_state = Some(outcome.state_json);
    }
}

/// A capturing observer for event-stream assertions (rotation).
#[derive(Default)]
struct CapturingObserver {
    events: Mutex<Vec<tidepool_harness::Event>>,
}

impl tidepool_harness::Observer for CapturingObserver {
    fn on_event(&self, event: &tidepool_harness::Event) {
        self.events.lock().push(event.clone());
    }
}

/// MACHINE ROTATION: with the fragment ceiling
/// forced to 1, the second cycle's loop-boundary maintenance finds the
/// shared machine over the ceiling and quiescent, ROTATES it (fresh machine
/// under the same session id), and the cycle then runs to completion with
/// durable state flowing through the checkpoint exactly as it always has —
/// the CI-exercised reconstruction path (a recovery path that only runs in
/// emergencies is a broken recovery path). Asserts both cycles complete,
/// state accumulates ACROSS the rotation, and the event stream records
/// `MachineStats` + `MachineRotated`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn machine_rotation_between_cycles_preserves_durable_state() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();
    // Process-isolated under nextest: forcing the ceiling to 1 makes the
    // SECOND cycle's maintenance rotate (the first cycle boots the machine).
    std::env::set_var("TIDEPOOL_MACHINE_FRAGMENT_CEILING", "1");

    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");
    let replies = vec![
        decision_reply("observe", "first loop", "Medium"),
        decision_reply("decide", "second loop", "High"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let writer = tidepool_harness::log::LogWriter::create(
        std::env::temp_dir().join(format!(
            "acceptance-selfharness-rotation-{}.jsonl",
            std::process::id()
        )),
        &header(),
    )
    .expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let observer = Arc::new(CapturingObserver::default());
    let mut driver = SelfHarnessDriver::new(agent, observer.clone());
    let source = load_harness_source(&examples_harness_dir().join("Harness.hs"))
        .expect("reference harness source loads");

    let outcome1 = driver
        .run_one_loop_iteration(&source, None)
        .await
        .expect("cycle 1 (boots the machine)");
    let outcome2 = driver
        .run_one_loop_iteration(&source, Some(&outcome1.state_json))
        .await
        .expect("cycle 2 (rotates at the boundary, then completes)");

    // Durable state accumulated ACROSS the rotation.
    assert_eq!(
        outcome2.state_json.get("mode").and_then(|v| v.as_str()),
        Some("Acting"),
        "state must keep accumulating across a machine rotation, got {:?}",
        outcome2.state_json
    );

    let events = observer.events.lock();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, tidepool_harness::Event::MachineStats { .. })),
        "loop-boundary maintenance must emit MachineStats"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, tidepool_harness::Event::MachineRotated { .. })),
        "the ceiling-of-1 run must record a MachineRotated event"
    );
}
