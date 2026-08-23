//! B1 widen acceptance coverage for `runLLMTurnFanout @T :: [Text] -> M [T]`
//! (`Harness::answer_fanout`) — one park, N thunk children, answers collected
//! in declaration order, resume with `[T]`. Record-replay, CI-shaped, zero
//! live calls — same production-path discipline as `golden_path.rs` /
//! `acceptance_run_llm_turn.rs`.
//!
//! Coverage: a fan of 3 prompts over `@Int`; the middle child's first
//! attempt is deliberately ill-typed (exercises the GHC-verbatim retry,
//! same as a plain fork); the final `[Int]` preserves prompt/declaration
//! order regardless of which child needed a retry.

mod support;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::selfharness::operator::FormShape;
use tidepool_harness::tree::{FanBadge, NodeId, NodeState};
use tidepool_harness::{Harness, HarnessError, HoleRouting, OperatorDecision, OperatorGate};

use support::haskell_call::{fanout_bind, haskell, resume_call};

/// A test-double [`OperatorGate`] that answers `present_form` from a
/// scripted channel — the SAME production seam a real web operator resolves
/// an escalation through (`Harness::escalate_to_operator` calls
/// `present_form` exactly like this on any configured gate), just with an
/// in-process channel standing in for the HTTP `/submit` round-trip. Every
/// presented [`FormShape`] is recorded, so a test can assert on what the
/// operator would actually see (the stuck node's reason/transcript preview)
/// before answering it.
struct ScriptedGate {
    answers: Mutex<mpsc::Receiver<serde_json::Value>>,
    // A second handle onto the SAME channel `present_form` blocks reading
    // from — retraction reuses it to release a blocked call, exactly how the
    // production `WebGate` releases its own `blocking_recv` via the pending
    // ask's stored oneshot sender.
    retract_tx: mpsc::Sender<serde_json::Value>,
    received: Mutex<Vec<FormShape>>,
    retracted: Mutex<Vec<FormShape>>,
}

impl ScriptedGate {
    /// Build a gate paired with the `Sender` a test uses to answer each
    /// `present_form` call, in order, whenever it chooses to.
    fn channel() -> (Arc<Self>, mpsc::Sender<serde_json::Value>) {
        let (tx, rx) = mpsc::channel();
        (
            Arc::new(ScriptedGate {
                answers: Mutex::new(rx),
                retract_tx: tx.clone(),
                received: Mutex::new(Vec::new()),
                retracted: Mutex::new(Vec::new()),
            }),
            tx,
        )
    }
}

impl OperatorGate for ScriptedGate {
    fn present_form(&self, shape: &FormShape) -> serde_json::Value {
        self.received.lock().unwrap().push(shape.clone());
        self.answers
            .lock()
            .unwrap()
            .recv()
            .unwrap_or_else(|_| json!({}))
    }

    fn retract_form(&self, shape: &FormShape) {
        self.retracted.lock().unwrap().push(shape.clone());
        // Release a still-blocked `present_form` call — a sentinel that
        // `decode_operator_decision` cannot parse as a recognized decision,
        // matching the production contract: whichever plane actually won
        // already delivered the real decision through a DIFFERENT channel.
        let _ = self.retract_tx.send(json!({"tag": "__retracted__"}));
    }
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
        prelude_hash: "acceptance-fanout".into(),
        extract_fingerprint: "acceptance-fanout".into(),
        harness_version: "test".into(),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 50,
        output_tokens: 10,
        cached_input_tokens: None,
        cache_write_tokens: None,
    }
}

fn reply(content: &str) -> RecordedReply {
    RecordedReply {
        content: content.to_string(),
        usage: usage(),
    }
}

