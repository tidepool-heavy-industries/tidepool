//! Atomic coupled spawn — LANE 1 of PRD 18 (per the 2026-08-09 addendum's
//! locked decisions 3 and 4: ONE authored call yields agent + worktree, and
//! `createWorktree` is not public vocabulary).
//!
//! One call runs the whole saga:
//!
//! ```text
//! Allocating → WorktreeReady → Bound → ThreadAccepted → Running
//! ```
//!
//! and returns either a completed [`OneCycleRun`] or ONE typed [`SpawnError`]
//! naming the stage that failed — the saga is hidden behind the call, never
//! exposed as separate authored steps.
//!
//! ## Rollback semantics, and what "no orphaned worktree" means here
//!
//! `tidepool-worktree` is retain-first (locked): nothing deletes a worktree,
//! branch, ref, or record. So rollback here does NOT undo creation — it
//! settles the binding, which is the only state whose persistence would lie:
//!
//! - failure after `Bound`: settle the binding `Released`. End state: the
//!   worktree is retained, registered, UNBOUND, rebindable. That IS the
//!   rolled-back state; "orphaned" means "left Active-bound to an agent that
//!   will never run", not "exists".
//! - success: settle the binding `Terminal` — a lane-1 agent is one cycle by
//!   construction, so cycle completion is agent completion. `Terminal` vs
//!   `Released` is the finished-vs-stopped-waiting distinction `binding.rs`
//!   documents; both permit rebinding.
//! - a rollback that itself fails is [`SpawnError::RollbackFailed`], loud,
//!   carrying both the original failure and the rollback failure — never a
//!   silent swallow of either.
//!
//! ## What this module does not do
//!
//! No git verbs (the manager owns git truth), no backend vocabulary (the
//! [`OneCycleBackend`] seam owns that), no durable agent registry (the
//! in-process [`AgentId`] mint is a lane-1 provisional — durable identity is
//! lifecycle-lane territory, routed in the lane handoff).

use tidepool_worktree::{
    BindingState, BindingTable, WorktreeError, WorktreeHandle, WorktreeId, WorktreeManager,
    WorktreeSpec,
};

use crate::backend::OneCycleBackend;
use crate::seam::{
    AgentBackendError, AgentId, BackendThreadId, CycleResultPayload, CycleSpec, ModelPolicy,
    ThreadSpec, TurnId,
};

/// Where in the saga something happened. Carried on every [`SpawnError`] so a
/// caller (and a receipt reader) can see how far the spawn got without
/// reconstructing it from prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnStage {
    /// Resolving the workspace: creating a managed worktree, or looking up an
    /// existing one.
    Allocating,
    /// Worktree handle in hand, not yet bound.
    WorktreeReady,
    /// Binding taken (`agent-<id>` is the binding's `AgentRef`).
    Bound,
    /// The backend accepted the thread.
    ThreadAccepted,
    /// The cycle was running.
    Running,
}

impl std::fmt::Display for SpawnStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SpawnStage::Allocating => "allocating",
            SpawnStage::WorktreeReady => "worktree-ready",
            SpawnStage::Bound => "bound",
            SpawnStage::ThreadAccepted => "thread-accepted",
            SpawnStage::Running => "running",
        };
        f.write_str(s)
    }
}

/// The ONE typed error a coupled spawn can return (PRD 18 addendum decision 2:
/// typed failure results everywhere; variant list filled by this lane's
/// contact with reality, deliberately provisional).
///
/// By the time a caller sees any variant except `RollbackFailed`, the rollback
/// has already happened: no Active binding remains, and the worktree (if one
/// was created) is retained and rebindable.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SpawnError {
    /// The workspace could not be resolved (creation failed, or an existing id
    /// was lost/unregistered).
    #[error("spawn failed at {stage} (worktree): {error}")]
    Worktree {
        stage: SpawnStage,
        error: WorktreeError,
    },

    /// The binding was refused (`WorktreeBusy` names the holder) or could not
    /// be persisted.
    #[error("spawn failed at {stage} (binding): {error}")]
    Binding {
        stage: SpawnStage,
        error: WorktreeError,
    },

    /// The backend failed. `stage` distinguishes a rejected thread from a
    /// failed cycle.
    #[error("spawn failed at {stage} (backend): {error}")]
    Backend {
        stage: SpawnStage,
        error: AgentBackendError,
    },

    /// The rollback itself failed. Both failures are carried — losing either
    /// would hide the one a fix needs.
    #[error(
        "spawn failed at {stage} AND its rollback failed: original={original}; rollback={rollback}"
    )]
    RollbackFailed {
        stage: SpawnStage,
        original: Box<SpawnError>,
        rollback: WorktreeError,
    },
}

/// What workspace a spawn runs in — a new managed worktree, or an existing
/// UNBOUND one by durable id (PRD 18 addendum decision 3: `spawnAgent`
/// accepts a `WorktreeSpec` OR an existing unbound worktree handle).
#[derive(Debug, Clone)]
pub enum SpawnWorkspace {
    New(WorktreeSpec),
    Existing(WorktreeId),
}

