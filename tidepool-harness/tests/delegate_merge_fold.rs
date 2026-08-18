//! PRD 21 C5 — the joint live-path acceptance the delegate-effect lane and
//! the node-worktree-tree lane each declared missing: a branch-node window
//! calls `delegate`, a (mock-backend) agent cycle commits REAL content in its
//! bound worktree, the parent's merge fold integrates that branch in
//! declared order, and the root worktree carries the result — one sibling
//! carries no worktree content at all, pinning the mixed case.
//!
//! Every piece this test drives is individually landed and green:
//! `delegate_positive_path.rs` proves delegation dispatch through the real
//! `SubagentHandler`/`MockBackend` saga (this file reuses the exact same
//! `answerer_decls_with_delegate()` + `EngineConfig::with_delegate_wrap()`
//! wiring, now the SAME wiring `tidepool-web/src/bin/tidepool-selfharness.rs`
//! selects live for the recursive-companion harness); `tidepool_worktree::
//! merge::merge_branch_into` is the exact primitive
//! `harness-dogfooding/recursive-companion/Harness.hs`'s `mergeChildInto`
//! calls through `Exec` (`tidepool-worktree/CLAUDE.md`'s narrow PRD 21 C5
//! exception); `companion_recursive_slice.rs` proves the scripted-provider,
//! multi-node fold surface this test's tree shape reuses.
//!
//! **What this test does NOT drive live**: `Harness.hs`'s own `mergeFold`
//! reads its declared-order plan from each child's `NodeAnswer
//! .answerMergeBranch`, and nothing in the shipped harness today wires a
//! `delegate` result into that field — its own doc comment
//! (`Harness.hs`, "The merge fold — PRD 21 lane C5") says so explicitly:
//! "The design also names a node's OWN subagent spawn as an acquisition
//! trigger; that rides through the very same `answerMergeBranch` channel
//! once a node window has a route to produce one (a sibling lane's...)".
//! That route is a separate, still-open gap. This test therefore composes
//! the individually-proven mechanisms directly — delegate dispatch through
//! the real driver, and the real `merge_branch_into` primitive driven by
//! this test in the SAME declared order `mergePlan` would compute (a pure,
//! total filter over `Maybe Text`s, already pinned exhaustively by
//! `dogfood_harness_typecheck.rs`'s
//! `recursive_companion_merge_fold_decisions_execute`) — rather than
//! asserting on a live `Harness.hs` merge that cannot yet happen.
//!
//! GHC-heavy: needs `TIDEPOOL_EXTRACT` + the with-packages GHC on PATH
//! (`--ignore-default-filter` to run).

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value as Json};

use tidepool_agent::backend::mock::MockBackend;
use tidepool_agent::{
    AgentBackend, AgentBackendError, BackendThreadId, CycleResultPayload, CycleSpec, ThreadSpec,
    ToolReply, TurnEvent,
};
use tidepool_handlers::{
    load_journal, ConsoleHandler, JournalHandler, SegmentPath, SubagentHandler,
};
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
use tidepool_worktree::{
    BranchName, GitCli, MergeOutcome, WorktreeManager, WorktreeRegistry, WorktreeSpec,
};

// ---------------------------------------------------------------------------
// Where the real harness lives
// ---------------------------------------------------------------------------

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
        "delegate-merge-fold-{}-{label}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn header() -> LogHeader {
    LogHeader {
        prelude_hash: "delegate-merge-fold".into(),
        extract_fingerprint: "delegate-merge-fold".into(),
        harness_version: "test".into(),
    }
}

// ---------------------------------------------------------------------------
// The scripted provider — needle SETS (companion_recursive_slice.rs's
// pattern, adapted): a node id is derived from a model-produced branch
// title, so a scenario names a child by POSITION ("NODE root/2-") and PHASE
// ("— DISCOVER") rather than hardcoding the slug the harness derives.
// ---------------------------------------------------------------------------

struct Script {
    needles: Vec<&'static str>,
    reply: String,
}

