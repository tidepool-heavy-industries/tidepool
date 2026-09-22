//! Acceptance tests for the coupled-spawn saga.
//!
//! Two disciplines, both load-bearing:
//!
//! 1. **Git is never mocked.** Every worktree here is a real `git worktree add`
//!    against a real temporary repository (`tidepool_worktree::testing`), for
//!    the same reason that crate gives: a git mock proves the mock agrees with
//!    the author's model of git, which is the thing in doubt.
//! 2. **The model IS mocked, always.** [`MockBackend`] is the only backend any
//!    committed test drives — no live turn, no token, no `~/.codex`.
//!
//! Rollback is asserted from DISK, not from the live table: each gate drops the
//! spawner (releasing `BindingTable`'s lifetime flock), reopens the table at the
//! same root, and reads the persisted rows back. A rollback that only happened
//! in memory would pass an in-process assertion and lose a worktree to a dead
//! agent across the next restart, which is the failure these gates exist for.

use std::path::{Path, PathBuf};

use tidepool_agent::backend::mock::{MockBackend, MockFailure, MockStep};
use tidepool_agent::backend::AgentBackend;
use tidepool_agent::seam::{
    AgentBackendError, AgentId, BackendThreadId, CycleResultPayload, CycleSpec, ModelPolicy,
    ReasoningEffort, ThreadSpec, ToolCall, ToolCallId, ToolOutcome, ToolReply, TurnEvent, TurnId,
};
use tidepool_agent::spawn::{
    CoupledSpawner, CycleProgress, CycleSaga, SpawnError, SpawnRequest, SpawnStage, SpawnStep,
    SpawnWorkspace, MAX_TOOL_ROUNDS,
};
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

    assert_eq!(run.run.agent, AgentId(0));
    assert_eq!(run.run.thread, BackendThreadId("mock-thread-0".into()));
    assert_eq!(run.receipt.turn, TurnId("mock-turn-1".into()));
    assert_eq!(run.payload, payload);
    // The EXACT model, never a tier name — literal by design.
    assert_eq!(run.receipt.resolved_model, MockBackend::MODEL);
    // `agent-<id>-<sanitized label>`: the space and `!` collapse to one dash.
    assert_eq!(run.receipt.binding_ref, "agent-0-worker-one");

    // Call-log shape: threads are ephemeral with no dynamic tools, and the
    // cycle runs in the bound worktree with the caller's schema.
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
    assert_eq!(rows[0].state(), BindingState::Terminal);
    assert_eq!(rows[0].agent().as_str(), "agent-0-worker-one");
    assert_eq!(rows[0].worktree(), &worktree);

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
    assert_eq!(second.run.worktree.id(), &worktree);
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
    assert_eq!(rows[0].agent().as_str(), "agent-0-doomed");
    assert_eq!(rows[0].state(), BindingState::Released);
    assert_eq!(rows[1].agent().as_str(), "agent-1-successor");
    assert_eq!(rows[1].state(), BindingState::Terminal);

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
    assert_eq!(rows[0].state(), BindingState::Released);
    assert_eq!(rows[0].agent().as_str(), "agent-0-half-run");

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
    assert_eq!(rows[0].agent().as_str(), "agent-99-squatter");
    assert_eq!(rows[0].state(), BindingState::Active);
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

// ============================================================================
// The DRIVEN saga: `begin` parks on a tool call, `answer` drives on. Same two
// disciplines as above — real git, mocked model — plus the same rule that the
// rolled-back end state is read back off DISK.
//
// The mock stays a scripted list of stops: every row below is "given exactly
// these events, the saga does X". Nothing here teaches it protocol behavior.
// ============================================================================

/// The only worktree the fixture's registry knows about. Every driven row
/// creates exactly one, and reads its id back through the registry rather than
/// through the spawner (which does not expose the running agent's workspace,
/// deliberately — nothing but the saga needs it).
fn only_worktree(fixture: &Fixture) -> WorktreeId {
    let mut ids: Vec<WorktreeId> = fixture
        .manager()
        .list()
        .expect("list")
        .into_iter()
        .map(|s| s.receipt.worktree_id)
        .collect();
    assert_eq!(ids.len(), 1, "expected exactly one worktree, got {ids:?}");
    ids.pop().expect("checked just above")
}

fn expect_parked(step: SpawnStep) -> (AgentId, ToolCall) {
    match step {
        SpawnStep::ToolCall { agent, call } => (agent, call),
        SpawnStep::Done(run) => panic!("expected a parked tool call, the turn finished: {run:?}"),
    }
}

