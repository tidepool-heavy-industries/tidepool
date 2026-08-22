//! PRD 21 C5 — the joint live-path acceptance the delegate-effect lane and
//! the node-worktree-tree lane each declared missing: a branch-node window
//! calls `delegate`, a (mock-backend) agent cycle commits REAL content in its
//! bound worktree, the parent's merge fold integrates that branch in
//! declared order, and the root worktree carries the result — one sibling
//! carries no worktree content at all, pinning the mixed case.
//!
//! **This drives the REAL `Harness.hs` path end to end** — the aspiration
//! this file's own module doc used to name as still-open: a branch-node
//! window delegates in its own DISCOVER window, the runtime stamps the
//! completed cycle's bound branch keyed by that node (`SelfHarnessDriver`'s
//! `branch_node_paths`/`delegated_branches`, PRD 21 C5's final wiring),
//! `foldAt` reads it back via `takeDelegatedBranches` and populates
//! `answerMergeBranch`, and the PARENT's `mergeFold` — completely unmodified
//! machinery — merges it into the root worktree it lazily acquires. Nothing
//! here composes the mechanisms by hand anymore: `run_one_cycle` alone
//! produces the merged root worktree and the real "merge" journal entry.
//!
//! Every piece this test drives is individually landed and green:
//! `delegate_positive_path.rs` proves delegation dispatch through the real
//! `SubagentHandler`/`MockBackend` saga (this file reuses the exact same
//! `answerer_decls_with_delegate()` + `EngineConfig::with_delegate_wrap()`
//! wiring, the SAME wiring `tidepool-web/src/bin/tidepool-selfharness.rs`
//! selects live for the recursive-companion harness); `companion_recursive_slice.rs`
//! proves the scripted-provider, multi-node fold surface this test's tree
//! shape reuses.
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
    load_journal, ConsoleHandler, ExecHandler, JournalHandler, SegmentPath, SubagentHandler,
    WorktreeHandler,
};
use tidepool_harness::engine::EngineConfig;
use tidepool_harness::log::{LogHeader, LogWriter};
use tidepool_harness::provider::DynModelProvider;
use tidepool_harness::selfharness::operator::FormShape;
use tidepool_harness::selfharness::persistence;
use tidepool_harness::{
    answerer_decls_with_delegate, load_harness_source, Harness, LogObserver, OperatorGate,
    SelfHarnessDriver,
};
use tidepool_worktree::registry::WorktreeRegistry;
use tidepool_worktree::testing::TestRepo;
use tidepool_worktree::GitCli;

use support::scripted_provider::{script, KeyedProvider, PathKey, Phase, Script};

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
// The scripted provider — keyed structurally on (path, phase) via
// `support::scripted_provider` (companion_recursive_slice.rs's pattern,
// commit b135d174; this file used to re-derive its own needle-SET matcher —
// see the shared module's doc for why (path, phase) beats scattered needles).
// ---------------------------------------------------------------------------

/// This scenario runs `GateOff` — no form should ever be presented.
struct NoGate;

impl OperatorGate for NoGate {
    fn present_form(&self, _shape: &FormShape) -> Json {
        panic!("this scenario runs GateOff — no form should ever be presented")
    }
}

fn haskell(block: &str) -> String {
    format!("```haskell\n{block}\n```")
}