fn outcome_tag(o: &tidepool_harness::TurnOutcome) -> &'static str {
    match o {
        tidepool_harness::TurnOutcome::Completed { .. } => "Completed",
        tidepool_harness::TurnOutcome::Suspended { .. } => "Suspended",
        tidepool_harness::TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

/// Resolve a single-child fanout's child id STRUCTURALLY — ids are minted
/// monotonically ([`tidepool_harness::tree::NodeTree::node_ids`]'s own doc),
/// so a node id absent from `before` but present now is the fanout child,
/// regardless of what its numeric value happens to be. Avoids assuming
/// `root = NodeId(0), child = NodeId(1)`, which breaks the moment an
/// internal node is minted ahead of the fanout child. Synchronous variant:
/// the child already exists by the time the caller's `answer_fanout` call
/// returned (it was awaited to completion, even on an error return).
fn new_child_since(harness: &Harness, before: &[NodeId]) -> NodeId {
    let after = harness.tree().node_ids();
    let new_ids: Vec<NodeId> = after.into_iter().filter(|n| !before.contains(n)).collect();
    assert_eq!(
        new_ids.len(),
        1,
        "expected exactly one new node id (the fanout child), got {new_ids:?}"
    );
    new_ids[0]
}

/// As [`new_child_since`], but for a fanout driven concurrently in a spawned
/// task: polls until exactly one new node id appears (the child is created
/// partway through the task's own execution, not before it starts).
async fn wait_for_new_child(harness: &Harness, before: &[NodeId], timeout: Duration) -> NodeId {
    let mut waited = Duration::ZERO;
    loop {
        let after = harness.tree().node_ids();
        let new_ids: Vec<NodeId> = after.into_iter().filter(|n| !before.contains(n)).collect();
        match new_ids.len() {
            1 => return new_ids[0],
            0 => {
                assert!(
                    waited < timeout,
                    "no new fanout child node appeared within {timeout:?}"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
                waited += Duration::from_millis(5);
            }
            n => panic!("expected at most one new node id at a time, got {n}: {new_ids:?}"),
        }
    }
}

/// Fan of 3 over `@Int`: root suspends on a fanout hole (fan badge
/// `Exact{n:3}`, per-child prompts in declaration order); the SECOND child's
/// first attempt is ill-typed (`resume "nope"`) and retries via the same
/// GHC-verbatim mechanism a plain fork uses; the final `[Int]` preserves
/// declaration order `[1, 2, 3]` regardless of which child needed a retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fanout_of_three_preserves_order_across_a_retry() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fanout.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let replies = vec![
        // 1. Root turn: fan out 3 prompts, return the assembled list.
        reply(&format!(
            "I'll fan out three prompts for numbers.\n\n{}",
            haskell(&format!(
                "do\n  {}\n  pure (toJSON ns)",
                fanout_bind("ns", "Int", &["pick 1", "pick 2", "pick 3"])
            ))
        )),
        // 2. Child 0 ("pick 1"): valid on the first attempt.
        reply(&resume_call("(1 :: Int)")),
        // 3. Child 1 ("pick 2"): DELIBERATELY ill-typed first attempt.
        reply(&resume_call("\"nope\"")),
        // 4. Child 1, corrected.
        reply(&format!("Right, an Int.\n\n{}", resume_call("(2 :: Int)"))),
        // 5. Child 2 ("pick 3"): valid on the first attempt.
        reply(&resume_call("(3 :: Int)")),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("fanout root", "Fan out for three numbers, finish.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to a hole");
    match outcome {
        tidepool_harness::TurnOutcome::Suspended { classified, .. } => match &classified.routing {
            HoleRouting::Fork {
                ty,
                fan: Some(fan),
                prompts,
                ..
            } => {
                assert_eq!(
                    ty.as_deref(),
                    Some("[Int]"),
                    "the fanout site's recorded type is the LIST type, got {ty:?}"
                );
                assert_eq!(*fan, FanBadge::Exact { n: 3 });
                assert_eq!(
                    prompts,
                    &vec![
                        "pick 1".to_string(),
                        "pick 2".to_string(),
                        "pick 3".to_string(),
                    ],
                    "per-child prompts are carried in declaration order"
                );
            }
            other => panic!("expected a fanout Fork hole, got {other:?}"),
        },
        other => panic!(
            "root should suspend on the fanout hole, got {}",
            outcome_tag(&other)
        ),
    }
    assert!(matches!(
        harness.tree().state(root),
        Some(NodeState::Suspended { .. })
    ));

    // Answer the fanout: registers + drives 3 children in declaration order
    // (the middle one retrying past its ill-typed first attempt), then
    // resumes the parent once with the assembled [Int].
    let children = harness
        .answer_fanout(root, Actor::Operator)
        .await
        .expect("fanout answered end to end incl. the GHC retry");
    assert_eq!(children.len(), 3, "one child per prompt");
    for child in &children {
        assert_eq!(
            harness.tree().state(*child),
            Some(NodeState::Done),
            "every fanout child completes once it delivers its answer"
        );
    }

    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the parent completes once the assembled [Int] resumes it"
    );

    // The rendered completion embeds the three answers in DECLARATION order
    // (1, 2, 3), not e.g. retry-completion order — the fanout answer is
    // assembled positionally from the N children, not from whichever
    // finished driving first.
    let (_header, events) = tidepool_harness::log::LogReader::open(&log_path).expect("open log");
    let rendered = events
        .filter_map(|r| r.ok())
        .find_map(|r| match r.event {
            tidepool_harness::log::Event::NodeDone {
                node,
                result_rendered,
            } if node == root => Some(result_rendered),
            _ => None,
        })
        .expect("root's NodeDone event is in the log");
    let pos1 = rendered.find('1').expect("rendered result contains 1");
    let pos2 = rendered.find('2').expect("rendered result contains 2");
    let pos3 = rendered.find('3').expect("rendered result contains 3");
    assert!(
        pos1 < pos2 && pos2 < pos3,
        "the rendered [Int] preserves declaration order [1, 2, 3], got: {rendered}"
    );
}