/// A turn that parks and then completes settles the binding Terminal ONCE: one
/// lease row, in the finished state, for a run that stopped twice.
#[test]
fn parked_turn_completes_and_settles_binding_terminal_once() {
    let fixture = Fixture::init();
    let mut spawner = fixture.spawner();
    let payload = CycleResultPayload::Structured(serde_json::json!({ "summary": "answered" }));
    let mut backend = MockBackend::scripted([
        MockStep::Calls {
            tool: "ask_parent".to_string(),
            arguments: serde_json::json!({ "q": "which file?" }),
        },
        MockStep::Completes(payload.clone()),
    ]);

    let req = request(
        SpawnWorkspace::New(WorktreeSpec::from_current_repository("parker")),
        "parker",
    );
    let (agent, call) = expect_parked(spawner.begin(&mut backend, &req).expect("begin parks"));
    assert_eq!(agent, AgentId(0));
    assert_eq!(spawner.running_agents(), vec![AgentId(0)]);
    assert_eq!(call.tool, "ask_parent");
    assert_eq!(call.arguments, serde_json::json!({ "q": "which file?" }));

    // PARKED means the agent is still alive: the binding on disk is Active,
    // because a parked turn is not a finished one.
    let worktree = only_worktree(&fixture);
    let parked_rows = rows_on_disk(&fixture.binding_root(), &worktree);
    assert_eq!(parked_rows.len(), 1, "one lease row: {parked_rows:?}");
    assert_eq!(
        parked_rows[0].state(),
        BindingState::Active,
        "a parked turn must not settle its binding — the agent has not finished"
    );

    let step = spawner
        .answer(
            &mut backend,
            agent,
            call.call.clone(),
            ToolOutcome::Answered(serde_json::json!({ "file": "a.txt" })),
        )
        .expect("answering the parked call drives the turn on");
    let run = match step {
        SpawnStep::Done(run) => *run,
        SpawnStep::ToolCall { call, .. } => panic!("the script completes here, got {call:?}"),
    };

    assert_eq!(run.payload, payload);
    assert_eq!(
        run.receipt.rounds, 1,
        "one answered call is one round, and the receipt is where that is checkable"
    );
    assert!(
        spawner.running_agents().is_empty(),
        "the agent's life is over"
    );
    // The parent's answer reached the backend, correlated to the parked call.
    assert_eq!(backend.replies.len(), 1);
    assert_eq!(backend.replies[0].call, call.call);
    assert_eq!(
        backend.replies[0].outcome,
        ToolOutcome::Answered(serde_json::json!({ "file": "a.txt" }))
    );

    drop(spawner);

    let rows = rows_on_disk(&fixture.binding_root(), &worktree);
    assert_eq!(
        rows.len(),
        1,
        "the two-stop turn settled ONE lease, not two: {rows:?}"
    );
    assert_eq!(rows[0].state(), BindingState::Terminal);
    assert_eq!(rows[0].agent().as_str(), "agent-0-parker");
}

/// A backend failure DURING the resume is the same rollback rule as a failure
/// during the first turn: the binding settles Released and the worktree is
/// retained.
#[test]
fn resume_backend_failure_rolls_back_binding_and_retains_worktree() {
    let fixture = Fixture::init();
    let mut spawner = fixture.spawner();
    let mut backend = MockBackend::scripted([
        MockStep::Calls {
            tool: "ask_parent".to_string(),
            arguments: serde_json::json!({}),
        },
        MockStep::Fails(AgentBackendError::RunFailed {
            detail: "model died mid-turn".to_string(),
        }),
    ]);

    let req = request(
        SpawnWorkspace::New(WorktreeSpec::from_current_repository("half-answered")),
        "half-answered",
    );
    let (agent, call) = expect_parked(spawner.begin(&mut backend, &req).expect("begin parks"));

    let err = spawner
        .answer(
            &mut backend,
            agent,
            call.call,
            ToolOutcome::Answered(serde_json::json!({ "ok": true })),
        )
        .expect_err("the resume was injected to fail");

    match &err {
        SpawnError::Backend { stage, error } => {
            assert_eq!(*stage, SpawnStage::Running);
            assert_eq!(
                error,
                &AgentBackendError::RunFailed {
                    detail: "model died mid-turn".to_string()
                }
            );
        }
        other => panic!("expected Backend at running, got {other:?}"),
    }
    assert_eq!(
        spawner.running_agents(),
        Vec::new(),
        "a failed resume ends the agent — it must not stay half-running"
    );

    let worktree = only_worktree(&fixture);
    drop(spawner);

    let rows = rows_on_disk(&fixture.binding_root(), &worktree);
    assert_eq!(rows.len(), 1, "one lease row: {rows:?}");
    assert_eq!(rows[0].state(), BindingState::Released);
    assert_eq!(rows[0].agent().as_str(), "agent-0-half-answered");

    assert_retained_and_unbound(&fixture, &worktree);
}

