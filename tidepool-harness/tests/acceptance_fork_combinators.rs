//! Wave C acceptance coverage for `Tidepool.Fork` (recursion-scheme fork
//! combinators, TARGET.md §1 D1 ruling) — through the real production path,
//! same discipline as `acceptance_fanout.rs`. Record-replay, CI-shaped, zero
//! live calls.
//!
//! Only `forkFilter` ships (see `Tidepool.Fork`'s module haddock and
//! `jit_surface.rs`'s `works_fork` for the full finding): `forkMap`/
//! `forkCata` would need a CALLER-chosen answer type to reach
//! `returnControlFanout` monomorphically, and extract has no mechanism to
//! duplicate a library-defined wrapper's definition into each call site —
//! verified against the real extract binary with both `INLINE` and an
//! explicit call-site `SPECIALIZE` pragma, neither closes the gap.
//! `forkFilter` always answers at a fixed `Bool`, so it composes over
//! `returnControlFanout` (the SAME merged verb `acceptance_fanout.rs`
//! exercises directly) with no such requirement.
//!
//! Coverage: `forkFilter` over 3 elements ("1", "2", "3") with scripted
//! verdicts `[True, False, True]` — the harness answers the SAME
//! `returnControlFanout` fanout hole `acceptance_fanout.rs` pins directly
//! (fan badge, per-child prompts in declaration order), and the combinator's
//! own `zip`/`filter` keeps only the `True`-verdict elements, in original
//! order, regardless of which child the harness happened to drive.

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
        prelude_hash: "acceptance-fork-combinators".into(),
        extract_fingerprint: "acceptance-fork-combinators".into(),
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

/// `forkFilter` over 3 elements: fans out prompts "1"/"2"/"3" (in
/// declaration order), scripted verdicts `[True, False, True]` — the final
/// `[Int]` keeps only 1 and 3, in original order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forkfilter_keeps_true_verdicts_in_declaration_order() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fork_combinators.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let replies = vec![
        // 1. Root turn: forkFilter over [1,2,3], keep the True verdicts.
        reply(
            "I'll judge each number and keep the ones that pass.\n\n\
             ```haskell\n\
             import Tidepool.Fork\n\
             \n\
             do\n\
             \x20 ys <- forkFilter (\\x -> T.pack (show (x :: Int))) [1, 2, 3 :: Int]\n\
             \x20 pure (toJSON (ys :: [Int]))\n\
             ```",
        ),
        // 2. Child 0 ("1"): keep it.
        reply("```haskell\nresume True\n```"),
        // 3. Child 1 ("2"): drop it.
        reply("```haskell\nresume False\n```"),
        // 4. Child 2 ("3"): keep it.
        reply("```haskell\nresume True\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("forkFilter root", "Judge 1, 2, 3 and keep the good ones.")
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
                    Some("[Bool]"),
                    "forkFilter's fanout answers a fixed [Bool], got {ty:?}"
                );
                assert_eq!(*fan, FanBadge::Exact { n: 3 });
                assert_eq!(
                    prompts,
                    &vec!["1".to_string(), "2".to_string(), "3".to_string()],
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
        assert_eq!(
            harness.tree().state(*child),
            Some(NodeState::Done),
            "every fanout child completes once it delivers its answer"
        );
    }

    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the parent completes once the assembled [Bool] resumes it and forkFilter's own zip/filter runs"
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
        rendered.contains('1') && rendered.contains('3'),
        "the rendered [Int] keeps the True-verdict elements 1 and 3, got: {rendered}"
    );
    assert!(
        !rendered.contains('2'),
        "the rendered [Int] must drop the False-verdict element 2, got: {rendered}"
    );
}
