//! PRD §11 acceptance coverage for the IN-CONTEXT `returnControl` path
//! (`Harness::answer_return_control`) — spec-flagged as "built but not
//! test-exercised" (FREEZES.md "Known-thin"). Record-replay, CI-shaped, zero
//! live calls — same harness (production entry point), same GHC-tier gate as
//! `golden_path.rs`.
//!
//! Coverage map (plans/harness-r0/PRD.md §11 / WIDEN.md §A3):
//!   - hole publishes with rendered type -> return_control_end_to_end_...
//!   - ill-typed resume rejected verbatim, continuation intact -> return_control_end_to_end_...
//!   - answer_return_control exercised end-to-end -> return_control_end_to_end_...
//!   - bottom answer does not consume -> return_control_bottom_answer_...
//!     (this one surfaced a production-path finding — see its doc comment)

use std::sync::Arc;

use serde_json::json;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, AnswerOutcome, Event, LogHeader, LogReader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::{NodeId, NodeState};
use tidepool_harness::{Harness, HoleRouting, Ui};

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
        prelude_hash: "acceptance-rc".into(),
        extract_fingerprint: "acceptance-rc".into(),
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

/// Node-filtered raw log events, in `seq` order — for asserting the DURABLE
/// record (not just in-memory state), same technique as `forcing.rs`'s
/// consent-integrity unit tests.
fn events_for(log_path: &std::path::Path, node: NodeId) -> Vec<Event> {
    let (_h, events) = LogReader::open(log_path).expect("open log");
    events
        .map(|r| r.expect("well-formed record").event)
        .filter(|e| event_node(e) == Some(node))
        .collect()
}