/// Everything one coupled spawn needs. Lane 1: no dynamic tools on the
/// authored surface, ephemeral thread, cheap-plumbing model tier.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub workspace: SpawnWorkspace,
    /// Human-readable tail of the binding's `AgentRef` (`agent-<id>-<label>`);
    /// never an identity.
    pub agent_label: String,
    /// The initial task prompt.
    pub task: String,
    /// JSON Schema for the terminal result (from the caller's result type).
    pub output_schema: Option<serde_json::Value>,
}

impl SpawnRequest {
    /// The lane-1 thread shape: ephemeral, no dynamic tools.
    pub fn thread_spec(&self) -> ThreadSpec {
        ThreadSpec {
            ephemeral: true,
            dynamic_tools: Vec::new(),
        }
    }

    /// The lane-1 cycle shape for a resolved workspace.
    pub fn cycle_spec(&self, cwd: String) -> CycleSpec {
        CycleSpec {
            cwd,
            task: self.task.clone(),
            output_schema: self.output_schema.clone(),
            model: ModelPolicy::CheapPlumbing,
        }
    }
}

/// The coupled pair a successful spawn yields (PRD 19: `WorkerRun` as the
/// result shape), plus the backend thread identity a later attach would need.
#[derive(Debug, Clone)]
pub struct WorkerRun {
    pub agent: AgentId,
    pub worktree: WorktreeHandle,
    pub thread: BackendThreadId,
}

/// The receipt: what the runtime actually did, every field checkable against
/// disk or the backend — never the model's account of itself.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnReceipt {
    pub agent: AgentId,
    pub worktree: WorktreeId,
    /// The exact `AgentRef` string the binding was taken under.
    pub binding_ref: String,
    pub thread: BackendThreadId,
    /// The EXACT model the backend resolved — never the tier name.
    pub resolved_model: String,
    pub turn: TurnId,
}

/// A completed one-cycle run: the coupled pair, the terminal payload (decoded
/// against the caller's type on the Haskell side — `Structured` here is not
/// yet a typed success), and the receipt.
#[derive(Debug, Clone)]
pub struct OneCycleRun {
    pub run: WorkerRun,
    pub payload: CycleResultPayload,
    pub receipt: SpawnReceipt,
    pub activity: Vec<crate::seam::AgentActivity>,
}

/// Owns the worktree substrate handles a coupled spawn needs. One spawner per
/// (registry root, binding root) — `BindingTable`'s lifetime flock enforces
/// the single-owner precondition, so constructing a second spawner over the
/// same binding root fails loudly at `open`.
pub struct CoupledSpawner {
    manager: WorktreeManager,
    bindings: BindingTable,
    next_agent: u64,
}

impl CoupledSpawner {
    /// Open a spawner over an existing manager and a binding root.
    pub fn open(
        manager: WorktreeManager,
        binding_root: impl AsRef<std::path::Path>,
    ) -> Result<Self, WorktreeError> {
        Ok(Self {
            manager,
            bindings: BindingTable::open(binding_root)?,
            next_agent: 0,
        })
    }

    pub fn manager(&self) -> &WorktreeManager {
        &self.manager
    }

    pub fn bindings(&self) -> &BindingTable {
        &self.bindings
    }

    /// Mint the next in-process agent identity. PROVISIONAL (lane 1): durable
    /// agent identity across restarts is lifecycle-lane territory.
    pub fn mint_agent_id(&mut self) -> AgentId {
        let id = AgentId(self.next_agent);
        self.next_agent += 1;
        id
    }

    /// Run the whole coupled-spawn saga: workspace, binding, thread, one
    /// cycle, receipt — or ONE typed error, with the rollback already done.
    ///
    /// See the module docs for the stage diagram and rollback semantics.
    pub fn spawn_one_cycle(
        &mut self,
        _backend: &mut dyn OneCycleBackend,
        _request: &SpawnRequest,
    ) -> Result<OneCycleRun, SpawnError> {
        // Dev lane `saga` implements this per lane1-scaffold-plan.md:
        //  1. Allocating: New → manager.create(spec); Existing → manager.lookup
        //     (WorktreeLost / not-registered → SpawnError::Worktree).
        //  2. mint AgentId, binding_ref = "agent-<id>-<sanitized label>".
        //  3. bind → SpawnError::Binding on refusal (WorktreeBusy names holder).
        //  4. start_thread → on Err: settle(Released) then SpawnError::Backend
        //     { stage: ThreadAccepted-edge } (rollback failure →
        //     RollbackFailed).
        //  5. run_cycle → on Err: settle(Released), as above, stage Running.
        //  6. success: settle(Terminal), assemble WorkerRun/receipt.
        todo!("lane-1 saga: implemented by the `saga` dev per the scaffold plan")
    }

    /// Settle the current binding for `worktree` (rollback / completion
    /// helper). Exposed so tests can drive edge cases directly.
    pub fn settle(
        &mut self,
        worktree: &WorktreeId,
        state: BindingState,
    ) -> Result<(), WorktreeError> {
        self.bindings.settle(worktree, state)
    }
}
