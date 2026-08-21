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

use std::sync::Arc;
use std::time::Duration;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::{FanBadge, NodeId, NodeState};
use tidepool_harness::{Harness, HarnessError, HoleRouting, OperatorDecision};

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
        node: NodeId(0),
        turn: 0,
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
        reply(
            "I'll fan out three prompts for numbers.\n\n\
             ```haskell\n\
             do\n\
             \x20 ns <- mapM liftEither =<< runLLMTurnFanout @Int [\"pick 1\", \"pick 2\", \"pick 3\"]\n\
             \x20 pure (toJSON ns)\n\
             ```",
        ),
        // 2. Child 0 ("pick 1"): valid on the first attempt.
        reply("```haskell\nresume (1 :: Int)\n```"),
        // 3. Child 1 ("pick 2"): DELIBERATELY ill-typed first attempt.
        reply("```haskell\nresume \"nope\"\n```"),
        // 4. Child 1, corrected.
        reply("Right, an Int.\n\n```haskell\nresume (2 :: Int)\n```"),
        // 5. Child 2 ("pick 3"): valid on the first attempt.
        reply("```haskell\nresume (3 :: Int)\n```"),
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
        reply(
            "```haskell\n\
             do\n\
             \x20 ns <- mapM liftEither =<< runLLMTurnFanout @Int [\"pick 1\"]\n\
             \x20 pure (toJSON ns)\n\
             ```",
        ),
        // 2. Child 0 ("pick 1"): a pure-prose reply, no ```haskell block —
        //    burns the child's entire 1-turn budget with nothing to run.
        reply("Let me think about this for a moment."),
        // 3. Child 0, after rung 1's corrective nudge: answers validly.
        reply("```haskell\nresume (1 :: Int)\n```"),
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
/// name the failing child. The operator-decision oneshot is fired directly
/// here (`Harness::resolve_escalation`), simulating the web popup's
/// `/steer/:node/abort` POST without a browser.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fanout_child_stuck_past_rung_one_aborts_clean_no_leaked_running_node() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fanout-abort.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let mut cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    cfg.max_child_turns = 1;

    let replies = vec![
        // 1. Root turn: fan out ONE prompt (so the stuck child's id is
        //    predictable: root = NodeId(0), child = NodeId(1)).
        reply(
            "```haskell\n\
             do\n\
             \x20 ns <- mapM liftEither =<< runLLMTurnFanout @Int [\"pick 1\"]\n\
             \x20 pure (toJSON ns)\n\
             ```",
        ),
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

    let child = NodeId(1);
    let fan_harness = harness.clone();
    let fan_task =
        tokio::spawn(async move { fan_harness.answer_fanout(root, Actor::Operator).await });

    // Poll for the escalation to appear (rung 1 exhausted, parked on rung 2)
    // — bounded so a regression that never escalates fails the test instead
    // of hanging it.
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

    // Simulate the web popup's abort control firing directly.
    harness
        .resolve_escalation(child, OperatorDecision::Abort)
        .expect("the escalation is pending; resolving it must succeed");

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
    let (all_ids, _) = harness.tree().node_ids_after(None, usize::MAX);
    for id in all_ids {
        assert!(
            !matches!(harness.tree().state(id), Some(NodeState::Running)),
            "node {id:?} was left Running — the old mid-fan hard-failure's leak"
        );
    }

    // The resolved escalation is cleaned up, not left dangling.
    assert!(harness.escalation_of(child).is_none());
}
