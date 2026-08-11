use std::path::PathBuf;

use tidepool_agent::backend::OneCycleBackend;
use tidepool_agent::seam::{AgentBackendError, AgentId, BackendThreadId, CycleResultPayload};
use tidepool_agent::spawn::{
    CoupledSpawner, OneCycleRun, SpawnError as DomainSpawnError, SpawnReceipt, SpawnRequest,
    SpawnStage, SpawnWorkspace, WorkerRun,
};
use tidepool_worktree::error::WorktreeError as DomainWorktreeError;
use tidepool_worktree::git::GitCli;
use tidepool_worktree::registry::WorktreeRegistry;
use tidepool_worktree::WorktreeManager;

use crate::effect_glue::JsonArg;
use crate::handlers::worktree::{
    error_to_wire as worktree_error_to_wire, handle_to_wire, spec_from_wire, worktree_id_from_wire,
    worktree_id_to_wire, WorktreeError as WireWorktreeError,
};
use tidepool_bridge_effects::{
    AgAgentId, AgBackendFailure, AgBackendThreadId, AgCyclePayload, AgSpawnOutcome, AgSpawnReceipt,
    AgSpawnSpec, AgSpawnStage, AgSpawnWorkspace, AgWorkerRun,
};

// ============================================================================
// Tag: Subagent (PRD 18 lane 1 — coupled agent+worktree spawn; deliberately
// NOT in the default base_effects! row, same opt-in status as Worktree /
// RepoEvent. A row containing Subagent must also contain Worktree — the
// generated types reference WorktreeSpec/WorktreeHandle/WorktreeError.)
// ============================================================================

// SubagentReq + DescribeEffect + EffectHandler dispatch + the wire SpawnError
// enum are generated from the single-source definition; only the handler
// struct and the per-verb method bodies below are hand-written.
tidepool_mcp::subagent_effect_def!(crate::effect_glue::effect_rust_projection);

/// Serves `SubagentSpawn`: the whole coupled-spawn saga behind ONE verb.
///
/// Owns the worktree substrate handles (via [`CoupledSpawner`] — whose
/// `BindingTable` holds the single-owner lifetime flock for its binding root)
/// and a [`OneCycleBackend`]. Production wires the codex adapter; every
/// committed test wires [`tidepool_agent::backend::mock::MockBackend`] — no
/// live-model turns in tests, ever (standing rule, Inanna 2026-08-09).
///
/// Not `Clone`, deliberately (RepoEventHandler precedent): it owns a boxed
/// backend and a flocked binding table, neither of which has a meaningful
/// second owner.
pub struct SubagentHandler {
    spawner: CoupledSpawner,
    backend: Box<dyn OneCycleBackend + Send>,
}

impl SubagentHandler {
    /// `registry_root`, `worktree_root`, and `binding_root` must live OUTSIDE
    /// `source_repository` — the never-dirty-the-source rule
    /// (`tidepool-worktree/CLAUDE.md`). Fallible: opening the registry and the
    /// binding table both are, and the binding table refuses a root another
    /// live table owns.
    pub fn new(
        registry_root: PathBuf,
        worktree_root: PathBuf,
        binding_root: PathBuf,
        source_repository: PathBuf,
        backend: Box<dyn OneCycleBackend + Send>,
    ) -> Result<Self, DomainWorktreeError> {
        let registry = WorktreeRegistry::open(&registry_root)?;
        let manager =
            WorktreeManager::new(GitCli::new(), registry, worktree_root, source_repository);
        Ok(Self {
            spawner: CoupledSpawner::open(manager, binding_root)?,
            backend,
        })
    }

    /// The spawner, for post-run assertions in tests (binding state, registry
    /// lookups) without reopening flocked state.
    pub fn spawner(&self) -> &CoupledSpawner {
        &self.spawner
    }

    // ------------------------------------------------------------------
    // Verb methods (errors-tagged: typed Result, no cx — the generated
    // dispatch arm wraps with cx.respond, Ok→Right / Err→Left).
    // ------------------------------------------------------------------

