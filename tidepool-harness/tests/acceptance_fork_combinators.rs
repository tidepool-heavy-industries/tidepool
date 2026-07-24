//! Wave C acceptance coverage for `Tidepool.Fork` (recursion-scheme fork
//! combinators, TARGET.md §1 D1 ruling) — through the real production path,
//! same discipline as `acceptance_fanout.rs`. Record-replay, CI-shaped, zero
//! live calls.
//!
//! `forkFilter` always answers at a fixed `Bool`, so it composes over
//! `returnControlFanout` (the SAME merged verb `acceptance_fanout.rs`
//! exercises directly) with no extra machinery.
//!
//! `forkMap`/`forkCata` need a CALLER-chosen answer type — that used to be a
//! hard extraction-pipeline wall (a library-defined wrapper's own
//! definition necessarily has that type free; extract rejects a
//! `returnControlFanout` occurrence whose answer type still carries a free
//! type variable — verified against the real extract binary with both
//! `INLINE` and an explicit call-site `SPECIALIZE` pragma, neither closed
//! the gap). Closed by the combinator-sites extract pass: `Translate.hs`
//! recognizes `forkMap`/`forkCata` by name (mirroring
//! `returnControl`/`returnControlFork`/`returnControlFanout`), capturing
//! the answer type at the USER CALL SITE — see `Tidepool.Fork`'s module
//! haddock and `jit_surface.rs`'s `works_fork_map` for the JIT-tier half of
//! this coverage.
//!
//! Coverage:
//! - `forkFilter` over 3 elements ("1", "2", "3") with scripted verdicts
//!   `[True, False, True]` — the harness answers the SAME
//!   `returnControlFanout` fanout hole `acceptance_fanout.rs` pins directly
//!   (fan badge, per-child prompts in declaration order), and the
//!   combinator's own `zip`/`filter` keeps only the `True`-verdict
//!   elements, in original order, regardless of which child the harness
//!   happened to drive.
//! - `forkCata` over a 2-level `RoseTree` (root + 3 leaf children) with a
//!   CALLER-chosen `@Int` answer type: the leaves batch into ONE fanout
//!   (fan = 3, "children before parents"), then the root's own prompt —
//!   built from the already-answered leaf verdicts ("prompts see child
//!   verdicts") — is answered as a second, singleton fanout (fan = 1) on
//!   the SAME node. The final `Int` is the sum of the leaf verdicts plus
//!   the root's own contribution, pinning that both dispatches actually
//!   fired and fed the right values through.

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

