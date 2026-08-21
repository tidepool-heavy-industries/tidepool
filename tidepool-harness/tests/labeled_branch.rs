//! PRD 21 C5's GUI lane: `runLLMTurnBranchLabeled`'s label rides the wire
//! structurally into `HoleRouting::Branch`'s `label` field, and the driver
//! resolves it to a per-node `OperatorGate` (`present_askuser_form`'s
//! `resolve_gate`) rather than the default one. This is the SAME
//! outer-branch family as `tests/companion_context_ref.rs` — a new, minimal
//! fixture rather than extending that one's `ContextRefHarness.hs`, because
//! this scenario's script (a labeled branch, an askUser round, then round
//! exhaustion) would otherwise change what the two existing tests there
//! script/assert, and those must stay byte-identical (see this crate's
//! `outer_fanout`/`companion_context_ref` VERIFY gate).
//!
//! The branch child's answerer asks the operator ONE `askUser` question, then
//! never finalizes (this fixture's script has no further Haskell block), so
//! the branch always folds via `ExitRoundsExhausted` — decision 6's typed
//! exit, not a hard failure. That is deliberately enough: the branch's answer
//! TYPE never needs to resolve through `asks.json` (a real gap this lane
//! leaves for a future `sitedVerbs` registration in `Translate.hs`, out of
//! this crate's boundary) to prove the label routes asks to its own gate and
//! retires it once the window folds.
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::selfharness::operator::{ContinueSignal, FormShape, OperatorGate};
use tidepool_harness::tree::NodeId;
use tidepool_harness::{
    answerer_decls, load_harness_source, Harness, LogObserver, SelfHarnessDriver,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn fixtures_dir() -> PathBuf {
    repo_root().join("tidepool-harness/tests/fixtures")
}

fn header() -> tidepool_harness::log::LogHeader {
    tidepool_harness::log::LogHeader {
        prelude_hash: "labeled-branch".into(),
        extract_fingerprint: "labeled-branch".into(),
        harness_version: "test".into(),
    }
}

fn code(block: &str) -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: format!("```haskell\n{block}\n```"),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

fn prose(text: &str) -> RecordedReply {
    RecordedReply {
        node: NodeId(0),
        turn: 0,
        content: text.to_string(),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

/// What the test gates share: how many times the DEFAULT gate's
/// `present_form` was reached (must stay zero — the branch child's own ask
/// must never fall through to it), how many times the CHILD gate's was, and
/// which labels were retired.
#[derive(Default)]
struct RoutingProbe {
    default_present_calls: AtomicUsize,
    child_present_calls: AtomicUsize,
    retired: Mutex<Vec<String>>,
    seeded: Mutex<Vec<(String, String)>>,
    finalized: Mutex<Vec<(String, String)>>,
    failed: Mutex<Vec<(String, String)>>,
}

/// The driver's default gate: registers a per-node gate ONLY for the label
/// this fixture's branch uses, mirroring `WebGate::node_gate`'s shape without
/// the web crate — this crate cannot depend on `tidepool-web`.
struct DefaultGate {
    probe: Arc<RoutingProbe>,
    child_label: &'static str,
}

impl OperatorGate for DefaultGate {
    fn present_form(&self, _shape: &FormShape) -> serde_json::Value {
        self.probe
            .default_present_calls
            .fetch_add(1, Ordering::SeqCst);
        json!("unexpected — the labeled branch's own ask must never reach the default gate")
    }

    fn await_continue(&self) -> ContinueSignal {
        ContinueSignal::Continue
    }

    fn node_gate(&self, label: &str) -> Option<Arc<dyn OperatorGate>> {
        (label == self.child_label).then(|| {
            Arc::new(ChildGate {
                probe: self.probe.clone(),
            }) as Arc<dyn OperatorGate>
        })
    }

    fn retire_node(&self, label: &str) {
        self.probe.retired.lock().unwrap().push(label.to_string());
    }

    fn node_seeded(&self, label: &str, seed: &str) {
        self.probe
            .seeded
            .lock()
            .unwrap()
            .push((label.to_string(), seed.to_string()));
    }

    fn node_finalized(&self, label: &str, value: &str) {
        self.probe
            .finalized
            .lock()
            .unwrap()
            .push((label.to_string(), value.to_string()));
    }

    fn node_failed(&self, label: &str, reason: &str) {
        self.probe
            .failed
            .lock()
            .unwrap()
            .push((label.to_string(), reason.to_string()));
    }
}

/// The labeled child's OWN gate — a distinct `Arc<dyn OperatorGate>` the
/// driver must resolve to via `node_gate` for every ask this node raises.
struct ChildGate {
    probe: Arc<RoutingProbe>,
}

impl OperatorGate for ChildGate {
    fn present_form(&self, _shape: &FormShape) -> serde_json::Value {
        self.probe
            .child_present_calls
            .fetch_add(1, Ordering::SeqCst);
        json!("a scripted answer")
    }

    fn await_continue(&self) -> ContinueSignal {
        ContinueSignal::Continue
    }
}

/// A labeled branch window's `askUser` form reaches its OWN per-node gate
/// (never the default one), and the node retires (marking it done) once its
/// window folds — here via round exhaustion, since this fixture's script
/// never finalizes it (PRD 21 locked decision 6: that is DATA at the branch
/// position, not a hard failure).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn labeled_branch_child_asks_route_to_its_own_gate_and_retire_on_exit() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        // The branch's answerer: one askUser round (a bare expression — the
        // WHOLE block — so resuming it completes the turn with no further
        // suspension), then round-less prose until the budget is spent.
        // `askUser @T` is NULLARY — the form derives entirely from `T`'s own
        // `Generic` representation, no prompt argument (`Tidepool.Form.hs`).
        code("askUser @Text"),
        prose("Branch: still thinking (1)."),
        prose("Branch: still thinking (2)."),
        prose("Branch: still thinking (3)."),
        prose("Branch: still thinking (4)."),
    ];

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let log_path =
        std::env::temp_dir().join(format!("labeled-branch-{}.jsonl", std::process::id()));
    let writer =
        tidepool_harness::log::LogWriter::create(&log_path, &header()).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let probe = Arc::new(RoutingProbe::default());
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_gate(Arc::new(DefaultGate {
        probe: probe.clone(),
        child_label: "root/1-child",
    }));
    driver.set_answerer_round_caps(1, 2);

    let source = load_harness_source(&fixtures_dir().join("LabeledBranchHarness.hs"))
        .expect("labeled-branch fixture loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("the outer turn completes despite the labeled branch exiting");

    let rendered_outcome = outcome
        .state_json
        .get("outcome")
        .and_then(|v| v.as_str())
        .expect("outcome is Text")
        .to_string();
    assert!(
        rendered_outcome.starts_with("exit:round exhaustion"),
        "the branch never finalizes in this script — it must fold via the typed \
         round-exhaustion exit at its own position: {rendered_outcome}"
    );

    assert_eq!(
        probe.child_present_calls.load(Ordering::SeqCst),
        1,
        "the branch's one askUser form must reach its OWN per-node gate"
    );
    assert_eq!(
        probe.default_present_calls.load(Ordering::SeqCst),
        0,
        "the labeled child's ask must never fall through to the default gate"
    );
    assert_eq!(
        probe.retired.lock().unwrap().as_slice(),
        ["root/1-child".to_string()],
        "the node's terminate/fold point must retire exactly its own label, \
         exactly once"
    );

    // The node-lifecycle extensions (seed at birth, outcome at fold): the
    // seed is the AUTHORED brief carried once at birth; this script never
    // finalizes, so the fold must attribute a FAILURE (the typed exit's
    // rendering) and no finalized value.
    let seeded = probe.seeded.lock().unwrap();
    assert_eq!(seeded.len(), 1, "exactly one seed, at birth: {seeded:?}");
    assert_eq!(seeded[0].0, "root/1-child");
    assert!(
        seeded[0].1.contains("Branch:"),
        "the seed is the authored brief, not the composed hole card: {}",
        seeded[0].1
    );
    let failed = probe.failed.lock().unwrap();
    assert_eq!(failed.len(), 1, "one failure at the fold: {failed:?}");
    assert_eq!(failed[0].0, "root/1-child");
    assert!(
        failed[0].1.contains("ExitRoundsExhausted"),
        "the failure reason is the InvocationExit rendering: {}",
        failed[0].1
    );
    assert!(
        probe.finalized.lock().unwrap().is_empty(),
        "a window that never finalized has no value to attribute"
    );
}
