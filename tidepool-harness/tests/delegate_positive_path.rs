//! PRD 21 C5 — end-to-end acceptance for the delegation surface's POSITIVE
//! path: a branch-node (coalgebra) window calls `delegate`, the driver's
//! `SubagentHandler` runs a real saga against a `MockBackend` (a real
//! temporary git repo, a real worktree/binding table — see
//! `outer_subagent.rs`'s module doc for why that tier is "real saga, no
//! model, no tokens"), the typed `DelegateResult` decodes, and the window
//! finalizes on what it found.
//!
//! Drives the SHIPPED `harness-dogfooding/recursive-companion/` harness
//! (same precedent as `companion_recursive_slice.rs`/`dogfood_harness_typecheck.rs`
//! — a copied fixture would keep passing while the shipped harness rotted).
//! Unlike that file's scenarios, THIS one boots
//! `EngineConfig::from_decls(answerer_decls_with_delegate(), ..)
//! .with_delegate_wrap()` for the nested-answerer config, so the root's
//! coalgebra (DISCOVER) window compiles against the narrow delegating row
//! instead of the plain answerer row.
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value as Json};

use tidepool_agent::backend::mock::MockBackend;
use tidepool_agent::seam::CycleResultPayload;
use tidepool_handlers::{ConsoleHandler, JournalHandler, SegmentPath, SubagentHandler};
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{LogHeader, LogWriter};
use tidepool_harness::provider::{
    DynModelProvider, ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse, Usage,
};
use tidepool_harness::selfharness::operator::FormShape;
use tidepool_harness::selfharness::persistence;
use tidepool_harness::{
    answerer_decls_with_delegate, load_harness_source, ContinueSignal, Harness, LogObserver,
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

/// One scripted window: the request's last message must contain `needle` for
/// `reply` to be served — adapted (single-needle form) from
/// `companion_recursive_slice.rs`'s `KeyedProvider`.
struct KeyedProvider {
    scripted: Vec<(&'static str, String)>,
}

impl ModelProvider for KeyedProvider {
    async fn complete(
        &self,
        req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let last = req
            .messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let reply = self
            .scripted
            .iter()
            .find(|(needle, _)| last.contains(needle))
            .map(|(_, r)| r.clone())
            .ok_or_else(|| {
                ProviderError::Api(format!("KeyedProvider: no scripted reply matches:\n{last}"))
            })?;
        Ok(TurnResponse {
            text: reply,
            usage: Usage {
                input_tokens: 50,
                output_tokens: 10,
                cached_input_tokens: None,
            },
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

/// A gate that never expects a presentation — this scenario runs `GateOff`.
struct NoGate;

impl OperatorGate for NoGate {
    fn present_form(&self, _shape: &FormShape) -> Json {
        panic!("this scenario runs GateOff — no form should ever be presented")
    }
    fn await_continue(&self) -> ContinueSignal {
        ContinueSignal::Continue
    }
}

fn haskell(block: &str) -> String {
    format!("```haskell\n{block}\n```")
}

/// The root's coalgebra reply: delegate, then finalize on what came back.
/// `renderDelegateError`/`delegateSummary` are exactly what
/// `harness-dogfooding/recursive-companion/Harness.hs`'s prompt teaches.
fn delegating_reply() -> String {
    haskell(
        "do { r <- delegate (DelegateBrief { delegateLabel = \"probe\", \
         delegateInstruction = \"look around\", delegateExpected = \"a one-line summary\" }); \
         case r of { \
           Left e -> finalize @LayerProposal (ProposeFinish { localAnswer = renderDelegateError e }); \
           Right ok -> finalize @LayerProposal (ProposeFinish { localAnswer = delegateSummary ok }) } }",
    )
}

fn state_json() -> Json {
    json!({
        "question": "SCENARIO: delegate from the root's coalgebra window.",
        "config": {
            "maxDepth": 3,
            "maxNodes": 10,
            "maxFanOut": 3,
            "gatePolicy": {"tag": "GateOff"},
            "gateMaxRounds": 8,
        },
        "turnCount": 0,
        "lastRun": Json::Null,
        "draft": "",
    })
}

/// Calling `delegate` for real — which lowers via
/// `Tidepool.Agent.Delegate.runDelegate`'s `reinterpret2` onto a
/// `send (SubagentSpawnAsync ...)` performed FROM WITHIN the reinterpretation
/// handler — used to reach the driver as an UNCLASSIFIED suspension
/// (`HoleRouting::Ask { payload: Null }`, `classify_hole`'s `con_name` lookup
/// missing `SubagentSpawnAsync`'s own constructor name), not
/// `HoleRouting::Subagent`. FIXED (`jit-reinterpret-rowchange` lane): the
/// root cause was a `tidepool-codegen` JIT bug, isolated with a minimal
/// standalone repro (`tests/reinterpret_rowchange_repro.rs`, no
/// Subagent/Worktree involved) and documented in
/// `plans/self-iterating-harness/21-c5-delegate-effect-survey.md`'s third
/// amendment — `decomp`'s literal-tag pattern match (`Data.OpenUnion`,
/// underlying every `reinterpret`/`reinterpret2` call) had no tolerance for
/// a BOXED `W#` tag reaching it from un-inlined cross-module generic code,
/// unlike its sibling `emit_data_dispatch`'s already-existing "Runtime
/// Lit-tolerance" in the opposite direction. Fixed in
/// `tidepool-codegen/src/emit/case.rs`'s `emit_lit_dispatch` by routing the
/// scrutinee through the same arity-guarded `unwrap_boxing_chain`
/// `unbox_addr`/`unbox_bytearray` already use.
///
/// This test is the full real-world positive path (isolated by direct
/// comparison against `direct_subagent_send_dispatches_within_the_answerer_row`
/// below, all on the SAME `answerer_decls_with_delegate()` row and the SAME
/// `Harness`/`SelfHarnessDriver`/`pending_hole_with_request`/
/// `resume_with_value` servicing that test proves works). The TYPE-LEVEL
/// mechanism (unnameability) was never affected and is proved separately at
/// the compile level (`delegate_type_pinning.rs`, 5/5 green).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_coalgebra_window_delegates_and_finalizes_on_the_result() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let dir = scratch("run");
    let checkpoint_path = dir.join("checkpoint.json");
    let journal_path = dir.join("journal.jsonl");
    let log_path = dir.join("log.jsonl");

    // The delegating nested-answerer config: `answerer_decls_with_delegate()`
    // (Subagent, Worktree prepended) + the wrap that pins the model's own
    // block to the narrow `Delegate`-only row.
    let agent_cfg = EngineConfig::from_decls(
        answerer_decls_with_delegate(),
        repo_root().join("haskell/lib"),
        Some(companion_dir()),
    )
    .expect("delegating answerer engine config over the recursive-companion harness dir")
    .with_delegate_wrap();

    let provider: Arc<dyn DynModelProvider> = Arc::new(KeyedProvider {
        scripted: vec![
            ("NODE root — DISCOVER", delegating_reply()),
            (
                "— FOLD",
                haskell(
                    "finalize @FoldDecision (FoldDecision { foldSynthesis = \"FOLDED\", \
                     foldTensions = [], foldEditsInOrder = [], \
                     foldProposed = [] })",
                ),
            ),
        ],
    });
    let writer = LogWriter::create(&log_path, &header()).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_checkpoint_path(checkpoint_path.clone());
    driver.set_console_handler(ConsoleHandler);
    driver.set_gate(Arc::new(NoGate));
    driver.set_answerer_round_caps(1, 2);
    driver.set_journal_handler(JournalHandler::new(
        SegmentPath::create_exclusive(journal_path.clone())
            .expect("this scenario's journal segment is fresh in its own tempdir"),
    ));

    // The real saga (real temporary git repo, real worktree/binding table),
    // MockBackend so no model call and no tokens — same tier
    // `outer_subagent.rs` uses.
    let store = TestRepo::init().expect("git init the delegation target repo");
    store
        .writer()
        .commit_file("README.md", "hello\n", "seed commit")
        .expect("seed commit");
    let roots = tempfile::TempDir::new().expect("substrate roots");
    let backend = MockBackend::completing(CycleResultPayload::Structured(json!({
        "delegateSummary": "found one file: README.md",
        "delegateCaveats": ["scope: read-only look-around"],
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
        .run_one_cycle(&source, Some(&restored))
        .await
        .expect("one render -> loop -> thoughtHylo -> render cycle, delegation included");

    let state = outcome.state_json;
    let last_run = state
        .get("lastRun")
        .filter(|v| !v.is_null())
        .unwrap_or_else(|| panic!("the cycle recorded no lastRun: {state}"));

    assert_eq!(
        last_run.get("runNodes").and_then(Json::as_i64),
        Some(1),
        "one node (the root), finished locally after delegating: {last_run}"
    );
    assert_eq!(
        last_run.get("runFailed").and_then(Json::as_i64),
        Some(0),
        "the delegation must succeed, not fail the node: {last_run}"
    );

    let tree = last_run
        .get("runTree")
        .and_then(Json::as_array)
        .expect("runTree is an array")
        .iter()
        .map(|v| v.as_str().expect("a tree line is a string").to_string())
        .collect::<Vec<_>>();
    assert!(
        tree.iter().any(|l| l.starts_with("root ")),
        "no root tree line in {tree:?}"
    );

    // The journal shows the node actually reached `discover` and `finish` —
    // ordinary companion bookkeeping, unaffected by delegation. The "finish"
    // entry's own payload (`journalLayer`, `harness-dogfooding/recursive-companion/
    // Harness.hs`) carries the coalgebra's raw `draftText` — the ONE place the
    // delegated subagent's OWN typed result (`delegateSummary`, decoded through
    // the real MockBackend saga) survives into anything this test can observe:
    // the tree line itself (`nodeLine`, `HarnessTypes.hs`) is deliberately just
    // `path  posture  title  badges`, never free text, and the root's
    // `runAnswer`/algebra-fold synthesis is this scenario's SCRIPTED "FOLDED"
    // reply, not a real model reading the child's answer.
    let journal = tidepool_handlers::load_journal(&journal_path).expect("journal loads");
    let kinds: Vec<&str> = journal.iter().map(|e| e.kind.as_str()).collect();
    assert!(
        kinds.contains(&"finish"),
        "the root must journal a finish, got {kinds:?}"
    );
    let finish_draft = journal
        .iter()
        .find(|e| e.kind == "finish")
        .and_then(|e| e.payload.get("draft"))
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("no \"finish\" journal entry with a \"draft\" field"));
    assert!(
        finish_draft.contains("found one file: README.md"),
        "the root's finish must carry the delegated subagent's OWN typed \
         result (delegateSummary), decoded through the real MockBackend \
         saga — not a placeholder: {finish_draft}"
    );
}

/// The half of the mechanism that DOES work end to end: a `Subagent` send
/// written DIRECTLY in a branch-node window's own turn text (no
/// `runDelegate`/`reinterpret2` involved) dispatches through the driver's
/// `SubagentHandler` (`pending_hole_with_request` reading the raw suspended
/// request `Harness` already retains, `resume_with_value` resuming the
/// node's own session — the new plumbing this lane added to
/// `HoleRouting::Subagent`'s existing, pre-`drain_note_holes` servicing) and
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
        answerer_decls_with_delegate(),
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
           Left _err -> finalize @LayerProposal (ProposeFinish { localAnswer = \"spawn failed\" }); \
           Right _cyc -> finalize @LayerProposal (ProposeFinish { localAnswer = \"spawned ok\" }) } }",
    );
    let provider: Arc<dyn DynModelProvider> = Arc::new(KeyedProvider {
        scripted: vec![
            ("NODE root — DISCOVER", direct_send_reply),
            (
                "— FOLD",
                haskell(
                    "finalize @FoldDecision (FoldDecision { foldSynthesis = \"FOLDED\", \
                     foldTensions = [], foldEditsInOrder = [], \
                     foldProposed = [] })",
                ),
            ),
        ],
    });
    let writer = LogWriter::create(&log_path, &header()).expect("log writer");
    let agent = Arc::new(Harness::new(writer, agent_cfg, provider).expect("agent harness boots"));

    let mut driver = SelfHarnessDriver::new(agent, Arc::new(LogObserver));
    driver.set_checkpoint_path(checkpoint_path.clone());
    driver.set_console_handler(ConsoleHandler);
    driver.set_gate(Arc::new(NoGate));
    driver.set_answerer_round_caps(1, 2);
    driver.set_journal_handler(JournalHandler::new(
        SegmentPath::create_exclusive(journal_path.clone())
            .expect("this scenario's journal segment is fresh in its own tempdir"),
    ));

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

    let outcome = driver.run_one_cycle(&source, Some(&restored)).await.expect(
        "one render -> loop -> thoughtHylo -> render cycle, with a direct \
             Subagent send serviced mid-window",
    );

    let state = outcome.state_json;
    let last_run = state
        .get("lastRun")
        .filter(|v| !v.is_null())
        .unwrap_or_else(|| panic!("the cycle recorded no lastRun: {state}"));
    assert_eq!(
        last_run.get("runNodes").and_then(Json::as_i64),
        Some(1),
        "one node (the root): {last_run}"
    );
    assert_eq!(
        last_run.get("runFailed").and_then(Json::as_i64),
        Some(0),
        "the direct Subagent send must be serviced, not fail the node: {last_run}"
    );
}
