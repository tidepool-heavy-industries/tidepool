//! End-to-end acceptance for the delegation surface's POSITIVE
//! path: the root session calls `delegate`, the driver's `SubagentHandler`
//! runs a real saga against a `MockBackend` (a real temporary git repo, a
//! real worktree/binding table — see `outer_subagent.rs`'s module doc for
//! why that tier is "real saga, no model, no tokens"), the typed
//! `DelegateResult` decodes, and the session finalizes on what it found —
//! the result returns INLINE.
//!
//! Drives the SHIPPED `harness-dogfooding/recursive-companion/` harness
//! (same precedent as `companion_collapsed_slice.rs`/`dogfood_harness_typecheck.rs`
//! — a copied fixture would keep passing while the shipped harness rotted),
//! booting `EngineConfig::from_decls(typed_request_agent_decls_with_delegate(), ..)
//! .with_delegate_wrap()` for the answerer config — the same wiring
//! `tidepool/src/bin/tidepool-selfharness.rs` selects live — so the
//! root session compiles against the narrow delegating row.
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value as Json};

use tidepool_agent::backend::mock::MockBackend;
use tidepool_agent::seam::CycleResultPayload;
use tidepool_handlers::{
    ConsoleHandler, JournalHandler, SegmentPath, SubagentHandler, WorktreeHandler,
};
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{LogHeader, LogWriter};
use tidepool_harness::provider::{DynModelProvider, Usage};
use tidepool_harness::replay::{RecordedReply, ReplayProvider};
use tidepool_harness::selfharness::operator::FormShape;
use tidepool_harness::selfharness::persistence;
use tidepool_harness::{
    load_harness_source, typed_request_agent_decls_with_delegate, Harness, LogObserver,
    OperatorGate, SelfHarnessDriver,
};
use tidepool_worktree::testing::TestRepo;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

fn companion_dir() -> PathBuf {
    repo_root().join("harness-dogfooding/recursive-companion")
}

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "delegate-positive-path-{}-{label}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "delegate-positive-path".into(),
        extract_fingerprint: "delegate-positive-path".into(),
        harness_version: "test".into(),
    }
}

/// A gate that never expects a presentation — this scenario runs `GateOff`.
struct NoGate;

impl OperatorGate for NoGate {
    fn present_form(&self, _shape: &FormShape) -> Json {
        panic!("this scenario runs GateOff — no form should ever be presented")
    }
}

fn haskell(block: &str) -> String {
    format!("```haskell\n{block}\n```")
}

/// The root session's reply: delegate, then finalize on what came back.
/// `renderDelegateError`/`delegateSummary` are exactly what the companion's
/// protocol (`HarnessTypes.companionProtocol`) teaches.
fn delegating_reply() -> String {
    haskell(
        "do { r <- delegateTyped @DelegateResult (DelegateBrief { delegateLabel = \"probe\", \
         delegateInstruction = \"look around\", delegateExpected = \"a one-line summary\" }); \
         case r of { \
           Left e -> (finalize @Text (renderDelegateError e) :: M ()); \
           Right run -> (finalize @Text (delegateSummary (delegateValue run) <> \
             if delegateBase run == delegateHead run then \" [workspace observed]\" \
             else \" [workspace advanced]\") :: M ()) } }",
    )
}

fn reply(content: String) -> RecordedReply {
    RecordedReply {
        content,
        usage: Usage {
            input_tokens: 50,
            output_tokens: 10,
            cached_input_tokens: None,
            cache_write_tokens: None,
        },
    }
}

fn state_json() -> Json {
    json!({
        "question": "SCENARIO: delegate from the root session.",
        "turnCount": 0,
        "lastAnswer": Json::Null,
    })
}