fn outcome_tag(o: &tidepool_harness::TurnOutcome) -> &'static str {
    match o {
        tidepool_harness::TurnOutcome::Completed { .. } => "Completed",
        tidepool_harness::TurnOutcome::Suspended { .. } => "Suspended",
        tidepool_harness::TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

fn event_node(e: &Event) -> Option<NodeId> {
    match e {
        Event::NodeCreated { node, .. }
        | Event::Forced { node, .. }
        | Event::TurnStart { node, .. }
        | Event::Effect { node, .. }
        | Event::HolePublished { node, .. }
        | Event::HoleAnswerAttempt { node, .. }
        | Event::HoleConsumed { node, .. }
        | Event::NodeDone { node, .. }
        | Event::NodeCancelled { node, .. }
        | Event::TurnDelta { node, .. }
        | Event::TurnForked { node, .. }
        | Event::TurnSpliced { node, .. } => Some(*node),
    }
}

/// `returnControl @Int "..."` (NOT `returnControlFork`) — the same node
/// answers in its own context. `answer_return_control` has never been
/// exercised by any existing test (golden_path.rs and return_control_sidecar.rs
/// both drive the FORK verb, or the extract-only rejection paths).
///
/// This test drives the full arc through the Harness (the production entry
/// point, not `fold_tree_state` or a hand-built `NodeTree`):
///
///   1. root suspends on a `returnControl @Int` hole — asserts the published
///      hole's routing carries the RENDERED type ("Int") from the asks.json
///      sidecar (A1).
///   2. `answer_return_control` drives the SAME node's own turn loop to an
///      answer. Its first attempt (`resume "nope"`) is ill-typed — asserts
///      the retry is a REJECTED `HoleAnswerAttempt` (continuation intact: no
///      `HoleConsumed` yet, same hole id throughout) whose logged error text
///      is the GHC compiler's own diagnostic (not a synthesized message) and
///      is fed back to the model VERBATIM as its next user turn.
///   3. the corrected attempt (`resume (42 :: Int)`) compiles, resumes the
///      parent, and the node completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn return_control_end_to_end_type_retry_and_answer() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("rc.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let replies = vec![
        reply(
            "```haskell\ndo\n  n <- returnControl @Int \"pick a number between 1 and 100\"\n  \
             pure (toJSON n)\n```",
        ),
        // Deliberately ill-typed: a String where Int is wanted.
        reply("```haskell\nresume \"nope\"\n```"),
        // Corrected.
        reply("Right, an Int.\n\n```haskell\nresume (42 :: Int)\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("rc root", "Get a number in-context, finish.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to a hole");
    let hole_before = match outcome {
        tidepool_harness::TurnOutcome::Suspended { hole, classified, .. } => {
            match &classified.routing {
                HoleRouting::ReturnControl { ty, .. } => {
                    assert_eq!(
                        ty.as_deref(),
                        Some("Int"),
                        "the published hole must carry the RENDERED answer type from asks.json, got {ty:?}"
                    );
                }
                other => panic!("expected a ReturnControl hole, got {other:?}"),
            }
            hole
        }
        other => panic!("root should suspend at returnControl, got {}", outcome_tag(&other)),
    };
    assert!(matches!(
        harness.tree().state(root),
        Some(NodeState::Suspended { .. })
    ));

    // Drive the in-context answer: ill-typed retry, then the valid answer.
    harness
        .answer_return_control(root)
        .await
        .expect("return-control answered end to end incl. the GHC retry");

    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the node completes once the corrected answer resumes it"
    );

    // --- durable-log assertions: continuation intact across the bad attempt ---
    let events = events_for(&log_path, root);

    let published: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::HolePublished { .. }))
        .collect();
    assert_eq!(
        published.len(),
        1,
        "exactly one hole is ever published on this node — the ill-typed \
         attempt must NOT re-publish or mint a new hole, got {published:?}"
    );
    let Event::HolePublished { hole: published_hole, ty, .. } = published[0] else {
        unreachable!()
    };
    assert_eq!(published_hole.0, hole_before, "published hole id matches the classified outcome");
    assert_eq!(ty.as_deref(), Some("Int"));

    let attempts: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::HoleAnswerAttempt { .. }))
        .collect();
    assert_eq!(
        attempts.len(),
        2,
        "one rejected attempt, one consuming attempt, got {attempts:?}"
    );
    let Event::HoleAnswerAttempt {
        hole: h0,
        outcome: AnswerOutcome::Rejected { error },
        ..
    } = attempts[0]
    else {
        panic!("first attempt must be Rejected, got {:?}", attempts[0]);
    };
    assert_eq!(h0.0, hole_before, "the rejected attempt references the SAME hole (not consumed)");
    // The rejected attempt's logged error is the GHC compiler's OWN
    // diagnostic (extract's stdout+stderr), not a harness-synthesized
    // message — it must name the mismatched types.
    assert!(
        error.contains("Int") || error.contains("Char"),
        "the logged rejection must be GHC's verbatim type-mismatch diagnostic, got:\n{error}"
    );

    let Event::HoleAnswerAttempt {
        hole: h1,
        outcome: AnswerOutcome::Consumed,
        ..
    } = attempts[1]
    else {
        panic!("second attempt must be Consumed, got {:?}", attempts[1]);
    };
    assert_eq!(h1.0, hole_before, "the consuming attempt is on the SAME hole the node suspended on");

    let consumed: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::HoleConsumed { .. }))
        .collect();
    assert_eq!(consumed.len(), 1, "hole consumed exactly once, and only by the valid attempt");

    // The retry prompt fed back to the model is the error VERBATIM (F3: "GHC
    // error text is the retry prompt, verbatim") — find the next User turn
    // after the assistant's ill-typed attempt and assert it embeds the exact
    // rejected error text.
    let turns: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::TurnDelta { .. }))
        .collect();
    let retry_turn = turns
        .iter()
        .find_map(|e| match e {
            Event::TurnDelta {
                role: tidepool_harness::provider::Role::User,
                content,
                ..
            } if content.contains("did not compile") => Some(content.clone()),
            _ => None,
        })
        .expect("a retry-prompt user turn must exist");
    assert!(
        retry_turn.contains(error.as_str()),
        "the retry prompt must embed the GHC error VERBATIM, got:\n{retry_turn}\n---\nexpected substring:\n{error}"
    );

    assert!(
        events.iter().any(|e| matches!(e, Event::NodeDone { .. })),
        "the node reaches NodeDone after the valid answer"
    );
}