/// A reply naming the wrong AGENT is refused, and refusing costs the running
/// agent nothing: it stays parked and can still be answered.
#[test]
fn answering_the_wrong_agent_is_refused_and_the_agent_stays_running() {
    let fixture = Fixture::init();
    let mut spawner = fixture.spawner();
    let mut backend = MockBackend::scripted([
        MockStep::Calls {
            tool: "ask_parent".to_string(),
            arguments: serde_json::json!({}),
        },
        MockStep::Completes(CycleResultPayload::Absent),
    ]);

    let req = request(
        SpawnWorkspace::New(WorktreeSpec::from_current_repository("misrouted")),
        "misrouted",
    );
    let (agent, call) = expect_parked(spawner.begin(&mut backend, &req).expect("begin parks"));

    let err = spawner
        .answer(
            &mut backend,
            AgentId(9),
            call.call.clone(),
            ToolOutcome::Answered(serde_json::json!({})),
        )
        .expect_err("agent 9 is not the one mid-turn");

    match &err {
        SpawnError::NotRunning {
            agent: named,
            detail,
        } => {
            // The error names the agent the CALLER asked for, and the detail
            // names the one actually running — both facts a misroute needs.
            assert_eq!(*named, AgentId(9));
            assert!(detail.contains("agent 0"), "{detail}");
        }
        other => panic!("expected NotRunning, got {other:?}"),
    }
    assert!(
        backend.replies.is_empty(),
        "a misrouted reply must never reach the backend"
    );
    assert_eq!(
        spawner.running_agents(),
        vec![agent],
        "the refusal costs the running agent nothing"
    );

    // Still answerable: the refusal did not consume the parked call.
    let step = spawner
        .answer(
            &mut backend,
            agent,
            call.call,
            ToolOutcome::Answered(serde_json::json!({})),
        )
        .expect("the correctly-addressed answer still works");
    assert!(matches!(step, SpawnStep::Done(_)));
}

/// A reply naming the wrong CALL is refused for the same reason, and likewise
/// leaves the parked call answerable.
#[test]
fn answering_the_wrong_call_is_refused_and_the_agent_stays_running() {
    let fixture = Fixture::init();
    let mut spawner = fixture.spawner();
    let mut backend = MockBackend::scripted([
        MockStep::Calls {
            tool: "ask_parent".to_string(),
            arguments: serde_json::json!({}),
        },
        MockStep::Completes(CycleResultPayload::Absent),
    ]);

    let req = request(
        SpawnWorkspace::New(WorktreeSpec::from_current_repository("stale-call")),
        "stale-call",
    );
    let (agent, call) = expect_parked(spawner.begin(&mut backend, &req).expect("begin parks"));

    let err = spawner
        .answer(
            &mut backend,
            agent,
            ToolCallId("some-other-call".to_string()),
            ToolOutcome::Answered(serde_json::json!({})),
        )
        .expect_err("that call is not the parked one");

    match &err {
        SpawnError::NotRunning {
            agent: named,
            detail,
        } => {
            assert_eq!(*named, agent);
            assert!(
                detail.contains(&call.call.0) && detail.contains("some-other-call"),
                "the detail must name both the parked call and the one offered: {detail}"
            );
        }
        other => panic!("expected NotRunning, got {other:?}"),
    }
    assert!(backend.replies.is_empty());
    assert_eq!(spawner.running_agents(), vec![agent]);

    let step = spawner
        .answer(
            &mut backend,
            agent,
            call.call,
            ToolOutcome::Answered(serde_json::json!({})),
        )
        .expect("the correctly-addressed answer still works");
    assert!(matches!(step, SpawnStep::Done(_)));
}

/// The runtime's hard backstop. It is not the authored round cap (that is
/// resident policy, enforced in the Haskell loop, which refuses politely);
/// reaching this one means the policy cap was absent or broken, so it fails
/// loudly AND rolls back rather than letting a loop spend an unwatched budget.
#[test]
fn round_backstop_fires_and_rolls_back() {
    let fixture = Fixture::init();
    let mut spawner = fixture.spawner();
    // One stop for the first park, then one per answer the backstop permits.
    let mut backend = MockBackend::scripted((0..=MAX_TOOL_ROUNDS).map(|i| MockStep::Calls {
        tool: "spin".to_string(),
        arguments: serde_json::json!({ "round": i }),
    }));

    let req = request(
        SpawnWorkspace::New(WorktreeSpec::from_current_repository("spinner")),
        "spinner",
    );
    let (agent, mut call) = expect_parked(spawner.begin(&mut backend, &req).expect("begin parks"));

    for round in 1..=MAX_TOOL_ROUNDS {
        let step = spawner
            .answer(
                &mut backend,
                agent,
                call.call,
                ToolOutcome::Answered(serde_json::json!({})),
            )
            .unwrap_or_else(|e| panic!("round {round} is under the backstop, got {e:?}"));
        call = expect_parked(step).1;
    }

    let err = spawner
        .answer(
            &mut backend,
            agent,
            call.call,
            ToolOutcome::Answered(serde_json::json!({})),
        )
        .expect_err("the backstop must fire on the round past the limit");
    assert_eq!(
        err,
        SpawnError::RoundBackstop {
            agent,
            limit: MAX_TOOL_ROUNDS,
        }
    );
    assert_eq!(
        err.stage(),
        SpawnStage::Running,
        "the backstop can only fire with a turn in flight"
    );
    assert_eq!(
        backend.replies.len(),
        MAX_TOOL_ROUNDS as usize,
        "the refused round never reached the backend"
    );
    assert!(spawner.running_agents().is_empty());

    let worktree = only_worktree(&fixture);
    drop(spawner);

    let rows = rows_on_disk(&fixture.binding_root(), &worktree);
    assert_eq!(rows.len(), 1, "one lease row: {rows:?}");
    assert_eq!(
        rows[0].state(),
        BindingState::Released,
        "the backstop rolls back like any other post-Bound failure"
    );

    assert_retained_and_unbound(&fixture, &worktree);
}