/// RUNG 1 of the escalation ladder: a fanout child that exhausts its
/// `max_child_turns` budget (here set to 1, via a `NoBlock` prose-only first
/// reply) does NOT hard-fail the fan — [`Harness::answer_fanout`] (via
/// `drive_answerer_to_value`) injects ONE auto corrective-retry turn and
/// grants a small extra budget, and the child recovers on its very next
/// reply. The fan completes with its one child `Done`; no operator
/// involvement, no escalation ever appears.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fanout_child_recovers_via_rung_one_auto_retry_after_cap_exhaustion() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fanout-rung1.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let mut cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    // A budget of 1 turn per child: the first (prose-only) reply immediately
    // exhausts it, forcing the very next loop check through the escalation
    // ladder's rung 1.
    cfg.max_child_turns = 1;

    let replies = vec![
        // 1. Root turn: fan out ONE prompt.
        reply(&haskell(&format!(
            "do\n  {}\n  pure (toJSON ns)",
            fanout_bind("ns", "Int", &["pick 1"])
        ))),
        // 2. Child 0 ("pick 1"): a pure-prose reply, no ```haskell block —
        //    burns the child's entire 1-turn budget with nothing to run.
        reply("Let me think about this for a moment."),
        // 3. Child 0, after rung 1's corrective nudge: answers validly.
        reply(&resume_call("(1 :: Int)")),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("fanout rung1", "Fan out for one number, finish.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();
    harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to the fanout hole");
    assert!(matches!(
        harness.tree().state(root),
        Some(NodeState::Suspended { .. })
    ));

    let children = harness
        .answer_fanout(root, Actor::Operator)
        .await
        .expect("the fan recovers via rung 1 and completes, despite child 0's cap exhaustion");
    assert_eq!(children.len(), 1);
    for child in &children {
        assert_eq!(
            harness.tree().state(*child),
            Some(NodeState::Done),
            "the fanout child completes after recovering via rung 1"
        );
    }
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));

    // No node was ever left awaiting an operator — rung 1 alone unwedged it.
    assert!(harness.first_escalated_node().is_none());
}

