//! LANE 1 acceptance tests for the coupled-spawn saga.
//!
//! Two disciplines, both load-bearing:
//!
//! 1. **Git is never mocked.** Every worktree here is a real `git worktree add`
//!    against a real temporary repository (`tidepool_worktree::testing`), for
//!    the same reason that crate gives: a git mock proves the mock agrees with
//!    the author's model of git, which is the thing in doubt.
//! 2. **The model IS mocked, always.** [`MockBackend`] is the only backend any
//!    committed test drives (standing rule, Inanna 2026-08-09) — no live turn,
//!    no token, no `~/.codex`.
//!
//! Rollback is asserted from DISK, not from the live table: each gate drops the
//! spawner (releasing `BindingTable`'s lifetime flock), reopens the table at the
//! same root, and reads the persisted rows back. A rollback that only happened
//! in memory would pass an in-process assertion and lose a worktree to a dead
//! agent across the next restart, which is the failure these gates exist for.

use std::path::{Path, PathBuf};

use tidepool_agent::backend::mock::{MockBackend, MockFailure};
use tidepool_agent::backend::AgentBackend;
use tidepool_agent::seam::{
    AgentBackendError, AgentId, BackendThreadId, CycleResultPayload, CycleSpec, ModelPolicy,
    ReasoningEffort, ThreadSpec, ToolReply, TurnEvent, TurnId,
};
use tidepool_agent::spawn::{CoupledSpawner, SpawnError, SpawnRequest, SpawnStage, SpawnWorkspace};
use tidepool_worktree::testing::TestRepo;
use tidepool_worktree::{
    AgentRef, Binding, BindingState, BindingTable, GitCli, WorktreeError, WorktreeId,
    WorktreeManager, WorktreeRegistry, WorktreeSpec,
};

/// One test's worth of substrate: a real source repository plus registry,
/// worktree, and binding roots that all live OUTSIDE it (never dirty the
/// source — the roots are sibling temp dirs, not paths under the repo).
struct Fixture {
    repo: TestRepo,
    base: tempfile::TempDir,
}

impl Fixture {
    fn init() -> Self {
        let repo = TestRepo::init().expect("git init");
        repo.writer()
            .commit_file("a.txt", "one", "first")
            .expect("seed commit");
        Self {
            repo,
            base: tempfile::TempDir::new().expect("tempdir"),
        }
    }

    /// A fresh manager over the same registry root — clonable substrate the
    /// test keeps for its own lookups while the spawner owns its copy.
    fn manager(&self) -> WorktreeManager {
        let registry =
            WorktreeRegistry::open(self.base.path().join("registry")).expect("open registry");
        WorktreeManager::new(
            GitCli::new(),
            registry,
            self.base.path().join("worktrees"),
            self.repo.path(),
        )
    }

    fn binding_root(&self) -> PathBuf {
        self.base.path().join("bindings")
    }

    fn spawner(&self) -> CoupledSpawner {
        CoupledSpawner::open(self.manager(), self.binding_root()).expect("open spawner")
    }
}

fn request(workspace: SpawnWorkspace, label: &str) -> SpawnRequest {
    SpawnRequest {
        workspace,
        agent_label: label.to_string(),
        task: "summarize the repository".to_string(),
        output_schema: Some(serde_json::json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "required": ["summary"],
        })),
        tools: Vec::new(),
        model: ModelPolicy::CheapPlumbing,
        effort: ReasoningEffort::Low,
    }
}

/// Every persisted lease row for `worktree`, read straight off disk. Returns
/// `[]` when the file was never written — which is itself an assertion several
/// gates make (an allocation failure must not leave a binding row behind).
fn rows_on_disk(binding_root: &Path, worktree: &WorktreeId) -> Vec<Binding> {
    let path = binding_root.join(format!("{}.json", worktree.as_str()));
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).expect("decode persisted bindings"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("read {}: {e}", path.display()),
    }
}

