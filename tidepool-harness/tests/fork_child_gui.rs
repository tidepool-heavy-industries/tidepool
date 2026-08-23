//! Fork-subsumes-split step 3 acceptance: a fork child gets the SAME
//! operator-GUI/tree lifecycle a `runLLMTurnBranchLabeled` child gets
//! (`tests/labeled_branch.rs`), even though nothing on the wire hands it a
//! label — `SelfHarnessDriver::fork_child_label` derives one instead. This
//! pins the three behaviors `labeled_branch.rs` pins for a wire-labeled
//! branch child, against a fork child instead: its seed reaches the gate at
//! birth, its own `askUser` routes to its OWN per-node gate (never the
//! default one), and its finalize reaches `node_finalized` at its fold.
//!
//! The fixture's derived label is known exactly: the per-loop answerer (the
//! fork's parent) has no companion `NodePath` and no registered GUI label of
//! its own, so `fork_child_label` falls back to the fixed root id `"root"`;
//! this is the first (and only) fork from that parent in the run, so
//! `idx == 0`; the brief `"explore"` is already a bare lowercase word, so it
//! slugs to itself — giving `"root/f0-explore"`.
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
use tidepool_harness::selfharness::operator::{FormShape, OperatorGate};
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
        prelude_hash: "fork-child-gui".into(),
        extract_fingerprint: "fork-child-gui".into(),
        harness_version: "test".into(),
    }
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

/// What the test gates share — same shape as `labeled_branch.rs`'s
/// `RoutingProbe`.
#[derive(Default)]
struct RoutingProbe {
    default_present_calls: AtomicUsize,
    child_present_calls: AtomicUsize,
    retired: Mutex<Vec<String>>,
    seeded: Mutex<Vec<(String, String)>>,
    finalized: Mutex<Vec<(String, String)>>,
    failed: Mutex<Vec<(String, String)>>,
}

/// The driver's default gate: registers a per-node gate ONLY for the fork
/// child's DERIVED label (see this module's doc for the exact derivation) —
/// mirrors `labeled_branch.rs`'s `DefaultGate` shape, just against a label
/// this fixture computed by hand instead of one a caller chose.
struct DefaultGate {
    probe: Arc<RoutingProbe>,
    child_label: &'static str,
}

impl OperatorGate for DefaultGate {
    fn present_form(&self, _shape: &FormShape) -> serde_json::Value {
        self.probe
            .default_present_calls
            .fetch_add(1, Ordering::SeqCst);
        json!("unexpected — the fork child's own ask must never reach the default gate")
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

/// The fork child's OWN gate — a distinct `Arc<dyn OperatorGate>` the driver
/// must resolve to via `node_gate` for every ask this node raises.
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
}

/// A fork child's `askUser` form reaches its OWN derived per-node gate
/// (never the default one), it seeds with the authored brief at birth, and
/// it finalizes and retires exactly once at its fold.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_child_asks_route_to_its_own_derived_gate_and_finalizes() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let replies = vec![
        // The per-loop answerer's own turn: fork ONE child, then finalize
        // with its answer — all one compiled block. The resume after fork
        // continues the SAME block via the session's own continuation, no
        // extra model round (same shape `acceptance_fork.rs`'s parent turn
        // uses for `forkAll` + `finalize`).
        code(
            "import Tidepool.Fork (fork)\n\n\
             do\n\
             \x20 n <- fork @Int \"explore\"\n\
             \x20 finalize @Int n :: M ()",
        ),
        // The fork child's own turn: one askUser round (a bare expression —
        // the WHOLE block — so resuming it completes the turn with no
        // further suspension). `askUser @T` is NULLARY (`Tidepool.Form.hs`).
        code("askUser @Text"),
        // The fork child's second turn: finalize with a typed Int.
        code("finalize @Int 99 :: M ()"),
    ];

    let agent_cfg = EngineConfig::from_decls(
        answerer_decls(),
        repo_root().join("haskell/lib"),
        Some(fixtures_dir()),
    )
    .expect("answerer engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let log_path =
        std::env::temp_dir().join(format!("fork-child-gui-{}.jsonl", std::process::id()));
    let writer =
        tidepool_harness::log::LogWriter::create(&log_path, &header()).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let probe = Arc::new(RoutingProbe::default());
    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_gate(Arc::new(DefaultGate {
        probe: probe.clone(),
        child_label: "root/f0-explore",
    }));

    let source = load_harness_source(&fixtures_dir().join("ForkChildGuiHarness.hs"))
        .expect("fork-child-gui fixture loads");

    let outcome = driver
        .run_one_cycle(&source, None)
        .await
        .expect("render->loop->runLLMTurn->fork(1 child, asks+finalizes)->finalize cycle");

    assert_eq!(
        outcome.state_json.get("lastValue").and_then(|v| v.as_i64()),
        Some(99),
        "the parent must resume with the fork child's finalized Int and finalize \
         with it in turn: {:?}",
        outcome.state_json
    );

    assert_eq!(
        probe.child_present_calls.load(Ordering::SeqCst),
        1,
        "the fork child's one askUser form must reach its OWN derived per-node gate"
    );
    assert_eq!(
        probe.default_present_calls.load(Ordering::SeqCst),
        0,
        "the fork child's ask must never fall through to the default gate"
    );
    assert_eq!(
        probe.retired.lock().unwrap().as_slice(),
        ["root/f0-explore".to_string()],
        "the fork child's terminate/fold point must retire exactly its own \
         derived label, exactly once"
    );

    // The node-lifecycle extensions (seed at birth, outcome at fold): the
    // seed is the AUTHORED brief carried once at birth, not the composed
    // hole card, and this script finalizes, so the fold must attribute a
    // finalized VALUE, no failure.
    let seeded = probe.seeded.lock().unwrap();
    assert_eq!(seeded.len(), 1, "exactly one seed, at birth: {seeded:?}");
    assert_eq!(seeded[0].0, "root/f0-explore");
    assert_eq!(
        seeded[0].1, "explore",
        "the seed is the AUTHORED brief, not the composed hole card: {}",
        seeded[0].1
    );

    let finalized = probe.finalized.lock().unwrap();
    assert_eq!(
        finalized.len(),
        1,
        "one finalize at the fold: {finalized:?}"
    );
    assert_eq!(finalized[0].0, "root/f0-explore");
    assert_eq!(
        finalized[0].1, "99",
        "the finalized value is the fork child's own rendered answer: {}",
        finalized[0].1
    );
    assert!(
        probe.failed.lock().unwrap().is_empty(),
        "a fork child that finalizes has no failure to attribute"
    );
}
