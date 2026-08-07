//! Acceptance coverage for `Tidepool.Fork.forkAll` — the `mapConcurrently`-shaped
//! surface verb over the SAME `runLLMTurnFanout` machinery `forkFilter`
//! already routes through (`Tidepool.Fork`'s haddock). Unlike `forkFilter`
//! (fixed at `Bool`), `forkAll`'s answer type is CALLER-CHOSEN
//! (`forkAll @T prompts`) — structurally identical to a bare
//! `runLLMTurnFanout @T` call, so `Translate.hs`'s existing
//! `isRunLLMTurnFanoutVar`-family recognizer (extended with `isForkAllVar`)
//! head-swaps it straight to the EXISTING `runLLMTurnFanoutSited` sibling,
//! no new `forkAllSited` needed. Record-replay, CI-shaped, zero live calls,
//! same discipline as `acceptance_fork_combinators.rs`.

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
        prelude_hash: "acceptance-forkall".into(),
        extract_fingerprint: "acceptance-forkall".into(),
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

/// `forkAll @Int` over 3 prompts: fans out in declaration order (one park,
/// three thunk children), and gathers the scripted `Int` verdicts into the
/// typed `[Int]` in original order. Also pins that the site is RECORDED with
/// the caller-chosen `[Int]` type (not the placeholder site-id-0/no-type
/// failure mode a missed head-swap would silently produce).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forkall_fans_out_and_gathers_typed_batch() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("forkall.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let replies = vec![
        reply(
            "I'll fan out to 3 sub-agents and gather typed verdicts.\n\n\
             ```haskell\n\
             import Tidepool.Fork (forkAll)\n\
             \n\
             do\n\
             \x20 xs <- forkAll @Int [\"a\", \"b\", \"c\"]\n\
             \x20 pure (toJSON (xs :: [Int]))\n\
             ```",
        ),
        reply("```haskell\nresume (1 :: Int)\n```"),
        reply("```haskell\nresume (2 :: Int)\n```"),
        reply("```haskell\nresume (3 :: Int)\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("forkAll root", "Fan out to 3 sub-agents.")
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
                    "forkAll @Int's fanout must record the caller-chosen [Int] answer type, got {ty:?}"
                );
                assert_eq!(*fan, FanBadge::Exact { n: 3 });
                assert_eq!(
                    prompts,
                    &vec!["a".to_string(), "b".to_string(), "c".to_string()],
                    "per-element prompts are carried in declaration order"
                );
            }
            other => panic!("expected a fanout Fork hole, got {other:?}"),
        },
        other => panic!(
            "root should suspend on the fanout hole, got {}",
            outcome_tag(&other)
        ),
    }

    let children = harness
        .answer_fanout(root, Actor::Operator)
        .await
        .expect("fanout answered end to end");
    assert_eq!(children.len(), 3, "one child per element");
    for child in &children {
        assert_eq!(harness.tree().state(*child), Some(NodeState::Done));
    }

    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the parent completes once the assembled [Int] resumes it"
    );

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
    assert!(
        rendered.contains('1') && rendered.contains('2') && rendered.contains('3'),
        "the rendered [Int] carries the 3 scripted verdicts, got: {rendered}"
    );
}