/// Calling `delegate` for real — which lowers via
/// `Tidepool.Agent.Delegate.runDelegate`'s `reinterpret2` onto a
/// `send (SubagentSpawnAsync ...)` performed FROM WITHIN the reinterpretation
/// handler — reaches the driver classified as `SuspensionRouting::Subagent`.
/// See `tests/reinterpret_rowchange_repro.rs` for a minimal, isolated
/// repro of the underlying JIT dispatch mechanism this relies on (no
/// Subagent/Worktree involved).
///
/// This test is the full real-world positive path (isolated by direct
/// comparison against `direct_subagent_send_dispatches_within_the_answerer_row`
/// below, all on the SAME `typed_request_agent_decls_with_delegate()` row and the SAME
/// `Harness`/`SelfHarnessDriver`/`pending_suspension_with_request`/
/// `resume_with_value` servicing that test proves works). The TYPE-LEVEL
/// mechanism (unnameability) was never affected and is proved separately at
/// the compile level (`delegate_type_pinning.rs`, 5/5 green).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_session_delegates_and_finalizes_on_the_result() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let dir = scratch("run");
    let checkpoint_path = dir.join("checkpoint.json");
    let journal_path = dir.join("journal.jsonl");
    let log_path = dir.join("log.jsonl");

    // The delegating nested-answerer config: `typed_request_agent_decls_with_delegate()`
    // (Subagent, Worktree prepended) + the wrap that pins the model's own
    // block to the narrow `Delegate`-only row.
    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls_with_delegate(),
        repo_root().join("haskell/lib"),
        Some(companion_dir()),
    )
    .expect("delegating answerer engine config over the recursive-companion harness dir")
    .with_delegate_wrap();

    let provider: Arc<dyn DynModelProvider> =
        Arc::new(ReplayProvider::new(vec![reply(delegating_reply())]));
    let writer = LogWriter::create(&log_path, &header()).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_checkpoint_path(checkpoint_path.clone());
    driver.set_console_handler(ConsoleHandler);
    driver.set_gate(Arc::new(NoGate));
    driver.set_answerer_round_caps(1, 2);
    driver.set_journal_handler(
        JournalHandler::new(
            SegmentPath::create_exclusive(journal_path.clone())
                .expect("this scenario's journal segment is fresh in its own tempdir"),
        )
        .expect("fresh segment header stamp succeeds"),
    );

    // The real saga (real temporary git repo, real worktree/binding table),
    // MockBackend so no model call and no tokens — same tier
    // `outer_subagent.rs` uses.
    let store = TestRepo::init().expect("git init the delegation target repo");
    store
        .writer()
        .commit_file("README.md", "hello\n", "seed commit")
        .expect("seed commit");
    let roots = tempfile::TempDir::new().expect("substrate roots");
    let registry_root = roots.path().join("registry");
    let worktree_root = roots.path().join("worktrees");
    let binding_root = roots.path().join("bindings");
    let backend = MockBackend::completing(CycleResultPayload::Structured(json!({
        "delegateSummary": "found one file: README.md",
        "delegateCaveats": ["scope: read-only look-around"],
    })));
    let subagent_handler = SubagentHandler::new(
        registry_root.clone(),
        worktree_root.clone(),
        binding_root,
        store.path().to_path_buf(),
        Box::new(backend),
    )
    .expect("subagent handler opens");
    let worktree_handler =
        WorktreeHandler::new(registry_root, worktree_root, store.path().to_path_buf())
            .expect("worktree handler opens against the subagent registry");
    driver.set_subagent_handler(subagent_handler);
    driver.set_worktree_handler(worktree_handler);

    let source = load_harness_source(&companion_dir().join("Harness.hs"))
        .expect("the shipped recursive-companion harness loads");

    persistence::save_checkpoint(
        &checkpoint_path,
        &persistence::Checkpoint::committed(
            None,
            state_json(),
            None,
            source.fingerprint.clone(),
            persistence::LoopIteration::new(0),
        ),
    )
    .expect("seed the scenario's durable checkpoint");

    let restored = driver
        .restore(&source)
        .await
        .expect("restore the seeded checkpoint")
        .expect("the seeded checkpoint is on disk");

    let outcome = driver
        .run_one_loop_iteration(&source, Some(&restored))
        .await
        .expect("one render -> loop -> thoughtHylo -> render cycle, delegation included");

    let state = outcome.state_json;
    let last_answer = state
        .get("lastAnswer")
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("the cycle recorded no lastAnswer: {state}"));

    // The delegated subagent's OWN typed result (`delegateSummary`, decoded
    // through the real MockBackend saga) IS the answer the session finalized
    // — delegate results return inline, and the loop stores the root's typed
    // value as-is (fork-subsumes-split step 4).
    assert_eq!(
        last_answer, "found one file: README.md [workspace observed]",
        "lastAnswer must carry the delegated result the session finalized on: {state}"
    );

    // The loop's own bookkeeping: a "turn" journal entry carrying the answer.
    let journal = tidepool_handlers::load_journal(&journal_path).expect("journal loads");
    let answer_entry = journal
        .iter()
        .filter(|e| e.kind == "turn")
        .find_map(|e| e.payload.get("answer").and_then(Json::as_str))
        .unwrap_or_else(|| panic!("no \"turn\" journal entry with an \"answer\" field"));
    assert!(
        answer_entry.contains("found one file: README.md [workspace observed]"),
        "the turn journal must carry the delegated subagent's OWN typed \
         result, decoded through the real MockBackend saga — not a \
         placeholder: {answer_entry}"
    );
}