// ============================================================================
// N CYCLES AT A TIME. One spawner, many sagas.
//
// This section replaces the old "a second `begin` is refused" gate, which
// asserted a constraint that no longer exists. Its successor is its opposite:
// two `begin`s under one spawner both succeed. The MISROUTE gates above are
// untouched and matter more now, not less — with N cycles in flight, "which
// agent is this reply for" stops being rhetorical.
// ============================================================================

/// Two `begin`s under ONE spawner both succeed, both agents are running, and
/// answering one drives ONLY that one.
///
/// Each cycle gets its own backend, because an `AgentBackend` is a step
/// function over one live thread — two cycles sharing one would interleave
/// their `resume`s onto the same session.
#[test]
fn two_concurrent_begins_both_run_and_answers_route_by_agent() {
    let fixture = Fixture::init();
    let mut spawner = fixture.spawner();
    let mut first_backend = MockBackend::scripted([
        MockStep::Calls {
            tool: "ask_parent".to_string(),
            arguments: serde_json::json!({ "who": "first" }),
        },
        MockStep::Completes(CycleResultPayload::Structured(
            serde_json::json!({ "summary": "first" }),
        )),
    ]);
    let mut second_backend = MockBackend::scripted([
        MockStep::Calls {
            tool: "ask_parent".to_string(),
            arguments: serde_json::json!({ "who": "second" }),
        },
        MockStep::Completes(CycleResultPayload::Structured(
            serde_json::json!({ "summary": "second" }),
        )),
    ]);

    let first = request(
        SpawnWorkspace::New(WorktreeSpec::from_current_repository("first")),
        "first",
    );
    let (agent_a, call_a) = expect_parked(
        spawner
            .begin(&mut first_backend, &first)
            .expect("first begin parks"),
    );

    let second = request(
        SpawnWorkspace::New(WorktreeSpec::from_current_repository("second")),
        "second",
    );
    let (agent_b, call_b) = expect_parked(
        spawner
            .begin(&mut second_backend, &second)
            .expect("a second begin is an ordinary spawn, not a refusal"),
    );

    assert_ne!(agent_a, agent_b, "each cycle mints its own agent id");
    assert_eq!(
        spawner.running_agents(),
        vec![agent_a, agent_b],
        "both cycles are in flight, sorted"
    );

    // Two worktrees, two binding files — each cycle allocated its own.
    let mut worktrees: Vec<WorktreeId> = fixture
        .manager()
        .list()
        .expect("list")
        .into_iter()
        .map(|s| s.receipt.worktree_id)
        .collect();
    worktrees.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    assert_eq!(worktrees.len(), 2, "one worktree per cycle: {worktrees:?}");
    assert_eq!(binding_files(&fixture.binding_root()).len(), 2);

    // Answering A drives A alone: B's backend saw no reply at all.
    let step = spawner
        .answer(
            &mut first_backend,
            agent_a,
            call_a.call.clone(),
            ToolOutcome::Answered(serde_json::json!({ "for": "a" })),
        )
        .expect("A's parked call is answerable");
    match step {
        SpawnStep::Done(run) => assert_eq!(
            run.payload,
            CycleResultPayload::Structured(serde_json::json!({ "summary": "first" }))
        ),
        other => panic!("A's script completes here, got {other:?}"),
    }
    assert_eq!(first_backend.replies.len(), 1);
    assert!(
        second_backend.replies.is_empty(),
        "answering A must not touch B's backend"
    );
    assert_eq!(
        spawner.running_agents(),
        vec![agent_b],
        "A finished; B is untouched and still running"
    );

    // B is still parked on its OWN call and finishes independently.
    let step = spawner
        .answer(
            &mut second_backend,
            agent_b,
            call_b.call,
            ToolOutcome::Answered(serde_json::json!({ "for": "b" })),
        )
        .expect("B's parked call is still answerable after A finished");
    match step {
        SpawnStep::Done(run) => assert_eq!(
            run.payload,
            CycleResultPayload::Structured(serde_json::json!({ "summary": "second" }))
        ),
        other => panic!("B's script completes here, got {other:?}"),
    }
    assert!(spawner.running_agents().is_empty());

    drop(spawner);

    // Both cycles settled their OWN binding Terminal.
    for worktree in &worktrees {
        let rows = rows_on_disk(&fixture.binding_root(), worktree);
        assert_eq!(rows.len(), 1, "one lease row per worktree: {rows:?}");
        assert_eq!(rows[0].state(), BindingState::Terminal);
    }
}

