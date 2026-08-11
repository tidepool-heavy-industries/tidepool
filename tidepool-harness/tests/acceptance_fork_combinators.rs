//! Wave C acceptance coverage for `Tidepool.Fork` (recursion-scheme fork
//! combinators, TARGET.md §1 D1 ruling) — through the real production path,
//! same discipline as `acceptance_fanout.rs`. Record-replay, CI-shaped, zero
//! live calls.
//!
//! `forkFilter` always answers at a fixed `Bool`; it composes over the `Fork`
//! effect's fanout with no extra machinery. (test-diet, coverage-overlap
//! census: the harness-level `forkFilter` acceptance test that lived here was
//! deleted — its semantics [keeps True verdicts, declaration order] are
//! pinned at the JIT tier by `jit_surface::works_fork`, and its fanout hole
//! shape [FanBadge, per-child prompts in declaration order] is pinned by
//! `acceptance_fanout.rs` via the same `runLLMTurnFanout` machinery
//! `forkFilter` composes over.)
//!
//! `forkMap`/`forkCata` need a CALLER-chosen answer type. `Translate.hs`
//! recognizes them by name (like `fork`/`forkAll`) and head-swaps each call
//! site to its `Fork`-effect `*Sited` sibling, capturing the answer type at
//! the USER CALL SITE — see `Tidepool.Fork`'s module haddock and
//! `jit_surface.rs`'s `works_fork_map` for the JIT-tier half of this coverage.
//!
//! Coverage:
//! - `forkCata` over a 2-level `RoseTree` (root + 3 leaf children) with a
//!   CALLER-chosen `@Int` answer type: the leaves batch into ONE fanout
//!   (fan = 3, "children before parents"), then the root's own prompt —
//!   built from the already-answered leaf verdicts ("prompts see child
//!   verdicts") — is answered as a second, singleton fanout (fan = 1) on
//!   the SAME node. The final `Int` is the sum of the leaf verdicts plus
//!   the root's own contribution, pinning that both dispatches actually
//!   fired and fed the right values through.
//! - A user's OWN `forkMap` — an unrelated, monomorphic local helper that
//!   merely shares the combinator's occurrence name (never imports
//!   `Tidepool.Fork`, never calls `runLLMTurnFanout`) — must NOT abort
//!   extract. `Translate.hs`'s recognizer matches by unqualified occurrence
//!   name only, so before the fork-catchall-fallthrough fix ANY Var named
//!   `forkMap`/`forkCata` that didn't match the exact recognized shape
//!   ([Type, Type] + 2 value args) hit a catch-all `error`, aborting the
//!   WHOLE eval. The recognizer now falls through to ordinary Var/App
//!   translation for a mis-shaped occurrence, mirroring the
//!   `runLLMTurn`/`runLLMTurnFork`/`runLLMTurnFanout` arm's own
//!   fall-through convention.

mod support;

use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::{FanBadge, NodeId, NodeState};
use tidepool_harness::{Harness, HoleRouting};

fn prelude_dir() -> std::path::PathBuf {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| std::path::PathBuf::from("haskell/lib"))
}

