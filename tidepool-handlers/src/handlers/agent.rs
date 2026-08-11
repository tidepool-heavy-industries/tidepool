use std::path::PathBuf;

use tidepool_agent::backend::AgentBackend;
use tidepool_agent::seam::{
    AgentActivity, AgentBackendError, AgentId, BackendThreadId, CycleResultPayload,
    DynamicToolDeclaration, ModelPolicy, ReasoningEffort, TokenUsage, ToolCallId, ToolOutcome,
};
use tidepool_agent::spawn::{
    CoupledSpawner, OneCycleRun, SpawnError as DomainSpawnError, SpawnReceipt, SpawnRequest,
    SpawnStage, SpawnStep, SpawnWorkspace, WorkerRun,
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
    AgAgentActivity, AgAgentId, AgAgentStep, AgBackendFailure, AgBackendThreadId, AgCyclePayload,
    AgSpawnOutcome, AgSpawnReceipt, AgSpawnSpec, AgSpawnStage, AgSpawnWorkspace, AgTokenUsage,
    AgWorkerRun,
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

/// Serves `SubagentSpawn` (the whole saga behind one verb) plus
/// `SubagentBegin`/`SubagentResume` (the same saga driven one stop at a time,
/// for an agent that holds dynamic tools).
///
/// Owns the worktree substrate handles (via [`CoupledSpawner`] — whose
/// `BindingTable` holds the single-owner lifetime flock for its binding root)
/// and an [`AgentBackend`]. Production wires the codex adapter; every
/// committed test wires [`tidepool_agent::backend::mock::MockBackend`] — no
/// live-model turns in tests, ever (standing rule, Inanna 2026-08-09).
///
/// **A parked turn lives exactly as long as this handler does.** Between a
/// `StepToolCall` and its `SubagentResume` the child's request is parked with
/// no response written, so if the eval driving the loop dies mid-dispatch the
/// call is never answered and the child's turn hangs until its own timeout.
/// The mitigation is ownership, not a protocol trick: this handler owns the
/// backend, and dropping it takes the app-server process with it.
///
/// Not `Clone`, deliberately (RepoEventHandler precedent): it owns a boxed
/// backend and a flocked binding table, neither of which has a meaningful
/// second owner.
pub struct SubagentHandler {
    spawner: CoupledSpawner,
    backend: Box<dyn AgentBackend + Send>,
    /// The model tier and effort every agent this handler spawns runs at.
    ///
    /// Handler configuration rather than an authored-surface field: a model
    /// budget is granted to an OPERATOR, and the operator is who wires the
    /// handler. An authored `spawnAgent` call choosing its own tier would let
    /// any eval spend at any price — PRD 18 open decision 3 is where a
    /// semantic tier vocabulary on the authored surface gets decided, and it
    /// is still open.
    model: ModelPolicy,
    effort: ReasoningEffort,
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
        backend: Box<dyn AgentBackend + Send>,
    ) -> Result<Self, DomainWorktreeError> {
        let registry = WorktreeRegistry::open(&registry_root)?;
        let manager =
            WorktreeManager::new(GitCli::new(), registry, worktree_root, source_repository);
        Ok(Self {
            spawner: CoupledSpawner::open(manager, binding_root)?,
            backend,
            model: ModelPolicy::CheapPlumbing,
            effort: ReasoningEffort::Low,
        })
    }

    /// Run every agent this handler spawns at `model`/`effort`.
    ///
    /// The live acceptance is the caller that needs this: its granted budget
    /// names `gpt-5.6-luna` at low effort specifically.
    #[must_use]
    pub fn with_model_policy(mut self, model: ModelPolicy, effort: ReasoningEffort) -> Self {
        self.model = model;
        self.effort = effort;
        self
    }

    /// The spawner, for post-run assertions in tests (binding state, registry
    /// lookups) without reopening flocked state.
    pub fn spawner(&self) -> &CoupledSpawner {
        &self.spawner
    }

    /// The backend's own transcript, as opaque JSONL lines — empty for a
    /// backend that keeps none (every mock). The live acceptance writes this to
    /// a fixture so the recording can drive the production pump in CI
    /// afterwards.
    pub fn backend_transcript_jsonl(&self) -> Vec<String> {
        self.backend.transcript_jsonl()
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
        let request = request_from_wire(spec, schema, Vec::new(), self.model, self.effort)?;
        let run = self
            .spawner
            .spawn_one_cycle(&mut *self.backend, &request)
            .map_err(spawn_error_to_wire)?;
        Ok(outcome_to_wire(&run))
    }

    /// Serves `SubagentBegin`: the same saga as `subagent_spawn`, stopped at
    /// its first stop instead of driven to the end, for an agent that holds
    /// dynamic tools.
    ///
    /// `tools` arrives as a flat `Value` rather than inside `spec` because
    /// `serde_json::Value` has no `FromCore` — see `AgAgentStep`'s docs for the
    /// asymmetry that forces. Parsing it is the FIRST thing that happens: a
    /// malformed declaration fails at `StageAllocating`, where nothing has been
    /// allocated, bound, or spawned.
    fn subagent_begin(
        &mut self,
        spec: AgSpawnSpec,
        tools: JsonArg,
        schema: JsonArg,
    ) -> Result<AgAgentStep, SpawnError> {
        let tools = tool_declarations_from_wire(&tools.0)?;
        let request = request_from_wire(spec, schema, tools, self.model, self.effort)?;
        let step = self
            .spawner
            .begin(&mut *self.backend, &request)
            .map_err(spawn_error_to_wire)?;
        Ok(step_to_wire(&step))
    }

    /// Serves `SubagentResume`: answer the parked tool call and drive on.
    ///
    /// `ok` false is a REFUSAL, not a transport failure — the child reads the
    /// text and reacts to it, so the call is always answered. Which agent and
    /// which call are checked by [`CoupledSpawner::answer`] before anything
    /// reaches the backend; a mismatch is `SpawnDriveFailed`, because the
    /// backend did nothing wrong.
    fn subagent_resume(
        &mut self,
        agent: AgAgentId,
        call: String,
        ok: bool,
        body: JsonArg,
    ) -> Result<AgAgentStep, SpawnError> {
        let agent = agent_id_from_wire(agent)?;
        let outcome = if ok {
            ToolOutcome::Answered(body.0)
        } else {
            ToolOutcome::Refused(refusal_text(body.0))
        };
        let step = self
            .spawner
            .answer(&mut *self.backend, agent, ToolCallId(call), outcome)
            .map_err(spawn_error_to_wire)?;
        Ok(step_to_wire(&step))
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
fn request_from_wire(
    spec: AgSpawnSpec,
    schema: JsonArg,
    tools: Vec<tidepool_agent::seam::DynamicToolDeclaration>,
    model: ModelPolicy,
    effort: ReasoningEffort,
) -> Result<SpawnRequest, SpawnError> {
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
        tools,
        model,
        effort,
    })
}