/// A deliberately BOTTOM answer (`error "boom"`) to a `returnControl @Int`
/// hole. `error "boom" :: Int` TYPE-CHECKS (extract accepts it — `error` is
/// `forall a. ... -> a`), so this is NOT the ill-typed-retry path (that one's
/// covered above); the fault is a RUNTIME error.
///
/// OBSERVED BEHAVIOR (verified by running this against the real GHC/JIT
/// pipeline): forcing `error "boom"` faults INSIDE `run_child`'s evaluation of
/// the answerer's own block, before `Harness::resume_parent` is ever called.
/// `drive_answerer_to_value`'s runtime-fault branch (harness.rs, the `Err(e)`
/// arm after `run_child`) feeds `"The answer failed at runtime: {e}. Try
/// again."` back to the answerer and loops — the SAME retry shape as an
/// ill-typed compile failure. So: the continuation genuinely is NOT consumed
/// by a bottom answer; `HoleConsumed` and `NodeDone` are correctly withheld
/// until a valid answer resumes it. A5 ("forced to NF before consumption")
/// reads as satisfied for this case, at least incidentally — WHNF-forcing an
/// `Int` during the answerer's own eval is enough to trip `error` before it
/// ever reaches the parent.
///
/// AUDIT FIX (B1 widen): the runtime-fault branch now logs a `Rejected`
/// `HoleAnswerAttempt` too, same as the compile-failure branch — the durable
/// audit trail now shows every attempt (bottom answers included), not just
/// the one that eventually consumes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn return_control_bottom_answer_faults_before_consumption_and_retries() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("bottom.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let replies = vec![
        reply(
            "```haskell\ndo\n  n <- returnControl @Int \"pick a number\"\n  pure (toJSON n)\n```",
        ),
        // Type-checks (Int), but forcing it is a Haskell `error` call — a
        // RUNTIME fault, not a compile rejection.
        reply("```haskell\nresume (error \"boom\")\n```"),
        // The retry: a real Int.
        reply("Sorry — here's a real one.\n\n```haskell\nresume (7 :: Int)\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness.create_root("bottom root", "Get a number, finish.").unwrap();
    harness.force(root, Actor::Operator).unwrap();
    let outcome = harness.run_to_hole_or_done(root).await.expect("drives to hole");
    let hole_before = match outcome {
        tidepool_harness::TurnOutcome::Suspended { hole, .. } => hole,
        other => panic!("root should suspend at returnControl, got {}", outcome_tag(&other)),
    };

    harness
        .answer_return_control(root)
        .await
        .expect("recovers via retry after the runtime fault");

    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the node completes once the CORRECTED answer resumes it, surviving the bottom-answer fault"
    );

    let events = events_for(&log_path, root);

    // Not consumed by the bottom answer: exactly one HolePublished (never
    // re-published), exactly one HoleConsumed (only the final valid answer),
    // and it references the SAME hole the node originally suspended on.
    let published: Vec<&Event> = events.iter().filter(|e| matches!(e, Event::HolePublished { .. })).collect();
    assert_eq!(published.len(), 1, "hole published exactly once, got {published:?}");
    let Event::HolePublished { hole: published_hole, .. } = published[0] else { unreachable!() };
    assert_eq!(published_hole.0, hole_before);

    let consumed: Vec<&Event> = events.iter().filter(|e| matches!(e, Event::HoleConsumed { .. })).collect();
    assert_eq!(consumed.len(), 1, "hole consumed exactly once — the bottom attempt must not consume, got {consumed:?}");
    let Event::HoleConsumed { hole: consumed_hole, .. } = consumed[0] else { unreachable!() };
    assert_eq!(consumed_hole.0, hole_before, "the SAME hole survives the bottom-answer fault to be consumed by the valid retry");

    // FIXED (B1 widen): the runtime-fault attempt now logs a Rejected
    // HoleAnswerAttempt too — one Rejected for the bottom-answer fault, one
    // Consumed for the valid retry — closing the audit-trail gap the
    // compile-failure path never had.
    let attempts: Vec<&Event> = events.iter().filter(|e| matches!(e, Event::HoleAnswerAttempt { .. })).collect();
    assert_eq!(
        attempts.len(),
        2,
        "one Rejected attempt (the runtime fault) and one Consumed attempt (the valid retry), got {attempts:?}"
    );
    let Event::HoleAnswerAttempt {
        hole: h0,
        outcome: AnswerOutcome::Rejected { error },
        ..
    } = attempts[0]
    else {
        panic!("first attempt must be Rejected (the runtime fault), got {:?}", attempts[0]);
    };
    assert_eq!(h0.0, hole_before, "the rejected attempt references the SAME hole (not consumed)");
    assert!(
        error.contains("boom"),
        "the logged rejection must name the runtime fault, got:\n{error}"
    );
    assert!(matches!(
        attempts[1],
        Event::HoleAnswerAttempt { outcome: AnswerOutcome::Consumed, .. }
    ));

    // The retry prompt the answerer actually saw names the runtime fault.
    let saw_retry_prompt = events.iter().any(|e| matches!(
        e,
        Event::TurnDelta { role: tidepool_harness::provider::Role::User, content, .. }
            if content.contains("failed at runtime") && content.contains("boom")
    ));
    assert!(saw_retry_prompt, "the answerer must see the runtime fault as its next turn");

    assert!(events.iter().any(|e| matches!(e, Event::NodeDone { .. })));
}