/// `finalize @LayerProposal (ProposeSplit …)` — `branches` is
/// `(title, role, instruction)` in declared order. Verbatim from
/// `companion_recursive_slice.rs`'s builder of the same name.
fn split_reply(posture: &str, focus: &str, branches: &[(&str, &str, &str)]) -> String {
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
         \"{focus}\", splitBranches = [{}] }})",
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

/// The root's OWN fold — scripted SEPARATELY from the children's, and
/// deliberately dishonest: its prose CLAIMS a branch name
/// (`totally-bogus-branch-xyz`) that was never delegated, never committed,
/// and does not exist anywhere in the repository. `FoldDecision` has no
/// field a model could use to actually NAME a branch — `foldSynthesis` is
/// free text nobody downstream treats as an identifier — so this is the
/// closest thing to an attempt at "attesting to its own execution" the type
/// even permits, and the assertions below confirm it changes nothing: the
/// real merged branch (in the journal AND in git) is the runtime-stamped
/// one, never this string.
fn root_fold_claiming_bogus_branch() -> Script {
    script(
        PathKey::Exact("root"),
        Phase::Fold,
        haskell(
            "finalize @FoldDecision (FoldDecision { foldSynthesis = \"FOLDED (this fold \
             hereby claims the merged branch was totally-bogus-branch-xyz)\", \
             foldTensions = [] })",
        ),
    )
}

/// Every OTHER fold window (Scout's own leaf fold, Builder's own leaf
/// fold) — a plain, uneventful narrative fold.
fn leaf_fold_script() -> Script {
    script(
        PathKey::Prefix(""),
        Phase::Fold,
        haskell(
            "finalize @FoldDecision (FoldDecision { foldSynthesis = \"FOLDED\", \
             foldTensions = [] })",
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

    let provider: Arc<dyn DynModelProvider> = Arc::new(KeyedProvider::new(vec![
        script(
            PathKey::Exact("root"),
            Phase::Discover,
            split_scout_and_builder(),
        ),
        // Position 1 (declared FIRST): Scout, no delegation, no content.
        script(PathKey::Prefix("root/1-"), Phase::Discover, finish_reply()),
        // Position 2 (declared SECOND): Builder, delegates.
        script(
            PathKey::Prefix("root/2-"),
            Phase::Discover,
            delegating_reply(),
        ),
        // Root's own fold — scripted first (more specific key) so it wins
        // over the generic leaf-fold fallback below.
        root_fold_claiming_bogus_branch(),
        leaf_fold_script(),
    ]));

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

    // The REAL worktree/exec wiring `Harness.hs`'s own `mergeFold` needs:
    // `createWorktree` (Worktree) and `gitIn` (Exec) dispatch through these,
    // exactly like the production binary's own `set_worktree_handler`/
    // `set_exec_handler` calls. Rooted at the SAME source repository the
    // delegated cycle commits into (`store`, below), so the branch it
    // delegates is reachable from the root worktree this fold acquires.
    let root_worktree_registry_root = dir.join("root-worktree-registry");
    let root_worktree_root = dir.join("root-worktrees");

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

    driver.set_worktree_handler(
        WorktreeHandler::new(
            root_worktree_registry_root.clone(),
            root_worktree_root.clone(),
            store.path().to_path_buf(),
        )
        .expect("root worktree handler opens"),
    );
    driver.set_exec_handler(ExecHandler::new(root_worktree_root.clone()));

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

    let outcome = driver.run_one_cycle(&source, Some(&restored)).await.expect(
        "one render -> loop -> thoughtHylo -> render cycle, one child delegating, \
             the parent's merge fold acquiring and merging for real",
    );

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
        "neither child, nor the delegation, nor the merge fold may fail the run: {last_run}"
    );
    let root_answer = last_run
        .get("runAnswer")
        .and_then(Json::as_str)
        .unwrap_or_default();
    assert!(
        root_answer.contains("totally-bogus-branch-xyz"),
        "sanity: the scripted root fold's dishonest claim really was delivered as \
         ordinary prose (not swallowed) — otherwise the assertions below prove \
         nothing: {root_answer}"
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

    // --- the REAL Harness.hs path: foldAt -> answerMergeBranch -> mergeFold -
    // No manual composition anywhere below — `run_one_cycle` alone acquired
    // the root worktree and merged Builder's delegated branch into it.
    let root_registry = WorktreeRegistry::open(&root_worktree_registry_root)
        .expect("the root worktree registry `mergeFold`'s own `createWorktree` wrote to reopens");
    let root_worktrees = root_registry
        .list()
        .expect("listing the root registry succeeds");
    assert_eq!(
        root_worktrees.len(),
        1,
        "exactly one worktree acquired: root's own (lazily, on Builder's content) — \
         Scout never triggers acquisition and Builder's OWN delegated worktree lives \
         in the SubagentHandler's separate registry, not this one: {root_worktrees:?}"
    );
    let root_receipt = &root_worktrees[0].receipt;
    assert_ne!(
        root_receipt.branch.as_str(),
        branch,
        "root's own branch is a FRESH worktree mergeFold acquired, never the \
         delegated branch itself — Builder's branch rides INTO it, not in its place"
    );

    // --- the root worktree carries the result -------------------------------
    let landed_in_root = std::fs::read_to_string(root_receipt.cwd.join("BUILD_MARKER.txt"))
        .expect("the delegated content lands in the root worktree after the real merge fold");
    assert_eq!(landed_in_root, "built by the delegated subagent\n");

    // --- the merge receipt: the REAL one `Harness.hs`'s own `foldAt` wrote --
    let journal = load_journal(&journal_path).expect("journal loads");
    let merge_entries: Vec<_> = journal.iter().filter(|e| e.kind == "merge").collect();
    assert_eq!(
        merge_entries.len(),
        1,
        "exactly one merge receipt — Builder's leaf `foldAt` hands its branch up \
         directly (no merge of its own, no worktree of its own: it never had \
         content-bearing children), so ROOT's is the only node that ever calls \
         `createWorktree`: {journal:?}"
    );
    assert_eq!(merge_entries[0].key, "root");
    assert_eq!(
        merge_entries[0]
            .payload
            .get("branch")
            .and_then(Json::as_str),
        Some(root_receipt.branch.as_str()),
        "the journaled branch is the RUNTIME's own root worktree branch"
    );
    let steps = merge_entries[0]
        .payload
        .get("steps")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        steps.len(),
        1,
        "one merge step, for the one content-bearing child (Builder): {steps:?}"
    );
    assert_eq!(
        steps[0].as_str(),
        Some(format!("{branch}: merged cleanly").as_str()),
        "the merged step names Builder's REAL, runtime-observed delegated branch"
    );

    // --- the model CANNOT override the branch --------------------------------
    // The scripted root fold's prose claimed `totally-bogus-branch-xyz` (and,
    // per the sanity check above, that claim really did reach `runAnswer`).
    // Neither the journal's `branch`/`steps`, nor the actual git branch
    // merged, nor the root worktree's content, mention it anywhere — the
    // fold window was never consulted for the branch identity at all
    // (`Harness.hs`'s `ownDelegatedBranch`/`mergeFold` never read
    // `FoldDecision` to begin with), so there was never a channel for this
    // claim to travel through.
    assert_ne!(
        merge_entries[0]
            .payload
            .get("branch")
            .and_then(Json::as_str),
        Some("totally-bogus-branch-xyz"),
        "the model's claimed branch name must not appear where the runtime-stamped \
         branch belongs"
    );
    assert!(
        !steps[0]
            .as_str()
            .unwrap_or_default()
            .contains("totally-bogus-branch-xyz"),
        "the model's claimed branch name must not appear in the real merge step: {steps:?}"
    );
}