fn script(needles: &[&'static str], reply: String) -> Script {
    Script {
        needles: needles.to_vec(),
        reply,
    }
}

fn window_message(req: &TurnRequest) -> String {
    req.messages
        .iter()
        .rev()
        .find(|m| m.content.contains(" — DISCOVER") || m.content.contains(" — FOLD"))
        .or_else(|| req.messages.last())
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

struct KeyedProvider {
    scripted: Vec<Script>,
}

impl ModelProvider for KeyedProvider {
    async fn complete(
        &self,
        req: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let last = window_message(&req);
        let reply = self
            .scripted
            .iter()
            .find(|s| s.needles.iter().all(|n| last.contains(n)))
            .map(|s| s.reply.clone())
            .ok_or_else(|| {
                ProviderError::Api(format!(
                    "KeyedProvider: no scripted reply matches the request:\n{last}"
                ))
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

/// This scenario runs `GateOff` — no form should ever be presented.
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

/// `finalize @LayerProposal (ProposeSplit …)` — `branches` is
/// `(title, role, instruction)` in declared order. Verbatim from
/// `companion_recursive_slice.rs`'s builder of the same name.
fn split_reply(
    posture: &str,
    strategy: &str,
    focus: &str,
    branches: &[(&str, &str, &str)],
) -> String {
    let rendered: Vec<String> = branches
        .iter()
        .map(|(title, role, instruction)| {
            format!(
                "ProposedBranch {{ branchTitle = \"{title}\", branchRole = {role}, \
                 branchInstruction = \"{instruction}\" }}"
            )
        })
        .collect();
    haskell(&format!(
        "finalize @LayerProposal (ProposeSplit {{ splitPosture = {posture}, splitFocus = \
         \"{focus}\", splitStrategy = {strategy}, splitBranches = [{}] }})",
        rendered.join(", ")
    ))
}

/// The layer this scenario's root proposes: `Scout` (declared FIRST, never
/// delegates — the content-less sibling) and `Builder` (declared SECOND,
/// delegates and produces real content) — declared in THIS order so the
/// merge-plan filter below has to skip a `None` at position 1 to reach the
/// `Some` at position 2, rather than trivially keeping "the only entry".
fn split_scout_and_builder() -> String {
    split_reply(
        "Explore",
        "WantSequential",
        "which branch should proceed",
        &[
            ("Scout", "Primary", "look around; do not delegate"),
            (
                "Builder",
                "Alternative",
                "delegate a subagent to produce real content",
            ),
        ],
    )
}

/// A plain leaf: no delegation, no worktree content — the sibling PRD 21 C5's
/// mixed case pins.
fn finish_reply() -> String {
    haskell(
        "finalize @LayerProposal (ProposeFinish { localAnswer = \"this node answers locally\" })",
    )
}

/// The delegating leaf: calls `delegate` (dispatching through the real
/// `Subagent`/`Worktree` machinery via `Tidepool.Agent.Delegate.runDelegate`,
/// exactly as `delegate_positive_path.rs`'s `delegating_reply` does at the
/// root), then finalizes on what came back.
fn delegating_reply() -> String {
    haskell(
        "do { r <- delegate (DelegateBrief { delegateLabel = \"builder\", \
         delegateInstruction = \"commit real content to your worktree\", \
         delegateExpected = \"a one-line summary\" }); \
         case r of { \
           Left e -> finalize @LayerProposal (ProposeFinish { localAnswer = renderDelegateError e }); \
           Right ok -> finalize @LayerProposal (ProposeFinish { localAnswer = delegateSummary ok }) } }",
    )
}

fn fold_script() -> Script {
    script(
        &["— FOLD"],
        haskell(
            "finalize @FoldDecision (FoldDecision { foldSynthesis = \"FOLDED\", \
             foldTensions = [], foldEditsInOrder = [], foldProposed = [] })",
        ),
    )
}

fn state_json() -> Json {
    json!({
        "question": "SCENARIO: a branch node delegates; the parent's merge fold integrates the result.",
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

// ---------------------------------------------------------------------------
// A backend that makes MockBackend's scripted completion real: it commits a
// file into whatever worktree `CycleSpec.cwd` names before handing off to
// the inner mock. MockBackend itself never touches git (tidepool-agent's own
// module doc: "it answers seam-trait calls from a fixed script... it knows
// nothing about... any Codex error shape" — no filesystem effect anywhere),
// so a REAL commit needs this thin shim, mirroring
// `subagent_one_cycle.rs`'s `RecordingBackend` idiom (wrap + forward) plus
// `spawn_saga.rs`'s proof that `CycleSpec.cwd` IS the bound worktree's own
// path.
// ---------------------------------------------------------------------------

struct CommittingBackend {
    inner: MockBackend,
    git: GitCli,
    rel_path: String,
    contents: String,
    message: String,
    /// `(branch, worktree cwd)` of the commit this backend actually made —
    /// the test's own record, read back after the cycle completes.
    committed: Arc<Mutex<Option<(String, PathBuf)>>>,
}

impl CommittingBackend {
    fn new(
        inner: MockBackend,
        rel_path: &str,
        contents: &str,
        message: &str,
        committed: Arc<Mutex<Option<(String, PathBuf)>>>,
    ) -> Self {
        Self {
            inner,
            git: GitCli::new(),
            rel_path: rel_path.to_string(),
            contents: contents.to_string(),
            message: message.to_string(),
            committed,
        }
    }

    fn run_git(
        &self,
        cwd: &Path,
        args: &[&str],
    ) -> Result<tidepool_worktree::GitOutput, AgentBackendError> {
        self.git
            .try_run(cwd, args)
            .map_err(|e| AgentBackendError::RunFailed {
                detail: e.to_string(),
            })
    }
}

impl AgentBackend for CommittingBackend {
    fn start_thread(&mut self, spec: &ThreadSpec) -> Result<BackendThreadId, AgentBackendError> {
        self.inner.start_thread(spec)
    }

    fn start_turn(
        &mut self,
        thread: &BackendThreadId,
        spec: &CycleSpec,
    ) -> Result<TurnEvent, AgentBackendError> {
        let cwd = PathBuf::from(&spec.cwd);
        let file_path = cwd.join(&self.rel_path);
        std::fs::write(&file_path, &self.contents).map_err(|e| AgentBackendError::RunFailed {
            detail: format!("write {}: {e}", file_path.display()),
        })?;
        self.run_git(&cwd, &["add", "--", self.rel_path.as_str()])?;
        self.run_git(&cwd, &["commit", "-q", "-m", self.message.as_str()])?;
        let branch_out = self.run_git(&cwd, &["symbolic-ref", "--short", "HEAD"])?;
        *self.committed.lock().unwrap() = Some((branch_out.trimmed().to_string(), cwd));
        self.inner.start_turn(thread, spec)
    }

    fn resume(&mut self, reply: ToolReply) -> Result<TurnEvent, AgentBackendError> {
        self.inner.resume(reply)
    }
}

// ---------------------------------------------------------------------------
// The journal's "merge" receipt — the SAME wire shape (`{seq,kind,key,
// payload}`, `tidepool_handlers::JournalEntry::to_json`) and the SAME
// `kind`/payload shape `Harness.hs`'s own `foldAt` writes (`record "merge"
// key (object ["branch" .= mergeBranch, "steps" .= mergeNotes])`) — appended
// directly because `SegmentPath`'s only public constructor
// (`create_exclusive`) refuses an already-created path, so there is no
// public Rust API to append to a segment `run_one_cycle`'s own
// `JournalHandler` already wrote to. This is this test's stand-in for what
// `Harness.hs` itself would journal once the live delegate ->
// `answerMergeBranch` route (this file's module doc) lands.
// ---------------------------------------------------------------------------

fn append_merge_journal_line(path: &Path, key: &str, branch: &str, steps: &[String]) {
    use std::io::Write;
    let line = json!({
        "seq": 1_000_000_u64,
        "kind": "merge",
        "key": key,
        "payload": { "branch": branch, "steps": steps },
    });
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .expect("journal segment opens for append");
    writeln!(file, "{line}").expect("journal line writes");
}

// ---------------------------------------------------------------------------
// The joint acceptance
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branch_node_delegate_commits_and_the_merge_fold_integrates_it_in_declared_order() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();

    let dir = scratch("run");
    let checkpoint_path = dir.join("checkpoint.json");
    let journal_path = dir.join("journal.jsonl");
    let log_path = dir.join("log.jsonl");

    // The SAME delegating wiring the production binary now selects live for
    // the recursive-companion harness (tidepool-web/src/bin/
    // tidepool-selfharness.rs, gated on `is_recursive_companion`).
    let agent_cfg = EngineConfig::from_decls(
        answerer_decls_with_delegate(),
        repo_root().join("haskell/lib"),
        Some(companion_dir()),
    )
    .expect("delegating answerer engine config over the recursive-companion harness dir")
    .with_delegate_wrap();

    let provider: Arc<dyn DynModelProvider> = Arc::new(KeyedProvider {
        scripted: vec![
            script(&["NODE root — DISCOVER"], split_scout_and_builder()),
            // Position 1 (declared FIRST): Scout, no delegation, no content.
            script(&["NODE root/1-", "— DISCOVER"], finish_reply()),
            // Position 2 (declared SECOND): Builder, delegates.
            script(&["NODE root/2-", "— DISCOVER"], delegating_reply()),
            fold_script(),
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

    // The real saga: a real temporary git repo, a real worktree/binding
    // table, `MockBackend` wrapped by `CommittingBackend` so the delegated
    // cycle's completion is backed by a REAL commit — same tier
    // `tidepool-agent/CLAUDE.md` names as the ceiling for this lane (mock
    // backend, no live model, no live codex).
    let store = TestRepo::init().expect("git init the delegation target repo");
    store
        .writer()
        .commit_file("README.md", "hello\n", "seed commit")
        .expect("seed commit");
    let roots = tempfile::TempDir::new().expect("substrate roots");
    let committed: Arc<Mutex<Option<(String, PathBuf)>>> = Arc::new(Mutex::new(None));
    let backend = CommittingBackend::new(
        MockBackend::completing(CycleResultPayload::Structured(json!({
            "delegateSummary": "committed BUILD_MARKER.txt",
            "delegateCaveats": [],
        }))),
        "BUILD_MARKER.txt",
        "built by the delegated subagent\n",
        "delegated build",
        committed.clone(),
    );
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
        .expect("one render -> loop -> thoughtHylo -> render cycle, one child delegating");

    let state = outcome.state_json;
    let last_run = state
        .get("lastRun")
        .filter(|v| !v.is_null())
        .unwrap_or_else(|| panic!("the cycle recorded no lastRun: {state}"));
    assert_eq!(
        last_run.get("runNodes").and_then(Json::as_i64),
        Some(3),
        "root + the two declared children (Scout, Builder): {last_run}"
    );
    assert_eq!(
        last_run.get("runFailed").and_then(Json::as_i64),
        Some(0),
        "neither child, nor the delegation, may fail the run: {last_run}"
    );

    // --- the delegate call actually produced a real commit ------------------
    let (branch, child_cwd) = committed
        .lock()
        .unwrap()
        .clone()
        .expect("the delegating child's backend committed real content in its bound worktree");
    let landed_in_child = std::fs::read_to_string(child_cwd.join("BUILD_MARKER.txt"))
        .expect("the committed file exists in the child's own bound worktree");
    assert_eq!(landed_in_child, "built by the delegated subagent\n");

    // --- declared-order merge plan: mergePlan's own semantics ---------------
    // Scout (declared position 1) carries no branch; Builder (position 2)
    // does. `mergePlan :: [Maybe Text] -> Maybe (NonEmpty Text)` is exactly
    // `NE.nonEmpty . catMaybes` (pinned exhaustively, and executed on the
    // real JIT, by `dogfood_harness_typecheck.rs`'s
    // `recursive_companion_merge_fold_decisions_execute`) — this mirrors
    // that same declared-order filter over the REAL branch this run produced.
    let declared: Vec<Option<String>> = vec![None, Some(branch.clone())];
    let plan: Vec<String> = declared.into_iter().flatten().collect();
    assert_eq!(
        plan,
        vec![branch.clone()],
        "mergePlan keeps only content-bearing children, in declared order — the \
         no-content sibling declared FIRST must not shift the survivor's position"
    );

    // --- the merge fold: the REAL primitive mergeChildInto calls -----------
    // Lazy acquisition, mirrored: `mergeFold` only creates a worktree once it
    // has a real plan (`createWorktree (fromCurrentRepository (renderPath
    // path))`); this test does the same, over the SAME source repository the
    // delegated child's own worktree came from.
    let root_registry =
        WorktreeRegistry::open(dir.join("root-registry")).expect("root registry opens");
    let root_manager = WorktreeManager::new(
        GitCli::new(),
        root_registry,
        dir.join("root-worktrees"),
        store.path().to_path_buf(),
    );
    let root_handle = root_manager
        .create(&WorktreeSpec::from_current_repository("root"))
        .expect("the root worktree acquires, mirroring mergeFold's lazy acquisition");

    let mut merge_notes = Vec::new();
    for b in &plan {
        let outcome = tidepool_worktree::merge_branch_into(
            root_manager.git(),
            root_handle.cwd(),
            &BranchName::from_raw(b.clone()),
            &format!("fold {b}"),
        )
        .expect("merge runs");
        match outcome {
            MergeOutcome::Merged { .. } => merge_notes.push(format!("{b}: merged cleanly")),
            MergeOutcome::Conflict { paths } => {
                panic!("unexpected conflict merging {b} into the root worktree: {paths:?}")
            }
        }
    }
    assert_eq!(
        merge_notes.len(),
        1,
        "exactly one content-bearing child to fold"
    );

    // --- the root worktree carries the result -------------------------------
    let landed_in_root = std::fs::read_to_string(root_handle.cwd().join("BUILD_MARKER.txt"))
        .expect("the delegated content lands in the root worktree after the merge fold");
    assert_eq!(landed_in_root, "built by the delegated subagent\n");

    // --- merge receipts asserted in the journal -----------------------------
    append_merge_journal_line(&journal_path, "root", &branch, &merge_notes);
    let journal = load_journal(&journal_path).expect("journal loads");
    let merge_entries: Vec<_> = journal.iter().filter(|e| e.kind == "merge").collect();
    assert_eq!(
        merge_entries.len(),
        1,
        "exactly one merge receipt: {journal:?}"
    );
    assert_eq!(merge_entries[0].key, "root");
    assert_eq!(
        merge_entries[0]
            .payload
            .get("branch")
            .and_then(Json::as_str),
        Some(branch.as_str())
    );
    assert_eq!(
        merge_entries[0]
            .payload
            .get("steps")
            .and_then(Json::as_array)
            .map(Vec::len),
        Some(1),
        "one merge step, for the one content-bearing child: {:?}",
        merge_entries[0].payload
    );
}
