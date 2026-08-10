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
    AgentRef, BindingState, BindingTable, WorktreeError, WorktreeHandle, WorktreeId,
    WorktreeManager, WorktreeSpec,
};

use crate::backend::OneCycleBackend;
use crate::seam::{
    AgentBackendError, AgentId, BackendThreadId, CycleResultPayload, CycleSpec, ModelPolicy,
    ThreadSpec, TurnId,
};

/// Wall-clock milliseconds since the Unix epoch, for `bound_at_ms`.
///
/// Local rather than shared with `tidepool-worktree`'s identical helper, which
/// is `pub(crate)` there; a clock before the epoch is a broken machine no
/// caller can act on, so it panics rather than widening every signature with a
/// `Result` whose only handling is `unwrap`.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_millis() as i64
}

/// Sanitize a caller-supplied label into the tail of an `AgentRef`.
///
/// The label is decoration — never a path, never an identity (the minted
/// [`AgentId`] in front of it is what makes the ref unique), so it is reduced
/// to `[A-Za-z0-9._-]` with runs of `-` collapsed and the ends trimmed. Same
/// shape as `create.rs`'s branch-name sanitizer minus `/`, which has no
/// business in a ref that is written into a filename-adjacent record. An
/// all-punctuation label falls back to `worker` rather than yielding a ref
/// ending in a bare `-`.
fn sanitize_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut last_was_dash = false;
    for c in label.chars() {
        let c = if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
            c
        } else {
            '-'
        };
        if c == '-' && last_was_dash {
            continue;
        }
        last_was_dash = c == '-';
        out.push(c);
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '.');
    if trimmed.is_empty() {
        "worker".to_string()
    } else {
        trimmed.to_string()
    }
}

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

impl SpawnError {
    /// How far the saga got. `RollbackFailed` reports the stage of the failure
    /// it wraps, not a stage of its own — the rollback is not a saga step.
    pub fn stage(&self) -> SpawnStage {
        match self {
            SpawnError::Worktree { stage, .. }
            | SpawnError::Binding { stage, .. }
            | SpawnError::Backend { stage, .. }
            | SpawnError::RollbackFailed { stage, .. } => *stage,
        }
    }
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
        backend: &mut dyn OneCycleBackend,
        request: &SpawnRequest,
    ) -> Result<OneCycleRun, SpawnError> {
        // 1. Allocating → WorktreeReady. Nothing is bound yet, so a failure
        //    here has nothing to compensate: no binding row is ever written.
        let worktree = self.resolve_workspace(&request.workspace)?;

        // 2. Identity. The label is decoration on the `AgentRef` — the id is
        //    what makes it unique — so sanitizing it cannot collide two agents.
        let agent = self.mint_agent_id();
        let binding_ref = format!("agent-{}-{}", agent.0, sanitize_label(&request.agent_label));

        // 3. Bound. `WorktreeBusy` (one worktree, one agent) and a persist
        //    failure are both binding failures; `BindingTable::bind` already
        //    rolled its own memory back on the latter, so again there is
        //    nothing here to compensate.
        self.bindings
            .bind(
                worktree.id(),
                &AgentRef::from_raw(binding_ref.clone()),
                now_ms(),
            )
            .map_err(|error| SpawnError::Binding {
                stage: SpawnStage::Bound,
                error,
            })?;

        // 4. ThreadAccepted. From here on every failure path must settle the
        //    binding `Released` — the worktree is retained (locked), but
        //    leaving it Active-bound to an agent that will never run is the
        //    orphan this saga exists to prevent.
        let thread = match backend.start_thread(&request.thread_spec()) {
            Ok(thread) => thread,
            Err(error) => {
                return Err(self.roll_back(
                    worktree.id(),
                    SpawnError::Backend {
                        stage: SpawnStage::ThreadAccepted,
                        error,
                    },
                ));
            }
        };

        // 5. Running. `cwd` is supplied per-cycle, not at thread creation —
        //    see `CycleSpec`'s docs for why the seam splits it that way.
        let cwd = worktree.cwd().to_string_lossy().into_owned();
        let outcome = match backend.run_cycle(&thread, &request.cycle_spec(cwd)) {
            Ok(outcome) => outcome,
            Err(error) => {
                return Err(self.roll_back(
                    worktree.id(),
                    SpawnError::Backend {
                        stage: SpawnStage::Running,
                        error,
                    },
                ));
            }
        };

        // 6. Success: the cycle IS the agent's whole life (lane 1), so the
        //    binding settles `Terminal`.
        //
        //    A settle failure here is reported `RollbackFailed` rather than
        //    `Binding`, deliberately: `RollbackFailed` is this type's ONLY
        //    signal that an Active binding may still be on disk, and that is
        //    exactly what a failed terminal settle leaves behind. Returning
        //    `Binding` would satisfy the "no Active binding remains" promise
        //    this enum's docs make for every other variant, and it would be a
        //    lie. `original` and `rollback` carry the same failure because on
        //    the success path the settle is both the operation and its own
        //    compensation — there is no earlier failure to lose, and no second
        //    write worth attempting against storage that just refused one.
        if let Err(rollback) = self.bindings.settle(worktree.id(), BindingState::Terminal) {
            return Err(SpawnError::RollbackFailed {
                stage: SpawnStage::Running,
                original: Box::new(SpawnError::Binding {
                    stage: SpawnStage::Running,
                    error: rollback.clone(),
                }),
                rollback,
            });
        }

        let receipt = SpawnReceipt {
            agent,
            worktree: worktree.id().clone(),
            binding_ref,
            thread: thread.clone(),
            resolved_model: outcome.resolved_model,
            turn: outcome.turn,
        };
        Ok(OneCycleRun {
            run: WorkerRun {
                agent,
                worktree,
                thread,
            },
            payload: outcome.payload,
            receipt,
            activity: outcome.activity,
        })
    }

    /// Stage 1: a new managed worktree, or an existing one by durable id.
    ///
    /// The `lookup` cases stay distinct all the way out: `Ok(None)` is a typo
    /// or a stale id (`WorktreeNotRegistered`), `Err(WorktreeLost)` is data
    /// loss, and collapsing them would tell an operator whose worktree a human
    /// deleted that it never existed.
    fn resolve_workspace(&self, workspace: &SpawnWorkspace) -> Result<WorktreeHandle, SpawnError> {
        let allocating = |error| SpawnError::Worktree {
            stage: SpawnStage::Allocating,
            error,
        };
        match workspace {
            SpawnWorkspace::New(spec) => self.manager.create(spec).map_err(allocating),
            SpawnWorkspace::Existing(id) => match self.manager.lookup(id) {
                Ok(Some(handle)) => Ok(handle),
                Ok(None) => Err(allocating(WorktreeError::WorktreeNotRegistered(id.clone()))),
                Err(error) => Err(allocating(error)),
            },
        }
    }

    /// Compensate a post-`Bound` failure: settle the binding `Released` so the
    /// retained worktree is left UNBOUND and rebindable, and return the error
    /// the caller should see. A rollback that itself fails is escalated to
    /// [`SpawnError::RollbackFailed`] carrying BOTH — swallowing either hides
    /// the one a fix needs.
    fn roll_back(&mut self, worktree: &WorktreeId, original: SpawnError) -> SpawnError {
        let stage = original.stage();
        match self.bindings.settle(worktree, BindingState::Released) {
            Ok(()) => original,
            Err(rollback) => SpawnError::RollbackFailed {
                stage,
                original: Box::new(original),
                rollback,
            },
        }
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