/// `forkCata @Int` over a 2-level `RoseTree` (root + 3 leaf children) — the
/// GHC-tier test the blocked leaf couldn't write (see the module doc). The
/// leaves batch into ONE fanout (fan = 3, "children before parents"); the
/// root's own prompt is built from those already-answered leaf verdicts
/// ("prompts see child verdicts") and answered as a SECOND, singleton
/// fanout (fan = 1) on the SAME node, both dispatches sharing the SAME
/// site-id (so both report the SAME "[Int]" sidecar type — the answer type
/// captured once, at forkCata's own call site). Leaf 1's first attempt is
/// deliberately ill-typed, exercising the SAME GHC-verbatim retry a plain
/// fork/fanout uses (`acceptance_fanout.rs`) — proving forkCata's
/// head-swapped call site retries exactly like a bare returnControlFanout
/// site, not some new mechanism.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forkcata_two_level_tree_batches_children_then_answers_parent() {
    if !extract_available() {
        eprintln!(
            "Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("forkcata.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let replies = vec![
        // 1. Root turn: forkCata over a root with 3 leaf children, answer
        //    type Int. Each node's prompt is the sum of its (already
        //    answered) children's verdicts, rendered as Text.
        reply(
            "I'll fold the tree bottom-up.\n\n\
             ```haskell\n\
             import Tidepool.Fork\n\
             \n\
             do\n\
             \x20 let tree = RoseTree () [RoseTree () [], RoseTree () [], RoseTree () []]\n\
             \x20 total <- forkCata @Int (\\_ childSum -> T.pack (show (sum childSum))) tree\n\
             \x20 pure (toJSON total)\n\
             ```",
        ),
        // 2. Leaf 0 (prompt \"0\", no children): verdict 1.
        reply("```haskell\nresume (1 :: Int)\n```"),
        // 3. Leaf 1 (prompt \"0\"): DELIBERATELY ill-typed first attempt.
        reply("```haskell\nresume \"nope\"\n```"),
        // 4. Leaf 1, corrected.
        reply("Right, an Int.\n\n```haskell\nresume (2 :: Int)\n```"),
        // 5. Leaf 2 (prompt \"0\"): verdict 3.
        reply("```haskell\nresume (3 :: Int)\n```"),
        // 6. Root's own turn (prompt \"6\" — sum of the 3 leaf verdicts,
        //    visibly carrying them forward): verdict 16.
        reply("The children summed to 6.\n\n```haskell\nresume (16 :: Int)\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("forkCata root", "Fold the tree bottom-up, finish.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    // First suspend: the 3 leaves, batched into ONE fanout.
    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to the leaf-batch hole");
    let first_ty = match outcome {
        tidepool_harness::TurnOutcome::Suspended { classified, .. } => match &classified.routing {
            HoleRouting::Fork {
                ty,
                fan: Some(fan),
                prompts,
                ..
            } => {
                assert_eq!(
                    *fan,
                    FanBadge::Exact { n: 3 },
                    "the 3 leaves batch into one fanout"
                );
                assert_eq!(
                    prompts,
                    &vec!["0".to_string(), "0".to_string(), "0".to_string()],
                    "each childless leaf's own prompt is \"0\" (sum of an empty child list)"
                );
                ty.clone()
            }
            other => panic!("expected a fanout Fork hole for the leaf batch, got {other:?}"),
        },
        other => panic!(
            "root should suspend on the leaf-batch fanout hole, got {}",
            outcome_tag(&other)
        ),
    };

    let leaves = harness
        .answer_fanout(root, Actor::Operator)
        .await
        .expect("leaf batch answered");
    assert_eq!(leaves.len(), 3, "one child per leaf");
    for leaf in &leaves {
        assert_eq!(harness.tree().state(*leaf), Some(NodeState::Done));
    }

    // The root is suspended AGAIN immediately (resume_parent re-published a
    // hole synchronously) — its own singleton fanout, prompt built from the
    // leaf verdicts just answered.
    let second_ty = match harness.pending_hole(root) {
        Some(classified) => match &classified.routing {
            HoleRouting::Fork {
                ty,
                fan: Some(fan),
                prompts,
                ..
            } => {
                assert_eq!(
                    *fan,
                    FanBadge::Exact { n: 1 },
                    "the root answers its own prompt as a singleton fanout"
                );
                assert_eq!(
                    prompts,
                    &vec!["6".to_string()],
                    "the root's own prompt carries the sum of the child verdicts forward"
                );
                ty.clone()
            }
            other => panic!("expected a singleton fanout Fork hole for the root, got {other:?}"),
        },
        None => panic!("root should still be suspended after the leaf batch resumes it"),
    };
    assert_eq!(
        first_ty, second_ty,
        "both dispatches share the SAME site-id, so the SAME recorded \"[Int]\" sidecar type"
    );

    let root_answerers = harness
        .answer_fanout(root, Actor::Operator)
        .await
        .expect("root's own singleton fanout answered");
    assert_eq!(
        root_answerers.len(),
        1,
        "one answerer for the root's own verdict"
    );

    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the root completes once its own singleton fanout resumes it"
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
        rendered.contains("16"),
        "the rendered total is the root's own scripted verdict (16), got: {rendered}"
    );
}