/// The ABORT FLOOR: a fanout child that is STILL stuck after rung 1 (its one
/// auto-retry) escalates to rung 2 — the operator popup — and here the
/// operator aborts. The failing child must end up `Cancelled` (never left
/// `Running` — the old mid-fan hard-failure's leak), the PARENT must stay
/// `Suspended` on its original fanout hole (re-answerable, its continuation
/// never half-consumed), and the error surfaced to the fan must be typed and
/// name the failing child. The operator's decision is delivered through the
/// PRODUCTION resolve path — a [`ScriptedGate`] wired via
/// `Harness::set_escalation_gate`, standing in for a real web operator
/// answering the escalation's `present_form` ask — not a direct
/// `resolve_escalation` backdoor call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fanout_child_stuck_past_rung_one_aborts_clean_no_leaked_running_node() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fanout-abort.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let mut cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    cfg.max_child_turns = 1;

    let replies = vec![
        // 1. Root turn: fan out ONE prompt — the single resulting child's
        //    id is resolved structurally (`wait_for_new_child`), not assumed.
        reply(&haskell(&format!(
            "do\n  {}\n  pure (toJSON ns)",
            fanout_bind("ns", "Int", &["pick 1"])
        ))),
        // 2..5. Four consecutive prose-only (no ```haskell block) replies:
        //    1 to exhaust the initial 1-turn budget, 3 more to exhaust rung
        //    1's auto-retry bump (AUTO_RETRY_BUMP = 3) — the fifth check
        //    (attempts=4 >= budget=4, rung 1 already spent) escalates to
        //    rung 2.
        reply("Thinking (1)."),
        reply("Thinking (2)."),
        reply("Thinking (3)."),
        reply("Thinking (4)."),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));
    let (gate, answer_tx) = ScriptedGate::channel();
    harness.set_escalation_gate(gate.clone());

    let root = harness
        .create_root("fanout abort", "Fan out for one number, finish.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();
    harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to the fanout hole");
    let root_hole = match harness.tree().state(root) {
        Some(NodeState::Suspended { hole }) => hole,
        other => panic!("expected root suspended on its fanout hole, got {other:?}"),
    };

    let before_children = harness.tree().node_ids();
    let fan_harness = harness.clone();
    let fan_task =
        tokio::spawn(async move { fan_harness.answer_fanout(root, Actor::Operator).await });

    let child = wait_for_new_child(&harness, &before_children, Duration::from_secs(10)).await;

    // Poll for the escalation to appear (rung 1 exhausted, parked on rung 2)
    // — bounded so a regression that never escalates fails the test instead
    // of hanging it. The gate's `present_form` is already blocked on
    // `answer_tx` at this point (nothing has sent yet), so this window is
    // real, not a race against an instantly-resolving gate.
    let mut waited = Duration::ZERO;
    while harness.escalation_of(child).is_none() {
        assert!(
            waited < Duration::from_secs(10),
            "child never escalated to the operator (rung 1 should have exhausted by now)"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
        waited += Duration::from_millis(5);
    }
    let escalation = harness
        .escalation_of(child)
        .expect("just observed Some above");
    assert!(
        escalation.reason.contains("cap-exhausted"),
        "escalation reason should name cap exhaustion: {}",
        escalation.reason
    );

    // The operator answers the PRESENTED ask (production path: the gate's
    // `present_form`, not a direct `resolve_escalation` call) with Abort.
    answer_tx.send(json!({"tag": "Abort"})).unwrap();

    let outcome = fan_task.await.expect("fan task did not panic");
    match outcome {
        Err(HarnessError::Aborted { node, reason }) => {
            assert_eq!(node, child, "the typed error names the failing child");
            assert!(
                reason.contains("operator aborted"),
                "the typed error's reason names the operator abort: {reason}"
            );
        }
        other => panic!("expected HarnessError::Aborted, got {other:?}"),
    }

    // The failing child is CANCELLED, not leaked Running.
    assert!(
        matches!(
            harness.tree().state(child),
            Some(NodeState::Cancelled { .. })
        ),
        "the aborted child must be Cancelled, got {:?}",
        harness.tree().state(child)
    );

    // The parent stays Suspended on its ORIGINAL hole — never half-resumed,
    // still re-answerable.
    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Suspended { hole: root_hole }),
        "the parent must stay suspended on its untouched fanout hole"
    );

    // NO node anywhere in the tree is left Running — the leak this ladder
    // replaces is structurally impossible now.
    let all_ids = harness.tree().node_ids();
    for id in all_ids {
        assert!(
            !matches!(harness.tree().state(id), Some(NodeState::Running)),
            "node {id:?} was left Running — the old mid-fan hard-failure's leak"
        );
    }

    // The resolved escalation is cleaned up, not left dangling.
    assert!(harness.escalation_of(child).is_none());

    // The gate actually received the ask (production wiring, not a
    // coincidence): one `EscalationDecision` form naming the stuck child.
    let received = gate.received.lock().unwrap();
    assert_eq!(received.len(), 1, "exactly one escalation ask presented");
    match &received[0] {
        FormShape::Sum { type_key, doc, .. } => {
            assert_eq!(type_key, "EscalationDecision");
            assert!(
                doc.as_deref().is_some_and(|d| d.contains("cap-exhausted")),
                "the presented form's doc names the cap-exhaustion reason: {doc:?}"
            );
        }
        other => panic!("expected a Sum EscalationDecision shape, got {other:?}"),
    }
}