/// The half of the mechanism that DOES work end to end: a `Subagent` send
/// written DIRECTLY in a branch-node window's own turn text (no
/// `runDelegate`/`reinterpret2` involved) dispatches through the driver's
/// `SubagentHandler` (`pending_suspension_with_request` reading the raw suspended
/// request `Harness` already retains, `resume_with_value` resuming the
/// node's own session — the new plumbing this lane added to
/// `SuspensionRouting::Subagent`'s existing, pre-`drain_note_holes` servicing) and
/// the typed `CycleId`/`SpawnOutcome` crosses back into the resumed
/// continuation. This is the isolating CONTROL for the test above: the
/// SAME row, the SAME driver wiring, the SAME `MockBackend` saga — the only
/// difference is that `SubagentSpawnAsync` is sent directly rather than
/// through `Tidepool.Agent.Delegate`'s reinterpretation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_subagent_send_dispatches_within_the_answerer_row() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let dir = scratch("direct");
    let checkpoint_path = dir.join("checkpoint.json");
    let journal_path = dir.join("journal.jsonl");
    let log_path = dir.join("log.jsonl");

    // Same row as the delegating scenario, but WITHOUT `with_delegate_wrap`:
    // the model's own text sends `SubagentSpawnAsync` directly.
    let agent_cfg = EngineConfig::from_decls(
        typed_request_agent_decls_with_delegate(),
        repo_root().join("haskell/lib"),
        Some(companion_dir()),
    )
    .expect("delegating answerer engine config over the recursive-companion harness dir");

    let direct_send_reply = haskell(
        "do { let { wspec = WorktreeSpec { specSource = SourceCurrentRepository, \
         specLabel = \"probe\", specDirtyPolicy = RequireClean }; \
         spec = spawnSpec wspec \"probe\" \"look around\" }; \
         spawned <- send (SubagentSpawnAsync spec Aeson.Null); \
         case spawned of { \
           Left _err -> (finalize @Text (\"spawn failed\" :: Text) :: M ()); \
           Right _cyc -> (finalize @Text (\"spawned ok\" :: Text) :: M ()) } }",
    );
    let provider: Arc<dyn DynModelProvider> =
        Arc::new(ReplayProvider::new(vec![reply(direct_send_reply)]));
    let writer = LogWriter::create(&log_path, &header()).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_checkpoint_path(checkpoint_path.clone());
    driver.set_console_handler(ConsoleHandler);
    driver.set_gate(Arc::new(NoGate));
    driver.set_answerer_round_caps(1, 2);
    driver.set_journal_handler(
        JournalHandler::new(
            SegmentPath::create_exclusive(journal_path.clone())
                .expect("this scenario's journal segment is fresh in its own tempdir"),
        )
        .expect("fresh segment header stamp succeeds"),
    );

    let store = TestRepo::init().expect("git init the delegation target repo");
    store
        .writer()
        .commit_file("README.md", "hello\n", "seed commit")
        .expect("seed commit");
    let roots = tempfile::TempDir::new().expect("substrate roots");
    let backend = MockBackend::completing(CycleResultPayload::Structured(json!({
        "delegateSummary": "unused by this probe",
        "delegateCaveats": [],
    })));
    let subagent_handler = SubagentHandler::new(
        roots.path().join("registry"),
        roots.path().join("worktrees"),
        roots.path().join("bindings"),
        store.path().to_path_buf(),
        Box::new(backend),
    )
    .expect("subagent handler opens");
    driver.set_subagent_handler(subagent_handler);

    let source = load_harness_source(&companion_dir().join("Harness.hs"))
        .expect("the shipped recursive-companion harness loads");

    persistence::save_checkpoint(
        &checkpoint_path,
        &persistence::Checkpoint::committed(
            None,
            state_json(),
            None,
            source.fingerprint.clone(),
            persistence::LoopIteration::new(0),
        ),
    )
    .expect("seed the scenario's durable checkpoint");

    let restored = driver
        .restore(&source)
        .await
        .expect("restore the seeded checkpoint")
        .expect("the seeded checkpoint is on disk");

    let outcome = driver
        .run_one_loop_iteration(&source, Some(&restored))
        .await
        .expect(
            "one render -> loop -> thoughtHylo -> render cycle, with a direct \
             Subagent send serviced mid-window",
        );

    let state = outcome.state_json;
    let last_answer = state
        .get("lastAnswer")
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("the cycle recorded no lastAnswer: {state}"));
    assert_eq!(
        last_answer, "spawned ok",
        "the direct Subagent send must be serviced, and the session's own \
         finalize must land as the turn's answer: {state}"
    );
}