    /// One atomic coupled spawn + one cycle: wire→domain conversion of `spec`
    /// (`schema` rides through as the backend `output_schema`), then
    /// `spawner.spawn_one_cycle(backend, req)`, then a total domain→wire
    /// conversion of the outcome / error.
    ///
    /// Every branch here is a conversion or a delegation — the saga itself
    /// lives in `tidepool_agent::spawn`, including all rollback. This method
    /// adds no policy of its own beyond the trust-boundary id check that
    /// `request_from_wire` performs before any path is built.
    fn subagent_spawn(
        &mut self,
        spec: AgSpawnSpec,
        schema: JsonArg,
    ) -> Result<AgSpawnOutcome, SpawnError> {
        let request = request_from_wire(spec, schema)?;
        let run = self
            .spawner
            .spawn_one_cycle(&mut *self.backend, &request)
            .map_err(spawn_error_to_wire)?;
        Ok(outcome_to_wire(&run))
    }
}

// ============================================================================
// Wire <-> domain conversions.
//
// Same rules as `handlers::worktree`'s section of the same name: the
// `tidepool_bridge_effects::Ag*` types are WIRE types, their field ORDER is
// the wire contract (positionally matching `subagent_effect_def!`'s
// `type_defs`), and the domain types they mirror live in `tidepool_agent`.
// The Worktree-shaped pieces are NOT re-converted here — they reuse
// `handlers::worktree`'s `pub(crate)` conversions so one contract has one
// conversion.
// ============================================================================

/// The trust boundary for the whole verb. An `SpawnExistingWorktree` id is
/// validated with `WorktreeId::is_path_safe` BEFORE it can reach the registry
/// or the binding table, both of which join ids into file paths — same
/// precedent, and the same `WorktreeNotRegistered` spelling, as
/// `handlers::worktree::worktree_id_from_wire`. Wrapped at
/// `StageAllocating`: nothing has been allocated when this fails.
///
/// `serde_json::Value::Null` for `schema` means "no schema" — the Haskell
/// `Value` argument is total, so the absence of a schema arrives as `Null`
/// rather than as a missing argument, and `CycleSpec::output_schema` is
/// `Option`. A literal `null` schema would constrain nothing anyway.
fn request_from_wire(spec: AgSpawnSpec, schema: JsonArg) -> Result<SpawnRequest, SpawnError> {
    let workspace = match spec.spawn_workspace {
        AgSpawnWorkspace::SpawnNewWorktree(wire_spec) => {
            SpawnWorkspace::New(spec_from_wire(wire_spec).map_err(allocating_worktree_failure)?)
        }
        AgSpawnWorkspace::SpawnExistingWorktree(wire_id) => SpawnWorkspace::Existing(
            worktree_id_from_wire(&wire_id).map_err(allocating_worktree_failure)?,
        ),
    };
    Ok(SpawnRequest {
        workspace,
        agent_label: spec.spawn_agent_label,
        task: spec.spawn_task,
        output_schema: match schema.0 {
            serde_json::Value::Null => None,
            v => Some(v),
        },
    })
}

/// A workspace that could not even be named, reported at the stage where the
/// saga would have resolved it. Mirrors `never_registered`'s reasoning: the
/// caller learns the id was never valid, and nothing about the filesystem.
fn allocating_worktree_failure(e: WireWorktreeError) -> SpawnError {
    SpawnError::SpawnWorktreeFailed(AgSpawnStage::StageAllocating, e)
}

fn agent_id_to_wire(id: AgentId) -> AgAgentId {
    AgAgentId { raw: id.0 as i64 }
}

fn thread_id_to_wire(t: &BackendThreadId) -> AgBackendThreadId {
    AgBackendThreadId { raw: t.0.clone() }
}

fn stage_to_wire(stage: SpawnStage) -> AgSpawnStage {
    match stage {
        SpawnStage::Allocating => AgSpawnStage::StageAllocating,
        SpawnStage::WorktreeReady => AgSpawnStage::StageWorktreeReady,
        SpawnStage::Bound => AgSpawnStage::StageBound,
        SpawnStage::ThreadAccepted => AgSpawnStage::StageThreadAccepted,
        SpawnStage::Running => AgSpawnStage::StageRunning,
    }
}

fn backend_failure_to_wire(e: AgentBackendError) -> AgBackendFailure {
    match e {
        AgentBackendError::BackendUnavailable { detail } => {
            AgBackendFailure::BackendUnavailable(detail)
        }
        AgentBackendError::ProtocolRejected { detail } => {
            AgBackendFailure::ProtocolRejected(detail)
        }
        AgentBackendError::RunFailed { detail } => AgBackendFailure::RunFailed(detail),
    }
}