/// The RECOVERY floor: a fanout child STILL stuck after rung 1 escalates to
/// rung 2, and here the operator grants `AllocateMore` — through the SAME
/// production gate mechanism as the abort test above — instead of aborting.
/// The child must recover and the fan must complete, proving `AllocateMore`
/// genuinely grants a fresh turn budget via the gate-resolved path (not just
/// a direct `resolve_escalation` call).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fanout_child_stuck_past_rung_one_recovers_via_operator_allocate_more_through_gate() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fanout-allocate-more.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let mut cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    cfg.max_child_turns = 1;

    let replies = vec![
        // 1. Root turn: fan out ONE prompt.
        reply(&haskell(&format!(
            "do\n  {}\n  pure (toJSON ns)",
            fanout_bind("ns", "Int", &["pick 1"])
        ))),
        // 2..5. Same four prose-only replies as the abort test: 1 exhausts
        //    the initial budget, 3 more exhaust rung 1's auto-retry bump —
        //    the fifth check escalates to rung 2.
        reply("Thinking (1)."),
        reply("Thinking (2)."),
        reply("Thinking (3)."),
        reply("Thinking (4)."),
        // 6. After the operator grants more turns, the child answers validly
        //    on its very next attempt.
        reply(&resume_call("(1 :: Int)")),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));
    // The operator's decision is queued up-front — the gate answers as soon
    // as the escalation ask reaches it, no polling needed for recovery.
    let (gate, answer_tx) = ScriptedGate::channel();
    answer_tx
        .send(json!({"tag": "AllocateMore", "turns": 2, "steer": null}))
        .unwrap();
    harness.set_escalation_gate(gate.clone());

    let root = harness
        .create_root("fanout allocate-more", "Fan out for one number, finish.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();
    harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to the fanout hole");

    let children = harness
        .answer_fanout(root, Actor::Operator)
        .await
        .expect("the fan recovers via the operator's gate-resolved AllocateMore");
    assert_eq!(
        children.len(),
        1,
        "expected exactly one fanout child, got {children:?}"
    );
    let child = children[0];
    assert_eq!(
        harness.tree().state(child),
        Some(NodeState::Done),
        "the escalated child completes after the operator grants more turns"
    );
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));

    // Escalation state is cleaned up once resolved.
    assert!(harness.escalation_of(child).is_none());

    // The gate really was consulted (production path, not a coincidence).
    let received = gate.received.lock().unwrap();
    assert_eq!(received.len(), 1, "exactly one escalation ask presented");
}