/// A misroute across CONCURRENT cycles: answering agent A's call while
/// addressing agent B reaches neither backend, and costs neither cycle its
/// parked call.
///
/// The single-agent misroute rows above prove the check exists; this proves it
/// still discriminates when there is more than one right answer to "who is
/// running".
#[test]
fn answering_a_concurrent_sibling_is_refused_and_reaches_no_backend() {
    let fixture = Fixture::init();
    let mut spawner = fixture.spawner();
    let script = || {
        MockBackend::scripted([
            MockStep::Calls {
                tool: "ask_parent".to_string(),
                arguments: serde_json::json!({}),
            },
            MockStep::Completes(CycleResultPayload::Absent),
        ])
    };
    let mut backend_a = script();
    let mut backend_b = script();

    let (agent_a, call_a) = expect_parked(
        spawner
            .begin(
                &mut backend_a,
                &request(
                    SpawnWorkspace::New(WorktreeSpec::from_current_repository("a")),
                    "a",
                ),
            )
            .expect("A begins"),
    );
    let (agent_b, call_b) = expect_parked(
        spawner
            .begin(
                &mut backend_b,
                &request(
                    SpawnWorkspace::New(WorktreeSpec::from_current_repository("b")),
                    "b",
                ),
            )
            .expect("B begins"),
    );

    // A call id that is nobody's, offered under B's agent id. Deliberately a
    // SYNTHETIC id rather than A's: a backend mints call ids per SESSION, so
    // two concurrent backends both name their first call `mock-call-0` and A's
    // id is literally equal to B's. That collision is why routing across
    // concurrent cycles is by AGENT — a call id alone does not identify a
    // cycle, which is exactly what the second half of this test pins down.
    assert_eq!(
        call_a.call, call_b.call,
        "per-session call ids collide across concurrent cycles — the agent is the router"
    );
    let stray = ToolCallId("some-other-call".to_string());
    let err = spawner
        .answer(
            &mut backend_b,
            agent_b,
            stray.clone(),
            ToolOutcome::Answered(serde_json::json!({})),
        )
        .expect_err("that is not B's parked call");
    match &err {
        SpawnError::NotRunning { agent, detail } => {
            assert_eq!(*agent, agent_b);
            assert!(
                detail.contains(&call_b.call.0) && detail.contains(&stray.0),
                "the detail must name both the parked call and the one offered: {detail}"
            );
        }
        other => panic!("expected NotRunning, got {other:?}"),
    }

    // An agent nobody is running names WHO is, so a caller can act on it.
    let err = spawner
        .answer(
            &mut backend_a,
            AgentId(99),
            call_a.call.clone(),
            ToolOutcome::Answered(serde_json::json!({})),
        )
        .expect_err("agent 99 is not running");
    match &err {
        SpawnError::NotRunning { agent, detail } => {
            assert_eq!(*agent, AgentId(99));
            assert!(
                detail.contains(&format!("agent {}", agent_a.0))
                    && detail.contains(&format!("agent {}", agent_b.0)),
                "with N running the refusal must name them all: {detail}"
            );
        }
        other => panic!("expected NotRunning, got {other:?}"),
    }

    assert!(backend_a.replies.is_empty(), "no misroute reached A");
    assert!(backend_b.replies.is_empty(), "no misroute reached B");
    assert_eq!(spawner.running_agents(), vec![agent_a, agent_b]);

    // Both refusals cost nothing: each cycle's own call still answers.
    for (backend, agent, call) in [
        (&mut backend_a, agent_a, call_a.call),
        (&mut backend_b, agent_b, call_b.call),
    ] {
        let step = spawner
            .answer(
                backend,
                agent,
                call,
                ToolOutcome::Answered(serde_json::json!({})),
            )
            .expect("the correctly-addressed answer still works");
        assert!(matches!(step, SpawnStep::Done(_)));
    }
}

