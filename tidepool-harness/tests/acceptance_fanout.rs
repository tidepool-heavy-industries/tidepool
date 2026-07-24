//! B1 widen acceptance coverage for `returnControlFanout @T :: [Text] -> M [T]`
//! (`Harness::answer_fanout`) — one park, N thunk children, answers collected
//! in declaration order, resume with `[T]`. Record-replay, CI-shaped, zero
//! live calls — same production-path discipline as `golden_path.rs` /
//! `acceptance_return_control.rs`.
//!
//! Coverage: a fan of 3 prompts over `@Int`; the middle child's first
//! attempt is deliberately ill-typed (exercises the GHC-verbatim retry,
//! same as a plain fork); the final `[Int]` preserves prompt/declaration
//! order regardless of which child needed a retry.

use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::{FanBadge, NodeId, NodeState};
use tidepool_harness::{Harness, HoleRouting};

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
        prelude_hash: "acceptance-fanout".into(),
        extract_fingerprint: "acceptance-fanout".into(),
        harness_version: "test".into(),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 50,
        output_tokens: 10,
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
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)");
        return;
    }

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
             \x20 ns <- returnControlFanout @Int [\"pick 1\", \"pick 2\", \"pick 3\"]\n\
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