/// The TIMEOUT FLOOR: a fanout child escalates to rung 2 and NOBODY ever
/// resolves it (no gate configured, no `resolve_escalation` call) — the
/// bounded wait must fail loud with `HarnessError::EscalationTimeout` rather
/// than hang the turn forever. Uses `EngineConfig::escalation_timeout`'s
/// test-injectable override (a few milliseconds), never a real multi-minute
/// wait. The cleanup contract matches the abort floor: the child ends up
/// `Cancelled`, the parent stays `Suspended` on its original hole.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fanout_child_stuck_past_rung_one_times_out_with_no_operator() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fanout-timeout.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let mut cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    cfg.max_child_turns = 1;
    // Test-injectable override: a bounded wait measured in milliseconds, not
    // the 20-minute production default.
    cfg.escalation_timeout = Duration::from_millis(100);

    let replies = vec![
        reply(&haskell(&format!(
            "do\n  {}\n  pure (toJSON ns)",
            fanout_bind("ns", "Int", &["pick 1"])
        ))),
        reply("Thinking (1)."),
        reply("Thinking (2)."),
        reply("Thinking (3)."),
        reply("Thinking (4)."),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));
    // No gate configured — mirrors a production harness with an unreachable
    // or never-attended operator.

    let root = harness
        .create_root("fanout timeout", "Fan out for one number, finish.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();
    harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to the fanout hole");
    let root_hole = match harness.tree().state(root) {
        Some(NodeState::Suspended { hole }) => hole,
        other => panic!("expected root suspended on its fanout hole, got {other:?}"),
    };

    let before_children = harness.tree().node_ids();
    let outcome = harness.answer_fanout(root, Actor::Operator).await;
    let child = new_child_since(&harness, &before_children);
    match outcome {
        Err(HarnessError::EscalationTimeout { node, waited }) => {
            assert_eq!(node, child, "the typed timeout error names the stuck child");
            assert!(
                waited >= Duration::from_millis(100),
                "the reported wait matches the configured override: {waited:?}"
            );
        }
        other => panic!("expected HarnessError::EscalationTimeout, got {other:?}"),
    }

    // Same cleanup contract as an operator abort: no leaked Running node, the
    // child is Cancelled, and the parent stays re-answerable on its original
    // hole.
    assert!(
        matches!(
            harness.tree().state(child),
            Some(NodeState::Cancelled { .. })
        ),
        "the timed-out child must be Cancelled, got {:?}",
        harness.tree().state(child)
    );
    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Suspended { hole: root_hole }),
        "the parent must stay suspended on its untouched fanout hole"
    );
    assert!(harness.escalation_of(child).is_none());
}