/// `dialogAsk`'s MECHANICAL answer path (D6, zero model turns) also flows
/// through `resume_parent` — sanity check that the NORMAL (non-bottom) answer
/// path is unaffected: `answer_dialog` still logs Consumed only alongside a
/// real completion when the value is well-formed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dialog_mechanical_answer_completes_and_logs_consistently() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("dialog.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let replies = vec![reply(
        "```haskell\ndo\n  _ <- dialogAsk (toJSON (card \"Confirm\" [choice \"Proceed?\" \
         [(\"yes\", \"Yes\"), (\"no\", \"No\")]]))\n  pure (toJSON (1 :: Int))\n```",
    )];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness.create_root("dialog root", "Confirm, finish.").unwrap();
    harness.force(root, Actor::Operator).unwrap();
    let outcome = harness.run_to_hole_or_done(root).await.expect("drives to hole");
    assert!(matches!(
        outcome,
        tidepool_harness::TurnOutcome::Suspended {
            classified: tidepool_harness::ClassifiedHole {
                routing: HoleRouting::Dialog { .. },
                ..
            },
            ..
        }
    ));

    harness
        .answer_dialog(root, json!({ "values": { "yes": true }, "prose": "" }))
        .await
        .expect("mechanical dialog answer");
    assert_eq!(harness.tree().state(root), Some(NodeState::Done));

    let events = events_for(&log_path, root);
    assert!(events.iter().any(|e| matches!(e, Event::NodeDone { .. })));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::HoleConsumed { .. }))
            .count(),
        1
    );
}