/// `PayloadStructured` is NOT a typed success on the far side — decoding it
/// against the caller's result type is the Haskell side's job (`spawnAgent`
/// runs the caller's ordinary `FromJSON` instance over it), and its failure is
/// the Haskell-side `SpawnResultMalformed`. Nothing here ever constructs that
/// variant.
fn payload_to_wire(p: &CycleResultPayload) -> AgCyclePayload {
    match p {
        CycleResultPayload::Structured(v) => AgCyclePayload::PayloadStructured(v.clone()),
        CycleResultPayload::Unstructured(t) => AgCyclePayload::PayloadUnstructured(t.clone()),
        CycleResultPayload::Absent => AgCyclePayload::PayloadAbsent,
    }
}

fn worker_run_to_wire(r: &WorkerRun) -> AgWorkerRun {
    AgWorkerRun {
        run_agent: agent_id_to_wire(r.agent),
        run_worktree: handle_to_wire(&r.worktree),
        run_thread: thread_id_to_wire(&r.thread),
    }
}

/// `receipt_model` carries the backend's EXACT resolved model verbatim — never
/// a tier name, never re-derived here (`ModelPolicy`'s rule: a receipt naming
/// a tier is not checkable).
fn spawn_receipt_to_wire(r: &SpawnReceipt) -> AgSpawnReceipt {
    AgSpawnReceipt {
        receipt_agent: agent_id_to_wire(r.agent),
        receipt_worktree: worktree_id_to_wire(&r.worktree),
        receipt_binding_ref: r.binding_ref.clone(),
        receipt_thread: thread_id_to_wire(&r.thread),
        receipt_model: r.resolved_model.clone(),
        receipt_turn: r.turn.0.clone(),
    }
}

/// `OneCycleRun::activity` has no wire field in lane 1's `SpawnOutcome` — the
/// authored surface gets the run, the payload, and the receipt. Adding it is a
/// `type_defs` change (root's call), not a silent widening here.
fn outcome_to_wire(run: &OneCycleRun) -> AgSpawnOutcome {
    AgSpawnOutcome {
        outcome_run: worker_run_to_wire(&run.run),
        outcome_payload: payload_to_wire(&run.payload),
        outcome_receipt: spawn_receipt_to_wire(&run.receipt),
    }
}