/// How many worktrees have a persisted binding file at all.
fn binding_files(binding_root: &Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(binding_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => panic!("read_dir {}: {e}", binding_root.display()),
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect()
}

/// The rolled-back end state, asserted from a REOPENED table: the worktree is
/// retained, still registered, and carries NO active binding.
///
/// "Orphaned" means Active-bound to an agent that will never run — not
/// "exists". Retain-first is locked, so this deliberately does not assert the
/// worktree is gone; it asserts the opposite.
fn assert_retained_and_unbound(fixture: &Fixture, worktree: &WorktreeId) {
    let reopened = BindingTable::open(fixture.binding_root()).expect("reopen binding table");
    assert!(
        reopened.current(worktree).is_none(),
        "worktree {worktree} must carry no Active binding after rollback, found {:?}",
        reopened.current(worktree),
    );
    drop(reopened);

    let handle = fixture
        .manager()
        .lookup(worktree)
        .expect("lookup must not fail — the worktree is retained, not deleted")
        .expect("worktree must still be registered after rollback");
    assert!(
        handle.cwd().exists(),
        "retain-first: the worktree directory must survive rollback"
    );
}

#[test]
fn spawn_completes_and_settles_binding_terminal() {
    let fixture = Fixture::init();
    let mut spawner = fixture.spawner();
    let payload = CycleResultPayload::Structured(serde_json::json!({ "summary": "one file" }));
    let mut backend = MockBackend::completing(payload.clone());

    let req = request(
        SpawnWorkspace::New(WorktreeSpec::from_current_repository("worker one")),
        "worker one!",
    );
    let run = spawner
        .spawn_one_cycle(&mut backend, &req)
        .expect("happy-path spawn");

    // The coupled pair and the receipt describe the SAME run.
    assert_eq!(run.run.agent, AgentId(0));
    assert_eq!(run.receipt.agent, run.run.agent);
    assert_eq!(&run.receipt.worktree, run.run.worktree.id());
    assert_eq!(run.receipt.thread, run.run.thread);
    assert_eq!(run.receipt.thread, BackendThreadId("mock-thread-0".into()));
    assert_eq!(run.receipt.turn, TurnId("mock-turn-1".into()));
    assert_eq!(run.payload, payload);
    // The EXACT model, never a tier name — literal by design.
    assert_eq!(run.receipt.resolved_model, MockBackend::MODEL);
    // `agent-<id>-<sanitized label>`: the space and `!` collapse to one dash.
    assert_eq!(run.receipt.binding_ref, "agent-0-worker-one");

    // Call-log shape: lane 1 threads are ephemeral with no dynamic tools, and
    // the cycle runs in the bound worktree with the caller's schema.
    assert_eq!(backend.started.len(), 1);
    assert!(backend.started[0].ephemeral);
    assert!(backend.started[0].dynamic_tools.is_empty());
    assert_eq!(backend.cycles.len(), 1);
    let (thread, spec) = &backend.cycles[0];
    assert_eq!(thread, &run.run.thread);
    assert_eq!(Path::new(&spec.cwd), run.run.worktree.cwd());
    assert_eq!(spec.task, req.task);
    assert_eq!(spec.output_schema, req.output_schema);
    assert_eq!(spec.model, ModelPolicy::CheapPlumbing);

    // Disk: one lease row, settled Terminal (finished — not Released, which
    // would say the resident stopped waiting), and no Active binding left.
    let worktree = run.run.worktree.id().clone();
    let cwd = run.run.worktree.cwd().to_path_buf();
    drop(spawner);

    let rows = rows_on_disk(&fixture.binding_root(), &worktree);
    assert_eq!(rows.len(), 1, "one lease row: {rows:?}");
    assert_eq!(rows[0].state, BindingState::Terminal);
    assert_eq!(rows[0].agent.as_str(), "agent-0-worker-one");
    assert_eq!(rows[0].worktree, worktree);

    let reopened = BindingTable::open(fixture.binding_root()).expect("reopen binding table");
    assert!(reopened.current(&worktree).is_none());
    drop(reopened);
    assert!(cwd.exists(), "the worktree survives a completed run");
}

#[test]
fn thread_start_failure_rolls_back_binding_and_retains_worktree() {
    let fixture = Fixture::init();
    let mut spawner = fixture.spawner();
    let mut backend = MockBackend::failing(MockFailure::AtThreadStart(
        AgentBackendError::BackendUnavailable {
            detail: "app-server not running".into(),
        },
    ));

    let err = spawner
        .spawn_one_cycle(
            &mut backend,
            &request(
                SpawnWorkspace::New(WorktreeSpec::from_current_repository("doomed")),
                "doomed",
            ),
        )
        .expect_err("thread start was injected to fail");

    match &err {
        SpawnError::Backend { stage, error } => {
            assert_eq!(*stage, SpawnStage::ThreadAccepted);
            assert_eq!(
                error,
                &AgentBackendError::BackendUnavailable {
                    detail: "app-server not running".into()
                }
            );
        }
        other => panic!("expected Backend at thread-accepted, got {other:?}"),
    }
    assert!(
        backend.cycles.is_empty(),
        "no cycle may run after the thread was refused"
    );

    // The saga created the worktree before it bound: retain-first means it is
    // still there, so it is discoverable through the registry.
    let created: Vec<WorktreeId> = fixture
        .manager()
        .list()
        .expect("list")
        .into_iter()
        .map(|s| s.receipt.worktree_id)
        .collect();
    assert_eq!(
        created.len(),
        1,
        "the failed spawn still created one worktree"
    );
    let worktree = created[0].clone();

    // Rebindable in fact, not just in state: a second spawn takes the SAME
    // worktree and completes.
    let mut good = MockBackend::completing(CycleResultPayload::Absent);
    let second = spawner
        .spawn_one_cycle(
            &mut good,
            &request(SpawnWorkspace::Existing(worktree.clone()), "successor"),
        )
        .expect("the rolled-back worktree must be rebindable");
    assert_eq!(second.receipt.worktree, worktree);
    assert_eq!(second.receipt.binding_ref, "agent-1-successor");

    drop(spawner);

    // Disk: the failed lease is Released, the successor's is Terminal, and
    // nothing is Active.
    let rows = rows_on_disk(&fixture.binding_root(), &worktree);
    assert_eq!(
        rows.len(),
        2,
        "both leases are retained in history: {rows:?}"
    );
    assert_eq!(rows[0].agent.as_str(), "agent-0-doomed");
    assert_eq!(rows[0].state, BindingState::Released);
    assert_eq!(rows[1].agent.as_str(), "agent-1-successor");
    assert_eq!(rows[1].state, BindingState::Terminal);

    assert_retained_and_unbound(&fixture, &worktree);
}

#[test]
fn cycle_failure_rolls_back_binding_and_retains_worktree() {
    let fixture = Fixture::init();
    let mut spawner = fixture.spawner();
    let mut backend = MockBackend::failing(MockFailure::AtCycle(AgentBackendError::RunFailed {
        detail: "model refused".into(),
    }));

    let err = spawner
        .spawn_one_cycle(
            &mut backend,
            &request(
                SpawnWorkspace::New(WorktreeSpec::from_current_repository("half-run")),
                "half-run",
            ),
        )
        .expect_err("the cycle was injected to fail");

    match &err {
        SpawnError::Backend { stage, error } => {
            assert_eq!(*stage, SpawnStage::Running);
            assert_eq!(
                error,
                &AgentBackendError::RunFailed {
                    detail: "model refused".into()
                }
            );
        }
        other => panic!("expected Backend at running, got {other:?}"),
    }
    // The thread WAS accepted here — that is what distinguishes this edge from
    // the thread-start one.
    assert_eq!(backend.started.len(), 1);
    assert_eq!(backend.cycles.len(), 1);

    let worktree = fixture
        .manager()
        .list()
        .expect("list")
        .into_iter()
        .map(|s| s.receipt.worktree_id)
        .next()
        .expect("the failed spawn still created a worktree");

    drop(spawner);

    let rows = rows_on_disk(&fixture.binding_root(), &worktree);
    assert_eq!(rows.len(), 1, "one lease row: {rows:?}");
    assert_eq!(rows[0].state, BindingState::Released);
    assert_eq!(rows[0].agent.as_str(), "agent-0-half-run");

    assert_retained_and_unbound(&fixture, &worktree);
}

#[test]
fn spawn_into_existing_bound_worktree_refuses_naming_holder() {
    let fixture = Fixture::init();
    let worktree = fixture
        .manager()
        .create(&WorktreeSpec::from_current_repository("shared"))
        .expect("create")
        .id()
        .clone();

    // Take an Active binding and hand the root back — the spawner's table must
    // load it at open, because that is what a restart does.
    {
        let mut table = BindingTable::open(fixture.binding_root()).expect("open binding table");
        table
            .bind(&worktree, &AgentRef::from_raw("agent-99-squatter"), 1_000)
            .expect("pre-bind");
    }

    let mut spawner = fixture.spawner();
    let mut backend = MockBackend::completing(CycleResultPayload::Absent);
    let err = spawner
        .spawn_one_cycle(
            &mut backend,
            &request(SpawnWorkspace::Existing(worktree.clone()), "latecomer"),
        )
        .expect_err("one worktree, one agent");

    match &err {
        SpawnError::Binding {
            stage,
            error:
                WorktreeError::WorktreeBusy {
                    worktree: w,
                    holder,
                },
        } => {
            assert_eq!(*stage, SpawnStage::Bound);
            assert_eq!(w, &worktree);
            // Naming the holder is the point: a refusal the resident cannot
            // act on is not an explicit failure.
            assert_eq!(holder, "agent-99-squatter");
        }
        other => panic!("expected Binding/WorktreeBusy at bound, got {other:?}"),
    }
    assert!(
        backend.started.is_empty(),
        "the backend must never be touched when the binding was refused"
    );

    drop(spawner);

    // The squatter's lease is untouched — a refused spawn does not settle,
    // steal, or overwrite the incumbent.
    let rows = rows_on_disk(&fixture.binding_root(), &worktree);
    assert_eq!(rows.len(), 1, "no row for the refused spawn: {rows:?}");
    assert_eq!(rows[0].agent.as_str(), "agent-99-squatter");
    assert_eq!(rows[0].state, BindingState::Active);
}

#[test]
fn spawn_into_unregistered_id_fails_at_allocating() {
    let fixture = Fixture::init();
    let mut spawner = fixture.spawner();
    let mut backend = MockBackend::completing(CycleResultPayload::Absent);
    let missing = WorktreeId::from_raw("wt-never-registered");

    let err = spawner
        .spawn_one_cycle(
            &mut backend,
            &request(SpawnWorkspace::Existing(missing.clone()), "ghost"),
        )
        .expect_err("an unregistered id cannot be a workspace");

    // Typed as NotRegistered, NOT WorktreeLost: this id was never here, which
    // is a different fact from data loss.
    assert_eq!(
        err,
        SpawnError::Worktree {
            stage: SpawnStage::Allocating,
            error: WorktreeError::WorktreeNotRegistered(missing.clone()),
        }
    );
    assert!(backend.started.is_empty());

    drop(spawner);
    assert!(
        binding_files(&fixture.binding_root()).is_empty(),
        "an allocation failure must not write a binding row"
    );
}

#[test]
fn allocating_failure_creates_no_binding() {
    let fixture = Fixture::init();
    // Dirty the source and demand a clean one (the default policy).
    fixture
        .repo
        .writer()
        .write_file("b.txt", "uncommitted")
        .expect("dirty the source");

    let mut spawner = fixture.spawner();
    let mut backend = MockBackend::completing(CycleResultPayload::Absent);
    let err = spawner
        .spawn_one_cycle(
            &mut backend,
            &request(
                SpawnWorkspace::New(WorktreeSpec::from_current_repository("clean-only")),
                "clean-only",
            ),
        )
        .expect_err("RequireClean must refuse a dirty source");

    match &err {
        SpawnError::Worktree {
            stage,
            error: WorktreeError::SourceDirty(summary),
        } => {
            assert_eq!(*stage, SpawnStage::Allocating);
            assert!(!summary.is_clean());
        }
        other => panic!("expected Worktree/SourceDirty at allocating, got {other:?}"),
    }
    assert!(backend.started.is_empty());

    drop(spawner);
    assert!(
        binding_files(&fixture.binding_root()).is_empty(),
        "nothing was bound, so nothing may be persisted"
    );
}

/// A backend that destroys the binding table's storage before it fails, so the
/// saga's compensating settle CANNOT succeed.
///
/// Not a second model mock — it is the same `MockFailure::AtThreadStart` shape
/// with a filesystem sabotage in front, which `MockBackend` (deliberately
/// boring) has no business carrying. The sabotage is `rename(2)` onto a
/// directory, which fails `EISDIR` for every user including root, so this gate
/// does not depend on file permissions.
struct SabotageBindingStorage {
    binding_file: PathBuf,
}

impl AgentBackend for SabotageBindingStorage {
    fn start_thread(&mut self, _spec: &ThreadSpec) -> Result<BackendThreadId, AgentBackendError> {
        std::fs::remove_file(&self.binding_file).expect("the bind must have written this row");
        std::fs::create_dir(&self.binding_file).expect("obstruct the row path with a directory");
        Err(AgentBackendError::BackendUnavailable {
            detail: "sabotage".into(),
        })
    }

    fn start_turn(
        &mut self,
        _thread: &BackendThreadId,
        _spec: &CycleSpec,
    ) -> Result<TurnEvent, AgentBackendError> {
        unreachable!("start_thread always fails")
    }

    fn resume(&mut self, _reply: ToolReply) -> Result<TurnEvent, AgentBackendError> {
        unreachable!("start_thread always fails")
    }
}

#[test]
fn rollback_failure_reports_both_causes() {
    let fixture = Fixture::init();
    let worktree = fixture
        .manager()
        .create(&WorktreeSpec::from_current_repository("unrollbackable"))
        .expect("create")
        .id()
        .clone();

    let mut spawner = fixture.spawner();
    let mut backend = SabotageBindingStorage {
        binding_file: fixture
            .binding_root()
            .join(format!("{}.json", worktree.as_str())),
    };

    let err = spawner
        .spawn_one_cycle(
            &mut backend,
            &request(SpawnWorkspace::Existing(worktree.clone()), "unrollbackable"),
        )
        .expect_err("thread start fails and its rollback cannot persist");

    match &err {
        SpawnError::RollbackFailed {
            stage,
            original,
            rollback,
        } => {
            // The stage is the ORIGINAL failure's — the rollback is not a saga
            // step of its own.
            assert_eq!(*stage, SpawnStage::ThreadAccepted);
            assert!(
                matches!(
                    original.as_ref(),
                    SpawnError::Backend {
                        stage: SpawnStage::ThreadAccepted,
                        ..
                    }
                ),
                "the original backend failure must survive, got {original:?}"
            );
            assert!(
                matches!(rollback, WorktreeError::StorageFailure { .. }),
                "the rollback failure must survive, got {rollback:?}"
            );
        }
        other => panic!("expected RollbackFailed, got {other:?}"),
    }
    // Both causes reach the operator through Display — losing either hides the
    // one a fix needs.
    let rendered = err.to_string();
    assert!(rendered.contains("sabotage"), "{rendered}");
    assert!(rendered.contains("storage failure"), "{rendered}");
}