/// The gated script every concurrency row below drives: park on one tool call
/// (so `begin` returns and the test can observe the cycle mid-flight), then
/// BLOCK inside `resume` until the test releases the gate, then complete.
///
/// `MockStep::Blocks` is seam-level SCHEDULING, not protocol — it is how the
/// order below is pinned by a rendezvous instead of by a sleep.
fn gated_script(summary: &str) -> MockBackend {
    MockBackend::scripted([
        MockStep::Calls {
            tool: "gate".to_string(),
            arguments: serde_json::json!({}),
        },
        MockStep::Blocks,
        MockStep::Completes(CycleResultPayload::Structured(
            serde_json::json!({ "summary": summary }),
        )),
    ])
}

/// Three detached sagas, three threads, three backends, ONE shared substrate —
/// completing in an order the test PINS with `MockControl` rather than by
/// sleeping.
///
/// This is the row that proves the substrate lock is never held across a
/// backend call. Every cycle is bound AND blocked inside `resume` at the same
/// time; if a saga held the mutex across a backend call, the second cycle could
/// not even allocate its worktree, and this test would deadlock rather than
/// fail. The three-Active-bindings assertion below is that fact stated
/// directly.
#[test]
fn three_detached_sagas_complete_out_of_spawn_order_on_three_threads() {
    let fixture = Fixture::init();
    let spawner = fixture.spawner();
    // The handle a cycle thread carries — a clone of the shared substrate, not
    // a second one (a second `BindingTable` over this root would fail at the
    // flock).
    let substrate = spawner.substrate();

    let labels = ["alpha", "bravo", "charlie"];
    let mut controls = Vec::new();
    let mut handles = Vec::new();
    let (bound_tx, bound_rx) = std::sync::mpsc::channel::<(usize, WorktreeId)>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<usize>();

    for (index, label) in labels.iter().enumerate() {
        let mut backend = gated_script(label);
        controls.push(backend.control());

        let substrate = substrate.clone();
        let bound_tx = bound_tx.clone();
        let done_tx = done_tx.clone();
        let request = request(
            SpawnWorkspace::New(WorktreeSpec::from_current_repository(*label)),
            label,
        );
        handles.push(std::thread::spawn(move || {
            // The WHOLE saga runs on this thread: allocate, bind, start the
            // thread, run the turn.
            let progress = CycleSaga::begin(&substrate, &mut backend, &request)
                .expect("each cycle begins independently");
            let parked = match progress {
                CycleProgress::Parked(parked) => parked,
                CycleProgress::Done(run) => {
                    panic!("the script parks before it completes: {run:?}")
                }
            };
            bound_tx
                .send((index, parked.saga().worktree().id().clone()))
                .expect("report the binding");

            // Refuses the gate call, then blocks inside `resume` until this
            // cycle's own control is released.
            let run = CycleProgress::Parked(parked)
                .run_to_completion(&mut backend)
                .expect("the released cycle completes");
            done_tx.send(index).expect("report completion");
            run
        }));
    }
    drop(bound_tx);
    drop(done_tx);

    // Wait for all three to be BOUND before releasing any of them. Three
    // simultaneous Active bindings is precisely what one-agent-at-a-time made
    // impossible.
    let mut worktrees: Vec<Option<WorktreeId>> = vec![None; labels.len()];
    for _ in 0..labels.len() {
        let (index, worktree) = bound_rx.recv().expect("every cycle binds");
        worktrees[index] = Some(worktree);
    }
    let worktrees: Vec<WorktreeId> = worktrees.into_iter().map(|w| w.expect("bound")).collect();
    {
        let bindings = spawner.bindings();
        for worktree in &worktrees {
            let binding = bindings
                .current(worktree)
                .unwrap_or_else(|| panic!("{worktree} must be Active while its cycle runs"));
            assert_eq!(binding.state(), BindingState::Active);
        }
    }
    let mut distinct: Vec<&str> = worktrees.iter().map(|w| w.as_str()).collect();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 3, "one worktree per cycle: {worktrees:?}");

    // Release in an order DIFFERENT from the spawn order and read completions
    // back off the channel: the order is pinned by the gate, never by timing.
    let release_order = [2usize, 0, 1];
    let mut completion_order = Vec::new();
    for index in release_order {
        controls[index].release();
        completion_order.push(done_rx.recv().expect("a released cycle completes"));
    }
    assert_eq!(
        completion_order, release_order,
        "completion order follows RELEASE order, not spawn order"
    );

    let runs: Vec<_> = handles
        .into_iter()
        .map(|h| h.join().expect("cycle thread"))
        .collect();

    // Three distinct agents and three distinct binding refs — nothing was
    // shared or reused across the cycles.
    let mut agents: Vec<AgentId> = runs.iter().map(|r| r.run.agent).collect();
    agents.sort_unstable();
    agents.dedup();
    assert_eq!(agents.len(), 3, "three distinct agent ids");

    let mut refs: Vec<String> = runs.iter().map(|r| r.receipt.binding_ref.clone()).collect();
    refs.sort();
    refs.dedup();
    assert_eq!(refs.len(), 3, "three distinct binding refs");

    // Each cycle got ITS OWN payload and its own worktree — completion order is
    // not an input to any result.
    for (index, label) in labels.iter().enumerate() {
        let run = runs
            .iter()
            .find(|r| r.receipt.binding_ref.ends_with(label))
            .unwrap_or_else(|| panic!("a run for {label}"));
        assert_eq!(
            run.payload,
            CycleResultPayload::Structured(serde_json::json!({ "summary": label }))
        );
        assert_eq!(
            run.run.worktree.id(),
            &worktrees[index],
            "{label} completed in the worktree it bound"
        );
    }

    drop(runs);
    drop(spawner);
    drop(substrate);

    for worktree in &worktrees {
        let rows = rows_on_disk(&fixture.binding_root(), worktree);
        assert_eq!(rows.len(), 1, "one lease row per cycle: {rows:?}");
        assert_eq!(
            rows[0].state(),
            BindingState::Terminal,
            "every completed cycle settles its OWN binding Terminal"
        );
    }
}