/// High-3 regression: the DIRECT resolution plane
/// (`Harness::resolve_escalation`) winning the race against a configured
/// gate must not leave the gate's own presentation live. Before this fix,
/// `escalate_to_operator` never told the losing `spawn_blocking(gate.
/// present_form)` call that the decision had already arrived through `rx` —
/// the gate's ask stayed published, and its blocked call was never released.
/// Here the gate's `present_form` is deliberately left unanswered (nobody
/// ever sends on `answer_tx`); the ONLY thing that resolves the escalation
/// is a direct `resolve_escalation` call — the same "test, or emergency
/// admin override" path the method's own doc names. `retract_form` must
/// still fire, naming the exact shape the gate was presenting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fanout_child_escalation_retracts_its_gate_ask_when_the_direct_plane_wins() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fanout-retract-direct.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let mut cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    cfg.max_child_turns = 1;

    let replies = vec![
        reply(&haskell(&format!(
            "do\n  {}\n  pure (toJSON ns)",
            fanout_bind("ns", "Int", &["pick 1"])
        ))),
        reply("Thinking (1)."),
        reply("Thinking (2)."),
        reply("Thinking (3)."),
        reply("Thinking (4)."),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));
    // The gate is configured (its present_form WILL be called), but never
    // answered through it — answer_tx is held and never sent on.
    let (gate, _answer_tx) = ScriptedGate::channel();
    harness.set_escalation_gate(gate.clone());

    let root = harness
        .create_root("fanout retract direct", "Fan out for one number, finish.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();
    harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to the fanout hole");

    let before_children = harness.tree().node_ids();
    let fan_harness = harness.clone();
    let fan_task =
        tokio::spawn(async move { fan_harness.answer_fanout(root, Actor::Operator).await });

    let child = wait_for_new_child(&harness, &before_children, Duration::from_secs(10)).await;

    let mut waited = Duration::ZERO;
    while harness.escalation_of(child).is_none() {
        assert!(
            waited < Duration::from_secs(10),
            "child never escalated to the operator"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
        waited += Duration::from_millis(5);
    }

    // The DIRECT plane resolves it — the gate's own present_form call is
    // still blocked in `recv()` at this point, nobody has sent it anything.
    harness
        .resolve_escalation(child, OperatorDecision::Abort)
        .expect("a rung-2 escalation is pending for the child");

    let outcome = fan_task.await.expect("fan task did not panic");
    assert!(
        matches!(outcome, Err(HarnessError::Aborted { node, .. }) if node == child),
        "the direct plane's decision must still resolve the escalation, got {outcome:?}"
    );

    // The gate's own ask was presented (the race was real, not a bypass)...
    let received = gate.received.lock().unwrap().clone();
    assert_eq!(received.len(), 1, "the gate's own ask was presented");
    // ...and retracted with that EXACT shape once the direct plane won.
    let retracted = gate.retracted.lock().unwrap().clone();
    assert_eq!(
        retracted, received,
        "retract_form must be called with the exact shape the gate was presenting"
    );
}

/// High-3 regression, timeout variant: when NEITHER plane resolves an
/// escalation before `escalation_timeout` elapses, the gate's own live
/// presentation must be retracted too — otherwise the timeline keeps
/// showing an actionable ask for a turn the harness has already given up
/// on and cancelled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fanout_child_escalation_retracts_its_gate_ask_on_timeout() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fanout-retract-timeout.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let mut cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    cfg.max_child_turns = 1;
    cfg.escalation_timeout = Duration::from_millis(100);

    let replies = vec![
        reply(&haskell(&format!(
            "do\n  {}\n  pure (toJSON ns)",
            fanout_bind("ns", "Int", &["pick 1"])
        ))),
        reply("Thinking (1)."),
        reply("Thinking (2)."),
        reply("Thinking (3)."),
        reply("Thinking (4)."),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));
    let (gate, _answer_tx) = ScriptedGate::channel();
    harness.set_escalation_gate(gate.clone());

    let root = harness
        .create_root("fanout retract timeout", "Fan out for one number, finish.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();
    harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to the fanout hole");

    let before_children = harness.tree().node_ids();
    let outcome = harness.answer_fanout(root, Actor::Operator).await;
    let child = new_child_since(&harness, &before_children);
    assert!(
        matches!(outcome, Err(HarnessError::EscalationTimeout { node, .. }) if node == child),
        "expected a typed timeout, got {outcome:?}"
    );

    let received = gate.received.lock().unwrap().clone();
    assert_eq!(received.len(), 1, "the gate's own ask was presented");
    let retracted = gate.retracted.lock().unwrap().clone();
    assert_eq!(
        retracted, received,
        "a timed-out escalation must retract the gate's own live ask, \
         not just abandon it"
    );
}
