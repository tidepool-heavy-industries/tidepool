//! Coverage for `Harness::answer_fork`'s new module-lookup pin: a fork
//! child's row is now widened from its OWN requested answer type — resolved
//! via `asks.json`'s module lookup (`AsksSidecar::modules_of`) — rather than
//! only when that type happened to MATCH the PARENT's own `AnswerContract`.
//! This root node never sets an `AnswerContract`, so under the OLD mechanism
//! (`parent_contract` matching) the fork below would have found no contract
//! to match against and the child's row would stay unpinned; under the fix
//! it is pinned to `Decision`/`HarnessTypes` purely from the fork site's own
//! resolution. The child still answers via `resume` (the verb THIS servicing
//! path's `run_child` one-shot compile actually supports — `resume` is
//! locally redefined as `pure` for the compile, so it never suspends;
//! `finalize @T` is a genuinely suspending effect and crashes this specific
//! path with `ResidentError::ChildSuspended`, a separate, pre-existing gap
//! this lane's fix does not newly enable — see this crate's CLAUDE.md).
//!
//! Needs `TIDEPOOL_EXTRACT` and the with-packages GHC on PATH
//! (`--ignore-default-filter` to run; see `tests/golden_path.rs` for the env
//! recipe).

mod support;

use std::sync::Arc;

use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{Actor, LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::tree::NodeState;
use tidepool_harness::{answerer_decls, Harness, HoleRouting, TurnOutcome};

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn prelude_dir() -> std::path::PathBuf {
    repo_root().join("haskell/lib")
}

fn examples_harness_dir() -> std::path::PathBuf {
    repo_root().join("examples/harness")
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "fork-own-type-pin".into(),
        extract_fingerprint: "fork-own-type-pin".into(),
        harness_version: "test".into(),
    }
}

fn reply(content: &str) -> RecordedReply {
    RecordedReply {
        content: content.to_string(),
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

fn outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_child_resolves_its_own_type_with_no_parent_contract() {
    support::require_extract();

    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fork-own-type-pin.jsonl");
    let writer = LogWriter::create(&log_path, &header()).unwrap();
    let cfg = EngineConfig::from_decls(
        answerer_decls(),
        prelude_dir(),
        Some(examples_harness_dir()),
    )
    .expect("answerer engine config");

    let replies = vec![
        reply(
            "```haskell\n\
             import Tidepool.Fork\n\
             import HarnessTypes\n\
             \n\
             fork @Decision \"decide\"\n\
             ```",
        ),
        reply(
            "```haskell\n\
             import HarnessTypes\n\
             \n\
             resume (Decision { action = \"observe\", rationale = \"because\", confidence = High })\n\
             ```",
        ),
    ];
    let provider: Arc<dyn DynModelProvider> = Arc::new(ReplayProvider::new(replies));
    let harness = Arc::new(Harness::new(writer, cfg, provider).expect("harness boots"));

    let root = harness
        .create_root("fork own-type root", "Fork a child to decide.")
        .unwrap();
    harness.force(root, Actor::Operator).unwrap();
    // Deliberately NOT calling `set_answer_contract` — the root has no
    // contract of its own, so a PRE-fix `answer_fork` would have pinned
    // nothing for the child either (the parent-contract-matching guess had
    // no contract to match against).

    let outcome = harness
        .run_to_hole_or_done(root)
        .await
        .expect("root drives to the fork hole");
    match &outcome {
        TurnOutcome::Suspended { classified, .. } => match &classified.routing {
            HoleRouting::Fork { ty, fan: None, .. } => {
                assert_eq!(ty.as_deref(), Some("Decision"));
            }
            other => panic!("expected a plain fork hole for Decision, got {other:?}"),
        },
        TurnOutcome::Completed { rendered } => {
            panic!("root should suspend on the fork hole, got Completed: {rendered}")
        }
        other => panic!(
            "root should suspend on the fork hole, got {}",
            outcome_tag(other)
        ),
    }

    let child = harness
        .answer_fork(root, Actor::Operator)
        .await
        .expect("the fork child answers with a Decision value");

    assert_eq!(
        harness.tree().state(child),
        Some(NodeState::Done),
        "the fork child answers and retires"
    );
    assert_eq!(
        harness.tree().state(root),
        Some(NodeState::Done),
        "the parent resumes and completes with the child's Decision value"
    );
}