/// A workspace that could not even be named, reported at the stage where the
/// saga would have resolved it. Mirrors `never_registered`'s reasoning: the
/// caller learns the id was never valid, and nothing about the filesystem.
fn allocating_worktree_failure(e: WireWorktreeError) -> SpawnError {
    SpawnError::SpawnWorktreeFailed(AgSpawnStage::StageAllocating, e)
}

/// Parse the flat `tools` argument into declarations.
///
/// Reported at `StageAllocating` because that is the truth: nothing has been
/// allocated while the declarations are still being READ. It is a
/// `SpawnDriveFailed` rather than a backend or worktree failure because a
/// malformed declaration is the driver handing Rust something it cannot mean —
/// neither the backend nor the filesystem has been touched.
///
/// Field spelling is `inputSchema`, matching the JSON Schema vocabulary every
/// backend speaks and the array `compileTools` builds. Strict about shape: a
/// non-array is refused rather than read as "no tools", so a caller that
/// mis-built the argument learns it instead of silently spawning a toolless
/// agent that then refuses every call it makes.
fn tool_declarations_from_wire(
    tools: &serde_json::Value,
) -> Result<Vec<DynamicToolDeclaration>, SpawnError> {
    let drive_failure = |detail: String| {
        SpawnError::SpawnDriveFailed(AgSpawnStage::StageAllocating, format!("tools: {detail}"))
    };
    let serde_json::Value::Array(items) = tools else {
        return Err(drive_failure(format!(
            "expected a JSON array of tool declarations, got {}",
            json_type_name(tools)
        )));
    };
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let field = |name: &str| {
                item.get(name)
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| drive_failure(format!("declaration {i} has no {name} string")))
            };
            Ok(DynamicToolDeclaration {
                name: field("name")?.to_string(),
                description: field("description")?.to_string(),
                input_schema: item
                    .get("inputSchema")
                    .cloned()
                    .ok_or_else(|| drive_failure(format!("declaration {i} has no inputSchema")))?,
            })
        })
        .collect()
}