/// A Fork-capable stack: the general eval decls, whose roster tail declares
/// the `Fork` effect (tag 11), so `Tidepool.Fork`'s `forkMap`/`forkCata`
/// (which lower to `Fork`) resolve.
fn fork_cfg() -> EngineConfig {
    let decls = tidepool_mcp::standard_decls();
    EngineConfig::from_decls(decls, prelude_dir(), None).expect("engine config")
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

/// `forkCata @Int` over a 2-level `RoseTree` (root + 3 leaf children) — the
/// GHC-tier test the blocked leaf couldn't write (see the module doc). The
/// leaves batch into ONE fanout (fan = 3, "children before parents"); the
/// root's own prompt is built from those already-answered leaf verdicts
/// ("prompts see child verdicts") and answered as a SECOND, singleton
/// fanout (fan = 1) on the SAME node, both dispatches sharing the SAME
/// site-id (so both report the SAME "[Int]" sidecar type — the answer type
/// captured once, at forkCata's own call site).
///
/// TRIMMED (test-diet, coverage-overlap census): a leaf's ill-typed-first-
/// attempt retry used to be scripted here too, but by this test's own doc
/// comment the retry is "not some new mechanism" — forkCata's head-swapped
/// call site retries exactly like a bare `runLLMTurnFanout` site, which
/// `acceptance_fanout.rs` already exercises directly. Every leaf now answers
/// validly on its first attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forkcata_two_level_tree_batches_children_then_answers_parent() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("forkcata.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = fork_cfg();

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
        // 3. Leaf 1 (prompt \"0\"): verdict 2.
        reply("```haskell\nresume (2 :: Int)\n```"),
        // 4. Leaf 2 (prompt \"0\"): verdict 3.
        reply("```haskell\nresume (3 :: Int)\n```"),
        // 5. Root's own turn (prompt \"6\" — sum of the 3 leaf verdicts,
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

/// A user's OWN `forkMap` — a plain, monomorphic top-level function in the
/// user's OWN project library module, unrelated to `Tidepool.Fork`, that
/// merely shares its occurrence name — must NOT abort extract. Regression
/// test for the fork-catchall-fallthrough fix.
///
/// `Translate.hs`'s forkMap/forkCata recognizer matches by UNQUALIFIED
/// occurrence name only (see `isForkMapVar`'s haddock), not by module —
/// before the fix, ANY Var named `forkMap`/`forkCata` that didn't match the
/// exact recognized shape ([Type, Type] + 2 value args) hit a catch-all
/// `error`, aborting the WHOLE eval. This is deliberately a TOP-LEVEL,
/// EXPORTED function in its own module (not an inline `let` in the eval
/// body): an inline local binding gets renamed with a unique-keyed suffix
/// by `externalizeInternalTops` (`GhcPipeline.hs`'s #313 fix for top-level
/// float collisions) before `Translate.hs` ever sees it, so it can never
/// actually collide with the recognizer's plain-name check — a genuine
/// module-level export (an EXTERNAL name, never renamed) is what a "user's
/// own forkMap" collision looks like in practice, e.g. a `.tidepool/lib`
/// project module. Verified against the real (pre-fix) catch-all: this
/// exact construction aborts extract with "forkMap site in forkMap is not
/// fully applied..." on the unpatched recognizer, and completes cleanly
/// once the catch-all is replaced with a fall-through.
///
/// The library's `forkMap` is single-type-variable (`forall a. (a -> a) ->
/// [a] -> [a]`), so its call site carries only ONE type argument — it can
/// never match the real combinator's `[Type, Type]` + 2-value-arg shape,
/// and must fall through to ordinary Var/App translation, running as an
/// ordinary recursive function.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_defined_forkmap_does_not_abort_extract() {
    support::require_extract();

    let lib_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        lib_dir.path().join("MyLib.hs"),
        "module MyLib (forkMap) where\n\
         import Prelude\n\
         \n\
         -- | A plain helper: nothing to do with Tidepool.Fork.\n\
         forkMap :: (a -> a) -> [a] -> [a]\n\
         forkMap f xs = case xs of\n\
         \x20 [] -> []\n\
         \x20 (y : ys) -> f y : forkMap f ys\n",
    )
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("user_forkmap.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), Some(lib_dir.path().to_path_buf()))
        .expect("engine config");

    let replies = vec![reply(
        "This is my own project's `forkMap` — a plain recursive map, \
         nothing to do with `Tidepool.Fork`. No `runLLMTurnFanout` \
         involved.\n\n\
         ```haskell\n\
         import MyLib (forkMap)\n\
         \n\
         pure (toJSON (forkMap (+1) [1, 2, 3 :: Int]))\n\
         ```",
    )];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("user forkMap root", "Call my own project's forkMap.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root completes without extract aborting on the name collision");
    match outcome {
        tidepool_harness::TurnOutcome::Completed { rendered } => {
            assert!(
                rendered.contains('2') && rendered.contains('3') && rendered.contains('4'),
                "the user's forkMap runs as an ordinary function, [1,2,3] -> \
                 [2,3,4], got: {rendered}"
            );
        }
        other => panic!(
            "a user-defined forkMap must complete normally (no suspend, no \
             abort), got {}",
            outcome_tag(&other)
        ),
    }

    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the root completes normally — the name collision with Tidepool.Fork's \
         forkMap never touches the runLLMTurnFanout suspend machinery"
    );
}
