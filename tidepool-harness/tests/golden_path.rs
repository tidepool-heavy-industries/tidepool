//! The S1 golden path, end to end, through the RECORD-REPLAY provider — zero
//! live API calls (CI-shaped). GHC-heavy tier: needs `TIDEPOOL_EXTRACT` and the
//! with-packages GHC on PATH (`--ignore-default-filter` to run).
//!
//! The thread (TARGET §3 / spike SPEC "THE GOLDEN PATH"):
//!
//!   force root → turn engine drives the (replayed) model → its block calls
//!   `runLLMTurnFork @Int "..."` → the node suspends on a FORK hole →
//!   the fork answerer is forced → its transcript is the parent's, forked at
//!   the checkpoint → ONE deliberate ill-typed attempt (`resume "nope"`)
//!   exercises the GHC-verbatim retry (continuation NOT consumed) → a valid
//!   `resume (42 :: Int)` runs via run_child against the suspended parent and
//!   resumes it → the parent's next turn calls `ask` → an OPERATOR hole →
//!   a mechanical answer consumes it → the program completes.
//!
//! Then: fold the log to the terminal tree state, and re-seed a fresh
//! `ReplayProvider` FROM the log to prove the recorded turns re-drive it.

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, Event, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Role, Usage};
use tidepool_harness::replay::{fold_tree_state, RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeState;
use tidepool_harness::Harness;

fn prelude_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| PathBuf::from("haskell/lib"))
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "golden".into(),
        extract_fingerprint: "golden".into(),
        harness_version: "test".into(),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 100,
        output_tokens: 20,
        cached_input_tokens: None,
        cache_write_tokens: None,
    }
}