/// A SECOND, sequential `returnControl` hole in the SAME compiled turn — the
/// resumed continuation (`Session::resume`) hits another `AskWith` before the
/// do-block completes. This is the re-suspend arm of `resume_parent`: it must
/// classify + publish the second hole's REAL site + type (`HoleRouting::ReturnControl
/// { ty: Some("Bool"), .. }`), not the `None`/`None` a stale re-suspend used to
/// carry. Also proves the typed-answer path works on hole 2: the mechanical
/// `Ui::Choice` form is derivable from the SAME `Bool` type via
/// `pending_derived_ui`, and answering it (in-context, via
/// `answer_return_control` again) resumes the continuation to completion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn return_control_second_sequential_hole_carries_its_type() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("rc-second-hole.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::standard(prelude_dir(), None).expect("engine config");

    let replies = vec![
        // Root turn: two SEQUENTIAL returnControl holes in one compiled block.
        reply(
            "```haskell\ndo\n  n <- returnControl @Int \"pick a number between 1 and 100\"\n  \
             b <- returnControl @Bool \"is it even?\"\n  pure (toJSON (n, b))\n```",
        ),
        // Answers the FIRST hole (Int).
        reply("```haskell\nresume (42 :: Int)\n```"),
        // Answers the SECOND hole (Bool) — the re-suspend this test targets.
        reply("```haskell\nresume True\n```"),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("rc second hole", "Get a number, then a bool, finish.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to its first hole");
    match &outcome {
        tidepool_harness::TurnOutcome::Suspended { classified, .. } => match &classified.routing {
            HoleRouting::ReturnControl { ty, .. } => {
                assert_eq!(ty.as_deref(), Some("Int"), "first hole carries Int");
            }
            other => panic!("expected a ReturnControl hole, got {other:?}"),
        },
        other => panic!("root should suspend at the first returnControl, got {}", outcome_tag(other)),
    }

    // Answer the FIRST hole. This drives the resumed continuation straight
    // into the SECOND returnControl — the re-suspend arm under test.
    harness
        .answer_return_control(root)
        .await
        .expect("first hole answered; resume hits the second hole");

    // The node must still be Suspended (on the second hole), not Done yet.
    assert!(matches!(
        harness.tree().state(root),
        Some(NodeState::Suspended { .. })
    ));

    // --- the flagship assertion: the SECOND hole carries its real type -----
    let second_pending = harness
        .pending_hole(root)
        .expect("root is suspended on the second hole");
    let (second_site, second_ty) = match &second_pending.routing {
        HoleRouting::ReturnControl { site, ty } => (*site, ty.clone()),
        other => panic!("second hole must also be a ReturnControl, got {other:?}"),
    };
    assert_eq!(
        second_ty.as_deref(),
        Some("Bool"),
        "the SECOND hole must carry its real type from the fresh classification, not None"
    );

    // The typed-answer/derived-form path works on hole 2: `Bool` is a nullary
    // sum, so the server-derived mechanical form is a Choice — provable only
    // if the second hole's type resolved correctly.
    let derived = harness
        .pending_derived_ui(root)
        .expect("hole 2's type resolves to a mechanically-derivable Ui");
    assert!(
        matches!(derived, Ui::Choice { .. }),
        "Bool derives a Choice form, got {derived:?}"
    );

    // The durable log's second HolePublished record also carries site + ty.
    let events = events_for(&log_path, root);
    let published: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::HolePublished { .. }))
        .collect();
    assert_eq!(
        published.len(),
        2,
        "two holes published in sequence, got {published:?}"
    );
    let Event::HolePublished { site, ty, .. } = published[1] else {
        unreachable!()
    };
    assert!(
        site.is_some(),
        "the second HolePublished record must carry a site id, got {site:?}"
    );
    assert_eq!(
        ty.as_deref(),
        Some("Bool"),
        "the durable HolePublished record for hole 2 must carry its type"
    );
    assert_eq!(site.map(|s| s.0), Some(second_site));

    // Answer the SECOND hole and drive the program to completion — proves the
    // typed-answer path is not just classified correctly but actually usable.
    harness
        .answer_return_control(root)
        .await
        .expect("second hole answered; the program completes");
    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the node completes once BOTH sequential holes are answered"
    );

    let events = events_for(&log_path, root);
    let consumed: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::HoleConsumed { .. }))
        .collect();
    assert_eq!(
        consumed.len(),
        2,
        "each hole consumes exactly once, got {consumed:?}"
    );
    assert!(events.iter().any(|e| matches!(e, Event::NodeDone { .. })));
}