/// The JSON kind of a value, for a diagnostic that says what arrived instead of
/// only what was wanted.
fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// What a refusal's `body` says to the CHILD.
///
/// A refusal authored as a plain string is passed through verbatim — that is
/// the shape the Haskell loop writes, and quoting it would put JSON escapes in
/// front of a model. Anything else is rendered as JSON rather than dropped:
/// the child is better served by a structured refusal it can read than by a
/// handler deciding its text was the wrong shape.
fn refusal_text(body: serde_json::Value) -> String {
    match body {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    }
}

/// A wire `AgentId` back into the domain. Ids are minted from 0 upward, so a
/// negative one was never minted — reported as a drive failure (the caller sent
/// an id nothing could be running under) rather than wrapped into a huge `u64`
/// that would fail later as a confusing "no such agent".
fn agent_id_from_wire(id: AgAgentId) -> Result<AgentId, SpawnError> {
    u64::try_from(id.raw).map(AgentId).map_err(|_| {
        SpawnError::SpawnDriveFailed(
            AgSpawnStage::StageRunning,
            format!("agent id {} was never minted", id.raw),
        )
    })
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

/// Total, no catch-all arm — `handlers::worktree::error_to_wire`'s precedent, so
/// a new activity kind is a compile error here rather than a silently dropped
/// observation.
fn activity_to_wire(a: &AgentActivity) -> AgAgentActivity {
    match a {
        AgentActivity::Command { command, exit_code } => {
            AgAgentActivity::ActivityCommand(command.clone(), exit_code.map(i64::from))
        }
        AgentActivity::FileChanged { path } => AgAgentActivity::ActivityFileChanged(path.clone()),
    }
}

/// Every counter crosses. Widening the seam's `TokenUsage` without widening
/// this is a compile error, which is the point of writing it out field by
/// field instead of deriving it.
fn usage_to_wire(u: &TokenUsage) -> AgTokenUsage {
    AgTokenUsage {
        usage_input: u.input_tokens,
        usage_cached_input: u.cached_input_tokens,
        usage_output: u.output_tokens,
        usage_reasoning_output: u.reasoning_output_tokens,
        usage_total: u.total_tokens,
    }
}

/// `receipt_model` carries the backend's EXACT resolved model verbatim — never
/// a tier name, never re-derived here (`ModelPolicy`'s rule: a receipt naming
/// a tier is not checkable). `receipt_usage` is `None` when the backend
/// reported no usage, which is not the same fact as zero.
fn spawn_receipt_to_wire(r: &SpawnReceipt) -> AgSpawnReceipt {
    AgSpawnReceipt {
        receipt_agent: agent_id_to_wire(r.agent),
        receipt_worktree: worktree_id_to_wire(&r.worktree),
        receipt_binding_ref: r.binding_ref.clone(),
        receipt_thread: thread_id_to_wire(&r.thread),
        receipt_model: r.resolved_model.clone(),
        receipt_turn: r.turn.0.clone(),
        receipt_rounds: i64::from(r.rounds),
        receipt_usage: r.usage.as_ref().map(usage_to_wire),
    }
}

fn outcome_to_wire(run: &OneCycleRun) -> AgSpawnOutcome {
    AgSpawnOutcome {
        outcome_run: worker_run_to_wire(&run.run),
        outcome_payload: payload_to_wire(&run.payload),
        outcome_receipt: spawn_receipt_to_wire(&run.receipt),
        outcome_activity: run.activity.iter().map(activity_to_wire).collect(),
    }
}

/// Where the driven saga stopped, projected TOTALLY — a parked call carries its
/// correlation fields verbatim (the caller echoes them straight back to
/// `SubagentResume`, so re-deriving any of them here would be the misroute the
/// triple exists to catch).
fn step_to_wire(step: &SpawnStep) -> AgAgentStep {
    match step {
        SpawnStep::ToolCall { agent, call } => AgAgentStep::StepToolCall(
            agent_id_to_wire(*agent),
            call.call.0.clone(),
            call.tool.clone(),
            call.arguments.clone(),
        ),
        SpawnStep::Done(run) => AgAgentStep::StepDone(Box::new(outcome_to_wire(run))),
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
    let stage = e.stage();
    match e {
        DomainSpawnError::Worktree { error, .. } => {
            SpawnError::SpawnWorktreeFailed(stage_to_wire(stage), worktree_error_to_wire(error))
        }
        DomainSpawnError::Binding { error, .. } => {
            SpawnError::SpawnBindingFailed(stage_to_wire(stage), worktree_error_to_wire(error))
        }
        DomainSpawnError::Backend { error, .. } => {
            SpawnError::SpawnBackendFailed(stage_to_wire(stage), backend_failure_to_wire(error))
        }
        // Both failures are RENDERED (the wire ctor takes two `Text` fields):
        // the original is an arbitrarily nested `SpawnError` and the rollback a
        // `WorktreeError`, and a wire type that recursed into itself to keep
        // them structured would buy case-matchability nobody has asked for.
        // Losing either string is the thing that must not happen.
        DomainSpawnError::RollbackFailed {
            original, rollback, ..
        } => SpawnError::SpawnRollbackFailed(
            stage_to_wire(stage),
            original.to_string(),
            rollback.to_string(),
        ),
        // Both are the DRIVER sequencing the loop wrongly, or the runtime's
        // backstop catching a loop that never stopped. Neither folds onto the
        // `Backend` arm: the backend did exactly what it was asked, and telling
        // a caller their backend failed would point them at the wrong system.
        DomainSpawnError::NotRunning { agent, detail } => SpawnError::SpawnDriveFailed(
            stage_to_wire(stage),
            format!("agent {}: {detail}", agent.0),
        ),
        DomainSpawnError::RoundBackstop { agent, limit } => SpawnError::SpawnDriveFailed(
            stage_to_wire(stage),
            format!(
                "agent {} exceeded the runtime tool-round backstop of {limit}",
                agent.0
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tidepool_agent::backend::mock::{MockBackend, MockFailure, MockStep};
    use tidepool_agent::seam::{ToolCall, TurnId};
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

    /// `request_from_wire` at this handler's defaults — the conversion tests
    /// are about the SPEC lane, not about policy plumbing.
    fn plain_request_from_wire(
        spec: AgSpawnSpec,
        schema: JsonArg,
    ) -> Result<SpawnRequest, SpawnError> {
        request_from_wire(
            spec,
            schema,
            Vec::new(),
            ModelPolicy::CheapPlumbing,
            ReasoningEffort::Low,
        )
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
        let request = plain_request_from_wire(
            new_worktree_spec("reviewer", "look around"),
            JsonArg(serde_json::Value::Null),
        )
        .expect("a new-worktree spec converts");
        assert_eq!(request.output_schema, None);

        let schema = sample_schema();
        let with_schema = plain_request_from_wire(
            new_worktree_spec("reviewer", "look around"),
            JsonArg(schema.clone()),
        )
        .expect("a new-worktree spec converts");
        assert_eq!(with_schema.output_schema, Some(schema));
    }

    #[test]
    fn handler_request_from_wire_carries_label_task_and_workspace() {
        let request = plain_request_from_wire(
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
        let request = plain_request_from_wire(
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
                rounds: 0,
                usage: None,
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
                receipt_rounds: 0,
                receipt_usage: None,
            }
        );
        assert!(
            wire.outcome_activity.is_empty(),
            "the sample run reported no activity"
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

    // ==================================================================
    // The tool-dispatch verbs: `SubagentBegin` / `SubagentResume`.
    // ==================================================================

    #[test]
    fn handler_activity_to_wire_covers_every_variant() {
        assert_eq!(
            activity_to_wire(&AgentActivity::Command {
                command: "cargo test".to_string(),
                exit_code: Some(101),
            }),
            AgAgentActivity::ActivityCommand("cargo test".to_string(), Some(101))
        );
        // A command with no reported exit code stays `Nothing` — "the backend
        // said nothing", which is a different fact from "it succeeded".
        assert_eq!(
            activity_to_wire(&AgentActivity::Command {
                command: "sleep 1".to_string(),
                exit_code: None,
            }),
            AgAgentActivity::ActivityCommand("sleep 1".to_string(), None)
        );
        assert_eq!(
            activity_to_wire(&AgentActivity::FileChanged {
                path: "src/lib.rs".to_string(),
            }),
            AgAgentActivity::ActivityFileChanged("src/lib.rs".to_string())
        );
    }

    #[test]
    fn handler_usage_to_wire_carries_every_counter() {
        assert_eq!(
            usage_to_wire(&TokenUsage {
                input_tokens: 11,
                cached_input_tokens: 22,
                output_tokens: 33,
                reasoning_output_tokens: 44,
                total_tokens: 55,
            }),
            AgTokenUsage {
                usage_input: 11,
                usage_cached_input: 22,
                usage_output: 33,
                usage_reasoning_output: 44,
                usage_total: 55,
            },
            "each counter lands on its own field — a transposition here would \
             misreport a budget"
        );
    }

    /// The correlation fields the caller echoes back to `SubagentResume` cross
    /// VERBATIM. Re-deriving any of them would be the misroute the triple
    /// exists to catch.
    #[test]
    fn handler_step_to_wire_carries_the_parked_call_verbatim() {
        let arguments = serde_json::json!({ "question": "which file?", "n": 3 });
        let step = SpawnStep::ToolCall {
            agent: AgentId(7),
            call: ToolCall {
                call: ToolCallId("call-abc".to_string()),
                thread: BackendThreadId("mock-thread-0".to_string()),
                turn: TurnId("turn-1".to_string()),
                tool: "ask_parent".to_string(),
                arguments: arguments.clone(),
            },
        };

        assert_eq!(
            step_to_wire(&step),
            AgAgentStep::StepToolCall(
                AgAgentId { raw: 7 },
                "call-abc".to_string(),
                "ask_parent".to_string(),
                arguments,
            )
        );
    }

    #[test]
    fn handler_step_to_wire_done_carries_activity_and_usage() {
        let mut run = sample_run(CycleResultPayload::Absent);
        run.activity = vec![
            AgentActivity::Command {
                command: "git status".to_string(),
                exit_code: Some(0),
            },
            AgentActivity::FileChanged {
                path: "notes.md".to_string(),
            },
        ];
        run.receipt.rounds = 2;
        run.receipt.usage = Some(TokenUsage {
            input_tokens: 1,
            cached_input_tokens: 2,
            output_tokens: 3,
            reasoning_output_tokens: 4,
            total_tokens: 10,
        });

        let AgAgentStep::StepDone(outcome) = step_to_wire(&SpawnStep::Done(Box::new(run))) else {
            panic!("a finished cycle is StepDone");
        };
        assert_eq!(
            outcome.outcome_activity,
            vec![
                AgAgentActivity::ActivityCommand("git status".to_string(), Some(0)),
                AgAgentActivity::ActivityFileChanged("notes.md".to_string()),
            ],
            "activity reaches the wire in the order the backend reported it"
        );
        assert_eq!(outcome.outcome_receipt.receipt_rounds, 2);
        assert_eq!(
            outcome.outcome_receipt.receipt_usage,
            Some(AgTokenUsage {
                usage_input: 1,
                usage_cached_input: 2,
                usage_output: 3,
                usage_reasoning_output: 4,
                usage_total: 10,
            }),
            "usage lands on the RECEIPT — it is a checkable fact about the run"
        );
    }

    fn one_declaration() -> serde_json::Value {
        serde_json::json!([{
            "name": "ask_parent",
            "description": "ask the parent a question",
            "inputSchema": { "type": "object", "properties": { "q": { "type": "string" } } },
        }])
    }

    #[test]
    fn handler_tool_declarations_from_wire_reads_a_well_formed_array() {
        let decls = tool_declarations_from_wire(&one_declaration()).expect("a well-formed array");
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].name, "ask_parent");
        assert_eq!(decls[0].description, "ask the parent a question");
        assert_eq!(
            decls[0].input_schema,
            serde_json::json!({ "type": "object", "properties": { "q": { "type": "string" } } }),
            "the schema crosses verbatim — nothing here rewrites it"
        );
        // Zero tools is an empty array, not an error.
        assert!(tool_declarations_from_wire(&serde_json::json!([]))
            .expect("an empty array is zero tools")
            .is_empty());
    }

    /// A malformed declaration fails at `StageAllocating` — and the claim that
    /// stage makes is checked, not just asserted: no worktree is registered and
    /// no binding row exists.
    #[test]
    fn handler_begin_refuses_a_malformed_tools_array_at_allocating() {
        let fx = Fixture::new();
        let mut handler = fx.handler(MockBackend::completing(CycleResultPayload::Absent));

        for (tools, expected) in [
            (
                serde_json::json!([{ "name": "ask_parent" }]),
                "tools: declaration 0 has no description string",
            ),
            (
                serde_json::json!([{ "name": "ask_parent", "description": "d" }]),
                "tools: declaration 0 has no inputSchema",
            ),
            (
                serde_json::json!([{ "description": "d", "inputSchema": {} }]),
                "tools: declaration 0 has no name string",
            ),
            (
                serde_json::json!({ "ask_parent": {} }),
                "tools: expected a JSON array of tool declarations, got an object",
            ),
            (
                serde_json::Value::Null,
                "tools: expected a JSON array of tool declarations, got null",
            ),
        ] {
            let err = handler
                .subagent_begin(
                    new_worktree_spec("reviewer", "summarize the diff"),
                    JsonArg(tools.clone()),
                    JsonArg(sample_schema()),
                )
                .expect_err("a malformed tools array cannot begin a spawn");

            assert_eq!(
                err,
                SpawnError::SpawnDriveFailed(AgSpawnStage::StageAllocating, expected.to_string()),
                "{tools} is refused at Allocating, naming what was wrong"
            );
        }

        assert!(
            fx.persisted_binding_files().is_empty(),
            "nothing is allocated while the declarations are still being read: {:?}",
            fx.persisted_binding_files()
        );
        assert!(
            handler
                .spawner()
                .manager()
                .list()
                .expect("list the registry")
                .is_empty(),
            "no worktree was created for a spawn that never began"
        );
    }

    /// Answering when nothing is running is the DRIVER's sequencing failure.
    /// `SpawnBackendFailed` would point an operator at the wrong system — the
    /// backend was never asked anything.
    #[test]
    fn handler_resume_with_no_agent_running_is_a_drive_failure() {
        let fx = Fixture::new();
        let mut handler = fx.handler(MockBackend::completing(CycleResultPayload::Absent));

        let err = handler
            .subagent_resume(
                AgAgentId { raw: 0 },
                "call-abc".to_string(),
                true,
                JsonArg(serde_json::json!({ "answer": "42" })),
            )
            .expect_err("nothing is parked, so nothing can be answered");

        assert_eq!(
            err,
            SpawnError::SpawnDriveFailed(
                AgSpawnStage::StageRunning,
                "agent 0: no agent is mid-turn".to_string(),
            )
        );
        assert!(
            !matches!(err, SpawnError::SpawnBackendFailed(..)),
            "the backend did nothing wrong: {err:?}"
        );
    }

    /// An id below the mint's floor was never handed out. Refusing it here
    /// keeps the failure legible instead of wrapping to a huge `u64` that
    /// surfaces later as a confusing "no such agent".
    #[test]
    fn handler_agent_id_from_wire_refuses_a_never_minted_id() {
        assert_eq!(agent_id_from_wire(AgAgentId { raw: 3 }), Ok(AgentId(3)));
        assert_eq!(
            agent_id_from_wire(AgAgentId { raw: -1 }),
            Err(SpawnError::SpawnDriveFailed(
                AgSpawnStage::StageRunning,
                "agent id -1 was never minted".to_string(),
            ))
        );
    }

    /// A refusal is written FOR THE CHILD: a plain string crosses verbatim
    /// rather than as a quoted JSON literal.
    #[test]
    fn handler_refusal_text_is_written_for_the_child() {
        assert_eq!(
            refusal_text(serde_json::json!("no such tool: frobnicate")),
            "no such tool: frobnicate"
        );
        assert_eq!(
            refusal_text(serde_json::json!({ "reason": "cap reached" })),
            "{\"reason\":\"cap reached\"}",
            "a structured refusal is rendered, never dropped"
        );
    }

    /// The whole verb pair on the real saga: begin parks on the child's call,
    /// resume answers it, and the turn finishes.
    #[test]
    fn handler_begin_parks_and_resume_drives_the_turn_to_done() {
        let fx = Fixture::new();
        let payload = serde_json::json!({ "summary": "asked and answered" });
        let mut handler = fx.handler(
            MockBackend::scripted([
                MockStep::Calls {
                    tool: "ask_parent".to_string(),
                    arguments: serde_json::json!({ "q": "which file?" }),
                },
                MockStep::Completes(CycleResultPayload::Structured(payload.clone())),
            ])
            .with_activity(vec![AgentActivity::FileChanged {
                path: "notes.md".to_string(),
            }]),
        );

        let step = handler
            .subagent_begin(
                new_worktree_spec("reviewer", "summarize the diff"),
                JsonArg(one_declaration()),
                JsonArg(sample_schema()),
            )
            .expect("the scripted turn parks on a tool call");
        let AgAgentStep::StepToolCall(agent, call, tool, arguments) = step else {
            panic!("the scripted turn parks, so begin returns StepToolCall, got {step:?}");
        };
        assert_eq!(tool, "ask_parent");
        assert_eq!(arguments, serde_json::json!({ "q": "which file?" }));

        let step = handler
            .subagent_resume(
                agent,
                call,
                true,
                JsonArg(serde_json::json!({ "file": "notes.md" })),
            )
            .expect("answering the parked call drives the turn on");
        let AgAgentStep::StepDone(outcome) = step else {
            panic!("the second scripted stop completes the turn, got {step:?}");
        };
        assert_eq!(
            outcome.outcome_payload,
            AgCyclePayload::PayloadStructured(payload)
        );
        assert_eq!(
            outcome.outcome_receipt.receipt_rounds, 1,
            "one answered call is one round, and the receipt says so"
        );
        assert_eq!(
            outcome.outcome_activity,
            vec![AgAgentActivity::ActivityFileChanged("notes.md".to_string())],
            "activity reaches the authored surface — the deferral this lane closed"
        );
    }
}