/// Total map from the domain `SpawnError` (`tidepool-agent/src/spawn.rs`) to
/// the wire `SpawnError` (generated by `subagent_effect_def!`'s `errors`
/// block) — `handlers::worktree::error_to_wire`'s precedent, no catch-all arm.
///
/// `SpawnResultMalformed` has NO arm and never will: it is produced by the
/// Haskell-side decoder when a `PayloadStructured` fails to decode against the
/// caller's result type. Rust has no way to know that and must not guess it.
fn spawn_error_to_wire(e: DomainSpawnError) -> SpawnError {
    match e {
        DomainSpawnError::Worktree { stage, error } => {
            SpawnError::SpawnWorktreeFailed(stage_to_wire(stage), worktree_error_to_wire(error))
        }
        DomainSpawnError::Binding { stage, error } => {
            SpawnError::SpawnBindingFailed(stage_to_wire(stage), worktree_error_to_wire(error))
        }
        DomainSpawnError::Backend { stage, error } => {
            SpawnError::SpawnBackendFailed(stage_to_wire(stage), backend_failure_to_wire(error))
        }
        // Both failures are RENDERED (the wire ctor takes two `Text` fields):
        // the original is an arbitrarily nested `SpawnError` and the rollback a
        // `WorktreeError`, and a wire type that recursed into itself to keep
        // them structured would buy case-matchability nobody has asked for.
        // Losing either string is the thing that must not happen.
        DomainSpawnError::RollbackFailed {
            stage,
            original,
            rollback,
        } => SpawnError::SpawnRollbackFailed(
            stage_to_wire(stage),
            original.to_string(),
            rollback.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tidepool_agent::backend::mock::{MockBackend, MockFailure};
    use tidepool_agent::seam::TurnId;
    use tidepool_worktree::create::WorktreeHandle;
    use tidepool_worktree::id::{BranchName, GitOid, WorktreeId};
    use tidepool_worktree::registry::{WorktreeOrigin, WorktreeReceipt, WorktreeRecordStatus};
    use tidepool_worktree::testing::TestRepo;

    use crate::handlers::worktree::never_registered;
    use tidepool_bridge_effects::{WtDirtyPolicy, WtWorktreeId, WtWorktreeSource, WtWorktreeSpec};

    // ------------------------------------------------------------------
    // Fixtures: a REAL temp source repository (git init + a real commit —
    // never a mock of git, tidepool-worktree/CLAUDE.md) with the registry,
    // worktree, and binding roots in a SIBLING temp dir, outside the source
    // working tree (the never-dirty-the-source rule).
    // ------------------------------------------------------------------

    struct Fixture {
        repo: TestRepo,
        roots: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let repo = TestRepo::init().expect("git init the source repository");
            repo.writer()
                .commit_file("README.md", "source\n", "initial commit")
                .expect("seed the source repository with a real commit");
            Self {
                repo,
                roots: tempfile::TempDir::new().expect("create the substrate roots"),
            }
        }

        fn registry_root(&self) -> PathBuf {
            self.roots.path().join("registry")
        }

        fn worktree_root(&self) -> PathBuf {
            self.roots.path().join("worktrees")
        }

        fn binding_root(&self) -> PathBuf {
            self.roots.path().join("bindings")
        }

        fn handler(&self, backend: MockBackend) -> SubagentHandler {
            SubagentHandler::new(
                self.registry_root(),
                self.worktree_root(),
                self.binding_root(),
                self.repo.path().to_path_buf(),
                Box::new(backend),
            )
            .expect("open the subagent handler over the temp substrate")
        }

        /// Persisted binding rows, ignoring `BindingTable`'s `.owner.lock`
        /// (which `open` always creates — its presence is not a binding).
        fn persisted_binding_files(&self) -> Vec<String> {
            let dir = match std::fs::read_dir(self.binding_root()) {
                Ok(d) => d,
                Err(_) => return Vec::new(),
            };
            let mut out: Vec<String> = dir
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".json"))
                .collect();
            out.sort();
            out
        }
    }

    fn new_worktree_spec(label: &str, task: &str) -> AgSpawnSpec {
        AgSpawnSpec {
            spawn_workspace: AgSpawnWorkspace::SpawnNewWorktree(WtWorktreeSpec {
                spec_source: WtWorktreeSource::SourceCurrentRepository,
                spec_label: label.to_string(),
                spec_dirty_policy: WtDirtyPolicy::RequireClean,
            }),
            spawn_agent_label: label.to_string(),
            spawn_task: task.to_string(),
        }
    }

    fn sample_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "required": ["summary"],
        })
    }

    // ==================================================================
    // Named gates that drive the real saga. These call
    // `CoupledSpawner::spawn_one_cycle`, which the `saga` dev implements on
    // a sibling branch; until the wave fold they panic on its `todo!()`,
    // deliberately un-gated so the gap is loud rather than skipped-as-passed.
    // ==================================================================

    #[test]
    fn handler_spawn_happy_path_maps_outcome_to_wire() {
        let fx = Fixture::new();
        let payload = serde_json::json!({ "summary": "did the thing" });
        let mut handler = fx.handler(MockBackend::completing(CycleResultPayload::Structured(
            payload.clone(),
        )));

        let outcome = handler
            .subagent_spawn(
                new_worktree_spec("reviewer", "summarize the diff"),
                JsonArg(sample_schema()),
            )
            .expect("the mock backend completes, so the spawn succeeds");

        assert_eq!(
            outcome.outcome_payload,
            AgCyclePayload::PayloadStructured(payload),
            "a Structured payload round-trips to the wire verbatim"
        );
        // The receipt records the EXACT model the backend resolved — the mock's
        // own constant, never a tier name (ModelPolicy's rule applies to mocks).
        assert_eq!(outcome.outcome_receipt.receipt_model, MockBackend::MODEL);

        // Every other receipt field is populated and agrees with the run.
        let receipt = &outcome.outcome_receipt;
        let run = &outcome.outcome_run;
        assert_eq!(receipt.receipt_agent, run.run_agent);
        assert_eq!(receipt.receipt_thread, run.run_thread);
        assert_eq!(
            receipt.receipt_worktree, run.run_worktree.handle_receipt.tree_id,
            "the receipt names the worktree the run actually got"
        );
        assert!(
            !receipt.receipt_binding_ref.is_empty(),
            "the binding ref is the string the binding was taken under"
        );
        assert!(
            !receipt.receipt_turn.is_empty(),
            "the turn id is the backend's own, echoed"
        );
        assert!(
            !run.run_worktree.handle_receipt.cwd.is_empty(),
            "the worktree handle carries the cwd the cycle ran in"
        );
    }

    #[test]
    fn handler_maps_thread_start_failure_to_typed_wire_error() {
        let fx = Fixture::new();
        let mut handler = fx.handler(MockBackend::failing(MockFailure::AtThreadStart(
            AgentBackendError::BackendUnavailable {
                detail: "codex app-server not running".to_string(),
            },
        )));

        let err = handler
            .subagent_spawn(
                new_worktree_spec("reviewer", "summarize the diff"),
                JsonArg(sample_schema()),
            )
            .expect_err("start_thread fails, so the spawn fails");

        assert_eq!(
            err,
            SpawnError::SpawnBackendFailed(
                AgSpawnStage::StageThreadAccepted,
                AgBackendFailure::BackendUnavailable("codex app-server not running".to_string()),
            ),
            "a rejected thread is a typed wire failure naming the stage it died at"
        );
    }

    // ==================================================================
    // Named gates over this handler's own mapping logic. These never reach
    // the saga.
    // ==================================================================

    /// The trust boundary: an id that could act as a path is refused BEFORE
    /// the registry or the binding table (both of which join ids into file
    /// paths) ever sees it, so nothing is allocated and nothing is bound.
    #[test]
    fn handler_rejects_path_unsafe_existing_id_before_touching_disk() {
        let fx = Fixture::new();
        let mut handler = fx.handler(MockBackend::completing(CycleResultPayload::Absent));

        for evil in ["../escape", "../../../etc/passwd", "a/b", "..", ""] {
            let wire_id = WtWorktreeId {
                raw: evil.to_string(),
            };
            let err = handler
                .subagent_spawn(
                    AgSpawnSpec {
                        spawn_workspace: AgSpawnWorkspace::SpawnExistingWorktree(wire_id.clone()),
                        spawn_agent_label: "intruder".to_string(),
                        spawn_task: "escape".to_string(),
                    },
                    JsonArg(serde_json::Value::Null),
                )
                .expect_err("a path-unsafe id must never resolve to a workspace");

            assert_eq!(
                err,
                SpawnError::SpawnWorktreeFailed(
                    AgSpawnStage::StageAllocating,
                    never_registered(&wire_id),
                ),
                "{evil:?} is refused at Allocating as never-registered"
            );
        }

        assert!(
            fx.persisted_binding_files().is_empty(),
            "no binding was persisted: {:?}",
            fx.persisted_binding_files()
        );
        assert!(
            handler
                .spawner()
                .manager()
                .list()
                .expect("list the registry")
                .is_empty(),
            "no worktree was registered"
        );
    }

    /// `Value::Null` is how "no schema" arrives — the Haskell `Value` argument
    /// is total, so absence cannot be a missing argument.
    #[test]
    fn handler_null_schema_becomes_none() {
        let request = request_from_wire(
            new_worktree_spec("reviewer", "look around"),
            JsonArg(serde_json::Value::Null),
        )
        .expect("a new-worktree spec converts");
        assert_eq!(request.output_schema, None);

        let schema = sample_schema();
        let with_schema = request_from_wire(
            new_worktree_spec("reviewer", "look around"),
            JsonArg(schema.clone()),
        )
        .expect("a new-worktree spec converts");
        assert_eq!(with_schema.output_schema, Some(schema));
    }

    #[test]
    fn handler_request_from_wire_carries_label_task_and_workspace() {
        let request = request_from_wire(
            new_worktree_spec("reviewer", "summarize the diff"),
            JsonArg(serde_json::Value::Null),
        )
        .expect("a new-worktree spec converts");

        assert_eq!(request.agent_label, "reviewer");
        assert_eq!(request.task, "summarize the diff");
        match request.workspace {
            SpawnWorkspace::New(spec) => {
                assert_eq!(spec.label, "reviewer");
                assert_eq!(
                    spec.source,
                    tidepool_worktree::WorktreeSource::CurrentRepository
                );
                assert_eq!(
                    spec.dirty_policy,
                    tidepool_worktree::DirtyPolicy::RequireClean
                );
            }
            other => panic!("expected a New workspace, got {other:?}"),
        }
    }

    #[test]
    fn handler_request_from_wire_accepts_a_path_safe_existing_id() {
        let request = request_from_wire(
            AgSpawnSpec {
                spawn_workspace: AgSpawnWorkspace::SpawnExistingWorktree(WtWorktreeId {
                    raw: "wt-19c8-2a4d-0-deadbeef".to_string(),
                }),
                spawn_agent_label: "reviewer".to_string(),
                spawn_task: "continue".to_string(),
            },
            JsonArg(serde_json::Value::Null),
        )
        .expect("a minted id converts");

        match request.workspace {
            SpawnWorkspace::Existing(id) => {
                assert_eq!(id, WorktreeId::from_raw("wt-19c8-2a4d-0-deadbeef"))
            }
            other => panic!("expected an Existing workspace, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // Domain -> wire, exercised directly on hand-built domain values so the
    // mapping is covered independently of the saga.
    // ------------------------------------------------------------------

    fn sample_worktree_handle() -> WorktreeHandle {
        WorktreeHandle::from_receipt(WorktreeReceipt {
            worktree_id: WorktreeId::from_raw("wt-1"),
            cwd: PathBuf::from("/worktrees/wt-1"),
            branch: BranchName::from_raw("tidepool/worktree/wt-1"),
            source_head: GitOid::from_raw("deadbeef"),
            snapshot_ref: None,
            origin: WorktreeOrigin::CurrentRepository,
            source_repository: PathBuf::from("/repo"),
            created_at_ms: 1_700_000_000_000,
            status: WorktreeRecordStatus::Finalized,
        })
    }

    fn sample_run(payload: CycleResultPayload) -> OneCycleRun {
        let handle = sample_worktree_handle();
        OneCycleRun {
            run: WorkerRun {
                agent: AgentId(7),
                worktree: handle,
                thread: BackendThreadId("mock-thread-0".to_string()),
            },
            payload,
            receipt: SpawnReceipt {
                agent: AgentId(7),
                worktree: WorktreeId::from_raw("wt-1"),
                binding_ref: "agent-7-reviewer".to_string(),
                thread: BackendThreadId("mock-thread-0".to_string()),
                resolved_model: "gpt-5.4-mini".to_string(),
                turn: TurnId("turn-1".to_string()),
            },
            activity: Vec::new(),
        }
    }

    #[test]
    fn handler_outcome_to_wire_carries_run_payload_and_receipt() {
        let run = sample_run(CycleResultPayload::Structured(
            serde_json::json!({ "summary": "done" }),
        ));
        let wire = outcome_to_wire(&run);

        assert_eq!(
            wire.outcome_run,
            AgWorkerRun {
                run_agent: AgAgentId { raw: 7 },
                run_worktree: handle_to_wire(&sample_worktree_handle()),
                run_thread: AgBackendThreadId {
                    raw: "mock-thread-0".to_string()
                },
            }
        );
        assert_eq!(
            wire.outcome_payload,
            AgCyclePayload::PayloadStructured(serde_json::json!({ "summary": "done" }))
        );
        assert_eq!(
            wire.outcome_receipt,
            AgSpawnReceipt {
                receipt_agent: AgAgentId { raw: 7 },
                receipt_worktree: WtWorktreeId {
                    raw: "wt-1".to_string()
                },
                receipt_binding_ref: "agent-7-reviewer".to_string(),
                receipt_thread: AgBackendThreadId {
                    raw: "mock-thread-0".to_string()
                },
                // Verbatim: the receipt records the model that ran, never the tier.
                receipt_model: "gpt-5.4-mini".to_string(),
                receipt_turn: "turn-1".to_string(),
            }
        );
    }

    #[test]
    fn handler_payload_to_wire_covers_every_variant() {
        assert_eq!(
            payload_to_wire(&CycleResultPayload::Structured(serde_json::json!([1, 2]))),
            AgCyclePayload::PayloadStructured(serde_json::json!([1, 2]))
        );
        assert_eq!(
            payload_to_wire(&CycleResultPayload::Unstructured("plain prose".to_string())),
            AgCyclePayload::PayloadUnstructured("plain prose".to_string())
        );
        assert_eq!(
            payload_to_wire(&CycleResultPayload::Absent),
            AgCyclePayload::PayloadAbsent
        );
    }

    #[test]
    fn handler_stage_to_wire_covers_every_stage() {
        for (domain, wire) in [
            (SpawnStage::Allocating, AgSpawnStage::StageAllocating),
            (SpawnStage::WorktreeReady, AgSpawnStage::StageWorktreeReady),
            (SpawnStage::Bound, AgSpawnStage::StageBound),
            (
                SpawnStage::ThreadAccepted,
                AgSpawnStage::StageThreadAccepted,
            ),
            (SpawnStage::Running, AgSpawnStage::StageRunning),
        ] {
            assert_eq!(stage_to_wire(domain), wire);
        }
    }

    #[test]
    fn handler_backend_failure_to_wire_covers_every_variant() {
        assert_eq!(
            backend_failure_to_wire(AgentBackendError::BackendUnavailable {
                detail: "gone".to_string()
            }),
            AgBackendFailure::BackendUnavailable("gone".to_string())
        );
        assert_eq!(
            backend_failure_to_wire(AgentBackendError::ProtocolRejected {
                detail: "bad field".to_string()
            }),
            AgBackendFailure::ProtocolRejected("bad field".to_string())
        );
        assert_eq!(
            backend_failure_to_wire(AgentBackendError::RunFailed {
                detail: "rate limited".to_string()
            }),
            AgBackendFailure::RunFailed("rate limited".to_string())
        );
    }

    #[test]
    fn handler_spawn_error_to_wire_maps_worktree_and_binding_failures() {
        let dirty = DomainWorktreeError::WorktreeNotRegistered(WorktreeId::from_raw("wt-ghost"));
        assert_eq!(
            spawn_error_to_wire(DomainSpawnError::Worktree {
                stage: SpawnStage::Allocating,
                error: dirty.clone(),
            }),
            SpawnError::SpawnWorktreeFailed(
                AgSpawnStage::StageAllocating,
                worktree_error_to_wire(dirty.clone()),
            )
        );

        let busy = DomainWorktreeError::WorktreeBusy {
            worktree: WorktreeId::from_raw("wt-1"),
            holder: "agent-3-reviewer".to_string(),
        };
        assert_eq!(
            spawn_error_to_wire(DomainSpawnError::Binding {
                stage: SpawnStage::Bound,
                error: busy.clone(),
            }),
            SpawnError::SpawnBindingFailed(AgSpawnStage::StageBound, worktree_error_to_wire(busy)),
            "a refused binding stays a BINDING failure — never folded onto the worktree arm"
        );
    }

    #[test]
    fn handler_spawn_error_to_wire_maps_backend_failure_with_its_stage() {
        assert_eq!(
            spawn_error_to_wire(DomainSpawnError::Backend {
                stage: SpawnStage::Running,
                error: AgentBackendError::RunFailed {
                    detail: "model error".to_string()
                },
            }),
            SpawnError::SpawnBackendFailed(
                AgSpawnStage::StageRunning,
                AgBackendFailure::RunFailed("model error".to_string()),
            )
        );
    }

    /// Both failures survive the rendering — losing either is the thing the
    /// domain variant exists to prevent.
    #[test]
    fn handler_spawn_error_to_wire_renders_both_sides_of_a_rollback_failure() {
        let original = DomainSpawnError::Backend {
            stage: SpawnStage::Running,
            error: AgentBackendError::RunFailed {
                detail: "model error".to_string(),
            },
        };
        let rollback = DomainWorktreeError::StorageFailure {
            path: PathBuf::from("/bindings/wt-1.json"),
            detail: "No space left on device".to_string(),
        };
        let wire = spawn_error_to_wire(DomainSpawnError::RollbackFailed {
            stage: SpawnStage::Running,
            original: Box::new(original.clone()),
            rollback: rollback.clone(),
        });

        match wire {
            SpawnError::SpawnRollbackFailed(stage, orig_text, rb_text) => {
                assert_eq!(stage, AgSpawnStage::StageRunning);
                assert_eq!(orig_text, original.to_string());
                assert!(
                    orig_text.contains("model error"),
                    "the original failure survives: {orig_text}"
                );
                assert_eq!(rb_text, rollback.to_string());
                assert!(
                    rb_text.contains("No space left on device"),
                    "the rollback failure survives: {rb_text}"
                );
            }
            other => panic!("expected SpawnRollbackFailed, got {other:?}"),
        }
    }
}