/// The scripted assistant turns, IN THE ORDER the harness requests them.
fn golden_replies() -> Vec<RecordedReply> {
    // The harness serves these order-only (a deterministic single thread).
    let r = |content: &str| RecordedReply {
        content: content.to_string(),
        usage: usage(),
    };
    vec![
        // 1. Root turn: ONE do-block that forks for a number, then confirms with
        //    the operator via a dialog, then completes. The block suspends first
        //    at the fork; when resumed it suspends again at the dialog; when THAT
        //    resumes it runs to completion — all one resident fragment.
        r(
            "I'll get a number from a sub-agent, confirm it, and finish.\n\n\
           ```haskell\n\
           do\n\
           \x20 Right n <- runLLMTurnFork @Int \"pick a number between 1 and 100\"\n\
           \x20 _ <- ask (SEnum [\"yes\", \"no\"]) \"Confirm: Proceed?\"\n\
           \x20 pure (toJSON n)\n\
           ```",
        ),
        // 2. Fork answerer, DELIBERATELY ill-typed: a String where Int is wanted.
        r("```haskell\nresume \"forty-two\"\n```"),
        // 3. Fork answerer, corrected: a real Int.
        r("Right, it must be an Int.\n\n```haskell\nresume (42 :: Int)\n```"),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn golden_path_record_replay() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("golden.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(golden_replies()));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    // 1. Operator creates the root (thunk) + forces it.
    let root = harness
        .create_root("golden root", "Get a number, confirm it, finish.")
        .unwrap();
    assert_eq!(harness.tree().state(root), Some(NodeState::Thunk));
    harness.force(root, Actor::Operator).unwrap();

    // 2. Drive the root turn loop → it suspends at the fork hole.
    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to a hole");
    match outcome {
        tidepool_harness::TurnOutcome::Suspended { classified, .. } => {
            assert!(
                matches!(
                    classified.routing,
                    tidepool_harness::HoleRouting::Fork { .. }
                ),
                "root should suspend on a FORK hole, got {:?}",
                classified.routing
            );
        }
        other => panic!(
            "root should suspend at runLLMTurnFork, got a different outcome: {}",
            outcome_tag(&other)
        ),
    }
    assert!(matches!(
        harness.tree().state(root),
        Some(NodeState::Suspended { .. })
    ));

    // 3. Answer the fork: forces a child answerer, drives it (ill-typed retry +
    //    valid answer), runs it via run_child, resumes the parent. The parent's
    //    ONE do-block continuation then advances to the NEXT suspension — the
    //    ask — so after this call the parent is suspended on an ASK hole.
    let child = harness
        .answer_fork(root, Actor::Operator)
        .await
        .expect("fork answered end to end incl. the GHC retry");
    // The child answerer node is done.
    assert_eq!(harness.tree().state(child), Some(NodeState::Done));

    // 4. The parent re-suspended at the ask (same fragment, next hole).
    let dialog = harness
        .pending_hole(root)
        .expect("parent re-suspended at a hole");
    assert!(
        matches!(dialog.routing, tidepool_harness::HoleRouting::Ask { .. }),
        "parent should re-suspend on an ASK hole after the fork resumes, got {:?}",
        dialog.routing
    );
    assert!(matches!(
        harness.tree().state(root),
        Some(NodeState::Suspended { .. })
    ));

    // 5. Operator answers the ask MECHANICALLY, no model turn. The
    //    continuation then runs the block's `pure (toJSON n)` tail to
    //    completion — the whole program is one resident fragment.
    harness
        .answer_dialog(root, json!("yes"))
        .await
        .expect("mechanical dialog answer");

    // 6. The program has completed.
    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the program completes after the operator confirms"
    );

    // --- per-round usage lands on TurnDelta, every assistant turn --------------
    // Not just a branch's first turn (`Event::BranchInvocation`): each of the
    // 3 scripted assistant replies above must carry the provider's own
    // `Usage` on its own `Event::TurnDelta`, so a within-window round 2/3
    // (the fork answerer's ill-typed retry and its correction) is just as
    // measurable as round 1.
    let (_, events) = LogReader::open(&log_path).expect("open log for usage check");
    let assistant_usages: Vec<Option<Usage>> = events
        .collect::<Result<Vec<_>, _>>()
        .expect("read all events")
        .into_iter()
        .filter_map(|record| match record.event {
            Event::TurnDelta {
                role: Role::Assistant,
                usage,
                ..
            } => Some(usage),
            _ => None,
        })
        .collect();
    assert_eq!(
        assistant_usages.len(),
        3,
        "one TurnDelta per scripted assistant reply (root do-block, ill-typed \
         retry, corrected answer)"
    );
    for (i, u) in assistant_usages.iter().enumerate() {
        assert_eq!(
            *u,
            Some(usage()),
            "assistant turn {i} must carry the provider's Usage on its own \
             TurnDelta, not just the branch's first turn"
        );
    }

    // --- crash-replay + record-replay round-trip -----------------------------
    let folded = fold_tree_state(&log_path).expect("fold the log");
    assert_eq!(
        folded.states.get(&root),
        Some(&NodeState::Done),
        "the folded log restores the root as Done"
    );
    // The fork child exists in the folded tree.
    assert!(
        folded.parents.values().any(|p| *p == Some(root)),
        "the fork child references the root as parent in the folded tree"
    );

    // A fresh ReplayProvider seeded FROM the log has the recorded assistant
    // turns (3: root do-block, the ill-typed attempt, the valid answer — the
    // operator's mechanical dialog answer costs no model turn).
    let replay = ReplayProvider::from_log(&log_path).expect("replay from log");
    assert_eq!(
        replay.remaining(),
        3,
        "the log records exactly the 3 scripted assistant turns, got {}",
        replay.remaining()
    );
}

/// Isolation: the fork path alone (no dialog) — a parent that only forks for an
/// Int and returns it. Proves fork answer → resume → completion crossing the
/// GHC retry, without the dialog hole.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_only_resumes_to_completion() {
    support::require_extract();
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fork.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("cfg");

    let r = |c: &str| RecordedReply {
        content: c.to_string(),
        usage: usage(),
    };
    let replies = vec![
        r("```haskell\ndo\n  Right n <- runLLMTurnFork @Int \"pick\"\n  pure (toJSON n)\n```"),
        r("```haskell\nresume \"nope\"\n```"),   // ill-typed
        r("```haskell\nresume (7 :: Int)\n```"), // valid
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness"));

    let root = harness
        .create_root("fork only", "fork for a number")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();
    let out = harness
        .run_to_hole_or_done(root)
        .await
        .expect("drives to fork hole");
    assert!(matches!(
        out,
        tidepool_harness::TurnOutcome::Suspended { .. }
    ));

    harness
        .answer_fork(root, Actor::Operator)
        .await
        .expect("fork answered");
    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "fork-only parent completes after the answer resumes it"
    );
}

fn outcome_tag(o: &tidepool_harness::TurnOutcome) -> &'static str {
    match o {
        tidepool_harness::TurnOutcome::Completed { .. } => "Completed",
        tidepool_harness::TurnOutcome::Suspended { .. } => "Suspended",
        tidepool_harness::TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}