/// A saga blocked inside a backend call is reaped from ANOTHER thread by the
/// backend's canceller, and the cycle then settles `Released` with the worktree
/// still registered.
///
/// Retain-first is locked: cancellation SETTLES, it deletes nothing. Killing
/// alone would leave a binding row Active against an agent that will never run
/// again — the exact orphan the saga's rollback semantics exist to prevent — so
/// the assertion here is about the binding STATE, not merely that the call
/// returned.
#[test]
fn a_blocked_saga_is_cancelled_from_another_thread_and_settles_released() {
    let fixture = Fixture::init();
    let spawner = fixture.spawner();

    let mut backend = gated_script("never-reached");
    // Taken BEFORE the cycle runs: once the cycle thread holds `&mut backend`
    // nothing else can reach it, which is exactly why a canceller is a separate
    // `Send + Sync` object rather than a `&mut self` method.
    let canceller = backend.canceller();

    let progress = spawner
        .begin_detached(
            &mut backend,
            &request(
                SpawnWorkspace::New(WorktreeSpec::from_current_repository("blocked")),
                "blocked",
            ),
        )
        .expect("begin_detached parks on the gate call");
    let parked = match progress {
        CycleProgress::Parked(parked) => parked,
        CycleProgress::Done(run) => panic!("the script parks before it completes: {run:?}"),
    };
    let agent = parked.agent();
    let call = parked.call().clone();
    let worktree = parked.saga().worktree().id().clone();
    let mut saga = parked.into_saga();

    // Active while the cycle is live — a blocked turn is not a finished one.
    let active = rows_on_disk(&fixture.binding_root(), &worktree);
    assert_eq!(active.len(), 1, "one lease row: {active:?}");
    assert_eq!(active[0].state(), BindingState::Active);

    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        started_tx.send(()).expect("announce the drive");
        // Refuses the gate call (this agent was created with no dynamic
        // tools), which is what blocks inside `resume` until this cycle's
        // canceller is released — same drive `CycleProgress::run_to_completion`
        // does, spelled out here because this gate needs the saga back
        // afterward to assert on directly.
        let refusal = ToolOutcome::Refused(format!(
            "no such tool: {} — this agent was created with no dynamic tools",
            call.tool
        ));
        let err = saga
            .answer(&mut backend, agent, call.call, refusal)
            .expect_err("a cancelled cycle cannot complete");
        (saga, err)
    });
    started_rx.recv().expect("the cycle thread started");

    // Reap from THIS thread while the cycle thread is inside the seam call.
    // `cancel` is a latch, so it is correct whether the cycle has reached its
    // gate yet or not — no sleep, no ordering assumption.
    canceller.cancel();

    let (mut saga, err) = handle.join().expect("cycle thread");
    match &err {
        SpawnError::Backend { stage, error } => {
            assert_eq!(*stage, SpawnStage::Running);
            assert_eq!(
                error,
                &AgentBackendError::RunFailed {
                    detail: "cancelled".to_string()
                }
            );
        }
        other => panic!("expected Backend at running, got {other:?}"),
    }

    // The reap already settled the binding through the saga's own rollback, and
    // `abandon` on a settled saga is a no-op — it must not write a second row.
    assert!(saga.is_finished());
    saga.abandon().expect("abandon is idempotent");
    saga.abandon().expect("abandon twice is still a no-op");

    // Assert BEFORE dropping: the binding must not be Active even while the
    // table is still open.
    assert!(
        spawner.bindings().current(&worktree).is_none(),
        "a cancelled cycle must leave NO Active binding"
    );

    drop(saga);
    drop(spawner);

    let rows = rows_on_disk(&fixture.binding_root(), &worktree);
    assert_eq!(
        rows.len(),
        1,
        "cancellation settles ONE lease, never two: {rows:?}"
    );
    assert_eq!(
        rows[0].state(),
        BindingState::Released,
        "cancel SETTLES: a killed cycle's binding is Released, not left Active"
    );
    assert_eq!(rows[0].agent().as_str(), "agent-0-blocked");
    assert_retained_and_unbound(&fixture, &worktree);
}

