//! GHC-tier: a `returnControl @Verdict` hole answered MECHANICALLY (§6 D6,
//! `uiof`) from its server-derived `Choice` form — zero model turns for the
//! answer. Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH
//! (`--ignore-default-filter` to run; see `tests/golden_path.rs` for the env
//! recipe).
//!
//! The thread: force root → the (replayed) model's block `import`s a small
//! user module declaring `data Verdict = GO | PARTIAL | NOGO` and suspends on
//! `returnControl @Verdict` → `pending_derived_ui` resolves to a `Choice` over
//! the three constructors, in declaration order → `answer_mechanical` with a
//! `"NOGO"` submission compiles+runs `resume NOGO` against the SAME node's
//! suspended session (no forked child, no second model turn) and resumes it →
//! the program completes.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::ReplayProvider;
use tidepool_harness::tree::NodeState;
use tidepool_harness::{Harness, HoleRouting, TurnOutcome, Ui};

fn extract_available() -> bool {
    std::env::var("TIDEPOOL_EXTRACT").is_ok()
        || std::process::Command::new("tidepool-extract")
            .arg("--help")
            .output()
            .is_ok()
}

fn prelude_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|r| r.join("haskell/lib"))
        .unwrap_or_else(|| PathBuf::from("haskell/lib"))
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "uiof-mechanical".into(),
        extract_fingerprint: "uiof-mechanical".into(),
        harness_version: "test".into(),
    }
}

/// A tiny user module declaring the nullary sum, in its own tempdir — kept
/// OUT of `haskell/` (frozen; A1 must not touch it). Returns the tempdir
/// (whose lifetime the caller must hold) and its path as the harness's
/// `project_lib` include entry.
fn verdict_lib_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Verdict.hs"),
        "module Verdict where\n\ndata Verdict = GO | PARTIAL | NOGO deriving (Show)\n",
    )
    .unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mechanical_choice_answer_resumes_return_control_to_completion() {
    if !extract_available() {
        eprintln!("Skipping: tidepool-extract not available (set TIDEPOOL_EXTRACT, run in nix develop)");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("uiof.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();

    let lib_dir = verdict_lib_dir();
    let cfg = EngineConfig::standard(prelude_dir(), Some(lib_dir.path().to_path_buf()))
        .expect("engine config");

    // ONE scripted reply: the root turn suspends at `returnControl @Verdict`.
    // No further scripted replies — the answer is MECHANICAL, no second model
    // turn drives the answer.
    let replies = vec![tidepool_harness::replay::RecordedReply {
        node: tidepool_harness::tree::NodeId(0),
        turn: 0,
        content: "I'll ask for a verdict.\n\n\
                  ```haskell\n\
                  import Verdict\n\n\
                  do\n\
                  \x20 v <- returnControl @Verdict \"pick GO, PARTIAL, or NOGO\"\n\
                  \x20 pure (toJSON (show v))\n\
                  ```"
            .to_string(),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 20,
        },
    }];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("uiof mechanical", "Ask for a verdict, mechanically.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();

    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to a hole");
    match &outcome {
        TurnOutcome::Suspended { classified, .. } => {
            assert!(
                matches!(
                    &classified.routing,
                    HoleRouting::ReturnControl { ty: Some(ty), .. } if ty == "Verdict"
                ),
                "root should suspend on a returnControl @Verdict hole, got {:?}",
                classified.routing
            );
        }
        other => panic!(
            "expected a Suspended outcome, got a different one: {}",
            outcome_tag(other)
        ),
    }
    assert!(matches!(
        harness.tree().state(root),
        Some(NodeState::Suspended { .. })
    ));

    // The derived form is a Choice over the three constructors, in
    // declaration order.
    let ui = harness
        .pending_derived_ui(root)
        .expect("uiof derives a Choice form for a nullary-sum answer type");
    assert_eq!(
        ui,
        Ui::Choice {
            prompt: "Choose a Verdict".to_string(),
            options: vec![
                ("GO".to_string(), "GO".to_string()),
                ("PARTIAL".to_string(), "PARTIAL".to_string()),
                ("NOGO".to_string(), "NOGO".to_string()),
            ],
        }
    );

    // Answer MECHANICALLY: a bare option-key submission, zero model turns.
    harness
        .answer_mechanical(root, json!({ "values": { "NOGO": true }, "prose": "" }))
        .await
        .expect("mechanical answer resumes the hole");

    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the program completes after the mechanical answer resumes it"
    );

    // Exactly the one scripted reply was consumed — the answer cost no model
    // turn.
    let replay = ReplayProvider::from_log(&log_path).expect("replay from log");
    assert_eq!(
        replay.remaining(),
        1,
        "only the root's own turn is a recorded assistant turn; the mechanical \
         answer is not a model turn"
    );
}

fn outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}