/// `abandon()` on a live saga settles `Released` exactly once, and on a saga
/// that already settled — completed or rolled back — it is `Ok(())` writing
/// nothing.
///
/// Idempotence is not tidiness: a cancel racing a completion is a real
/// sequence, and a second settle would record two lease rows for a life that
/// ended once.
#[test]
fn abandon_settles_released_once_and_is_a_no_op_on_a_settled_saga() {
    let fixture = Fixture::init();
    let spawner = fixture.spawner();

    // --- 1. A live, parked saga: abandon settles Released. ---
    let mut parked_backend = MockBackend::scripted([
        MockStep::Calls {
            tool: "ask_parent".to_string(),
            arguments: serde_json::json!({}),
        },
        MockStep::Completes(CycleResultPayload::Absent),
    ]);
    let progress = spawner
        .begin_detached(
            &mut parked_backend,
            &request(
                SpawnWorkspace::New(WorktreeSpec::from_current_repository("abandoned")),
                "abandoned",
            ),
        )
        .expect("begin_detached");
    let parked = match progress {
        CycleProgress::Parked(parked) => parked,
        CycleProgress::Done(run) => panic!("this script parks on ask_parent first: {run:?}"),
    };
    // `into_saga` for the raw, repeatable `abandon()` this gate needs —
    // `ParkedCycle::abandon` is consuming, correct for a caller with one shot
    // at cancelling, but this gate calls it three times to pin idempotence.
    let mut saga = parked.into_saga();
    let abandoned = saga.worktree().id().clone();
    assert!(!saga.is_finished());

    saga.abandon().expect("abandon a live cycle");
    assert!(saga.is_finished(), "an abandoned saga is finished");
    saga.abandon().expect("a second abandon is a no-op");
    saga.abandon().expect("and a third");

    // --- 2. A COMPLETED saga: abandon must not settle a second time. ---
    let mut completed_backend = MockBackend::completing(CycleResultPayload::Absent);
    let progress = spawner
        .begin_detached(
            &mut completed_backend,
            &request(
                SpawnWorkspace::New(WorktreeSpec::from_current_repository("completed")),
                "completed",
            ),
        )
        .expect("begin_detached");
    // A cycle that completes at its very first stop yields `Done` straight
    // from `begin_detached` — no `CycleSaga` survives to (mis)call `abandon`
    // on. What used to be a runtime idempotency check is now a type that
    // cannot represent the misuse.
    let run = match progress {
        CycleProgress::Done(run) => run,
        CycleProgress::Parked(parked) => panic!(
            "a script that completes at the first stop parks nowhere: {:?}",
            parked.call()
        ),
    };
    let finished = run.run.worktree.id().clone();

    // --- 3. A ROLLED-BACK saga: same rule, reached through a failure. ---
    let mut failed_backend =
        MockBackend::failing(MockFailure::AtCycle(AgentBackendError::RunFailed {
            detail: "model refused".to_string(),
        }));
    let err = spawner
        .begin_detached(
            &mut failed_backend,
            &request(
                SpawnWorkspace::New(WorktreeSpec::from_current_repository("rolled-back")),
                "rolled-back",
            ),
        )
        .expect_err("the cycle was injected to fail");
    assert!(matches!(err, SpawnError::Backend { .. }));
    let rolled_back = fixture
        .manager()
        .list()
        .expect("list")
        .into_iter()
        .map(|s| s.receipt.worktree_id)
        .find(|id| *id != abandoned && *id != finished)
        .expect("the failed spawn still created a worktree");

    drop(saga);
    drop(run);
    drop(spawner);

    // One row each, in the state that cycle's ending earned — never two.
    for (worktree, state) in [
        (&abandoned, BindingState::Released),
        (&finished, BindingState::Terminal),
        (&rolled_back, BindingState::Released),
    ] {
        let rows = rows_on_disk(&fixture.binding_root(), worktree);
        assert_eq!(
            rows.len(),
            1,
            "exactly one lease row for {worktree}: {rows:?}"
        );
        assert_eq!(rows[0].state(), state, "for {worktree}");
    }
    assert_retained_and_unbound(&fixture, &abandoned);
    assert_retained_and_unbound(&fixture, &rolled_back);
}
