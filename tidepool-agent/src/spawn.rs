//! Atomic coupled spawn: one authored call yields an agent + worktree pair.
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
//! ## The split: shared substrate, detachable saga
//!
//! The worktree substrate ([`WorktreeManager`] + the flocked [`BindingTable`])
//! is single-owner BY CONSTRUCTION — `BindingTable::open` takes a lifetime
//! flock on its binding root, so there is exactly one of these per binding
//! root and "one substrate per cycle" is not available. The backend, by
//! contrast, must be per-cycle: an `AgentBackend` is a step function over ONE
//! live thread.
//!
//! So concurrency is expressed as two pieces, not one:
//!
//! - [`SpawnSubstrate`] — SHARED, mutex-guarded, and touched only in SHORT
//!   critical sections: allocate a worktree, take a binding, settle a binding,
//!   mint an agent id.
//! - [`CycleSaga`] — PER-CYCLE, detachable, and holding no lock while it
//!   blocks. Every `start_thread` / `start_turn` / `resume` call happens with
//!   no substrate lock held.
//!
//! **The load-bearing invariant of this module: a saga must never hold the
//! [`SpawnSubstrate`] mutex across a backend call.** A saga that does
//! serializes every cycle and silently reinstates the one-agent-at-a-time
//! constraint this design exists to remove. It is visible in the code shape as
//! well as in this prose: every critical section in this module is opened by
//! [`lock_substrate`] (or by a bare `substrate.lock()` in
//! [`CycleSaga::abandon`] and [`roll_back_detached`]), and NONE of those scopes
//! contains a `backend.` call. Grep `lock_substrate` to audit it — the guard is
//! never a `let` that outlives its block and never a struct field.
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
//! - success: settle the binding `Terminal` — an agent from this spawner is
//!   one cycle by construction, so cycle completion is agent completion.
//!   `Terminal` vs `Released` is the finished-vs-stopped-waiting distinction
//!   `binding.rs` documents; both permit rebinding.
//! - CANCELLATION ([`CycleSaga::abandon`]): settle `Released`, same as any
//!   post-`Bound` failure. A cancelled cycle is no more entitled to leak an
//!   Active binding than a failed one is.
//! - a rollback that itself fails is [`SpawnError::RollbackFailed`], loud,
//!   carrying both the original failure and the rollback failure — never a
//!   silent swallow of either.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use tidepool_worktree::{
    AgentLabel, AgentRef, BindingState, BindingTable, WorktreeError, WorktreeHandle, WorktreeId,
    WorktreeManager, WorktreeSpec,
};

use crate::backend::AgentBackend;
use crate::seam::{
    AgentBackendError, AgentId, BackendThreadId, CycleResultPayload, CycleSpec,
    DynamicToolDeclaration, ModelPolicy, ReasoningEffort, ThreadSpec, TokenUsage, ToolCall,
    ToolCallId, ToolOutcome, ToolReply, TurnEvent, TurnId,
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

/// The ONE typed error a coupled spawn can return.
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

    /// A tool reply named an agent that is not running — never begun, or
    /// already finished. Not a saga STAGE (nothing was being allocated, bound,
    /// or run); a caller-sequencing failure, which is why it has its own
    /// variant rather than being folded onto `Backend`.
    #[error("no running agent {agent:?} to answer: {detail}")]
    NotRunning { agent: AgentId, detail: String },

    /// The runtime's hard backstop on tool-call rounds fired
    /// ([`MAX_TOOL_ROUNDS`]) — the authored policy cap was absent or broken.
    #[error("agent {agent:?} exceeded the runtime tool-round backstop of {limit}")]
    RoundBackstop { agent: AgentId, limit: u32 },
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
            // Both of these can only happen with a turn in flight.
            SpawnError::NotRunning { .. } | SpawnError::RoundBackstop { .. } => SpawnStage::Running,
        }
    }
}

/// The runtime's hard ceiling on tool-call rounds in one turn.
///
/// A backstop, not a policy: the authored cap lives in the Haskell driver loop
/// (where it can refuse politely and let the child finish its turn), and this
/// exists only so a missing or broken policy cap cannot spin against a live
/// backend indefinitely. Set well above any plausible authored cap, because a
/// backstop that fires during normal work is a bug generator.
pub const MAX_TOOL_ROUNDS: u32 = 64;

/// What workspace a spawn runs in — a new managed worktree, or an existing
/// UNBOUND one by durable id.
#[derive(Debug, Clone)]
pub enum SpawnWorkspace {
    New(WorktreeSpec),
    Existing(WorktreeId),
}

/// Everything one coupled spawn needs.
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
    /// The tools this agent may call, compiled from the caller's tools record.
    /// Frozen for the agent's life — dynamic tools are thread-scoped.
    pub tools: Vec<DynamicToolDeclaration>,
    /// Which model tier, and how hard it should think. Supplied by the caller
    /// rather than fixed here: the two are budget decisions, and a budget is
    /// granted to an operator, not baked into a saga.
    pub model: ModelPolicy,
    pub effort: ReasoningEffort,
}

impl SpawnRequest {
    /// The thread shape: ephemeral, carrying the declared tools.
    pub fn thread_spec(&self) -> ThreadSpec {
        ThreadSpec {
            ephemeral: true,
            dynamic_tools: self.tools.clone(),
        }
    }

    /// The cycle shape for a resolved workspace.
    pub fn cycle_spec(&self, cwd: String, extra_writable_roots: Vec<String>) -> CycleSpec {
        CycleSpec {
            cwd,
            task: self.task.clone(),
            output_schema: self.output_schema.clone(),
            model: self.model,
            effort: self.effort,
            extra_writable_roots,
        }
    }
}

/// Where a driven spawn stopped.
///
/// The authored loop alternates between these: a `ToolCall` is answered and
/// driving continues; a `Done` is the end of the agent's life.
#[derive(Debug, Clone)]
pub enum SpawnStep {
    /// The child called a tool. Its turn is PARKED until
    /// [`CoupledSpawner::answer`].
    ToolCall { agent: AgentId, call: ToolCall },
    /// The cycle finished, the binding is settled, and the receipt is complete.
    Done(Box<OneCycleRun>),
}

/// The coupled pair a successful spawn yields, plus the backend thread
/// identity a later attach would need.
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
    /// How many tool-call rounds the child actually took. Checkable against the
    /// backend's own transcript, and the number a budget conversation needs.
    pub rounds: u32,
    /// What the turn cost, when the backend reported it. `None` is "the backend
    /// said nothing", never "it was free".
    pub usage: Option<TokenUsage>,
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

// ============================================================================
// The shared substrate.
// ============================================================================

/// The single-owner worktree substrate a cycle touches, and the ONLY thing
/// concurrent cycles contend on.
///
/// Every method here is a SHORT critical section. **No backend call may happen
/// under this lock** — see the module docs; that is the invariant the whole
/// concurrent design rests on.
///
/// Single-owner is not a policy choice: [`BindingTable::open`] takes a lifetime
/// flock on its binding root, so a second table over the same root fails
/// loudly at `open`. That is why the substrate is shared behind a mutex rather
/// than cloned per cycle.
///
/// [`resolve_workspace`](Self::resolve_workspace) is inside the lock even
/// though [`WorktreeManager`] is internally immutable (`&self` everywhere, ids
/// minted from a process-global atomic, records written with a rename). It is
/// here because concurrent `git worktree add` invocations against ONE source
/// repository contend on git's own index/worktree locks, and a spurious
/// `GitFailure` from that contention would read as a real allocation failure.
/// A worktree add is tens to hundreds of milliseconds against a model turn
/// measured in seconds, so serializing it costs nothing the lane cares about.
pub struct SpawnSubstrate {
    manager: Arc<WorktreeManager>,
    bindings: BindingTable,
    next_agent: u64,
}

impl SpawnSubstrate {
    pub fn manager(&self) -> &WorktreeManager {
        &self.manager
    }

    pub fn bindings(&self) -> &BindingTable {
        &self.bindings
    }

    /// The source repository managed worktrees are created from. A worker's
    /// write sandbox must admit its `.git`, because a LINKED worktree's git
    /// metadata lives there and not under the worktree.
    pub fn source_repository(&self) -> &std::path::Path {
        self.manager.source_repository()
    }

    /// Mint the next in-process agent identity. PROVISIONAL: not durable
    /// across restarts.
    pub fn mint_agent_id(&mut self) -> AgentId {
        let id = AgentId(self.next_agent);
        self.next_agent += 1;
        id
    }

    /// Stage 1: a new managed worktree, or an existing one by durable id.
    ///
    /// The `lookup` cases stay distinct all the way out: `Ok(None)` is a typo
    /// or a stale id (`WorktreeNotRegistered`), `Err(WorktreeLost)` is data
    /// loss, and collapsing them would tell an operator whose worktree a human
    /// deleted that it never existed.
    pub fn resolve_workspace(
        &self,
        workspace: &SpawnWorkspace,
    ) -> Result<WorktreeHandle, SpawnError> {
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

    /// Take the binding for `worktree` under `binding_ref`.
    ///
    /// `WorktreeBusy` (one worktree, one agent) and a persist failure are both
    /// binding failures; `BindingTable::bind` already rolled its own memory
    /// back on the latter, so there is nothing here to compensate.
    pub fn bind(&mut self, worktree: &WorktreeId, binding_ref: &str) -> Result<(), SpawnError> {
        self.bindings
            .bind(
                worktree,
                &AgentRef::from_raw(binding_ref.to_string()),
                tidepool_worktree::storage::now_ms(),
            )
            .map_err(|error| SpawnError::Binding {
                stage: SpawnStage::Bound,
                error,
            })
    }

    /// Settle the current binding for `worktree`.
    pub fn settle(
        &mut self,
        worktree: &WorktreeId,
        state: BindingState,
    ) -> Result<(), WorktreeError> {
        self.bindings.settle(worktree, state)
    }

    /// Compensate a post-`Bound` failure: settle the binding `Released` so the
    /// retained worktree is left UNBOUND and rebindable, and return the error
    /// the caller should see. A rollback that itself fails is escalated to
    /// [`SpawnError::RollbackFailed`] carrying BOTH — swallowing either hides
    /// the one a fix needs.
    pub fn roll_back(&mut self, worktree: &WorktreeId, original: SpawnError) -> SpawnError {
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
}

/// The rendering a POISONED substrate mutex gets.
///
/// Poisoning here means a concurrent cycle's saga panicked while holding the
/// lock, so the [`BindingTable`]'s in-memory rows may not match what is on
/// disk and no invariant a saga relies on can be assumed. Two things this
/// deliberately is NOT: it is not an `unwrap()` (which would turn one cycle's
/// panic into every other cycle's panic, losing the diagnosis), and it is not
/// a recover-by-ignoring (`PoisonError::into_inner`, which would keep writing
/// binding rows on top of unknown state).
///
/// It is spelled as a [`WorktreeError::StorageFailure`] against the binding
/// root rather than as a new [`SpawnError`] variant on purpose: the durable
/// binding state is exactly what became untrustworthy, and `SpawnError`'s
/// variants are the wire contract `tidepool-handlers` converts exhaustively —
/// widening it is a separate, coordinated change, not something a lock
/// mechanism gets to do.
fn poisoned(stage: SpawnStage) -> SpawnError {
    SpawnError::Binding {
        stage,
        error: poisoned_storage(),
    }
}

fn poisoned_storage() -> WorktreeError {
    WorktreeError::StorageFailure {
        path: std::path::PathBuf::from("<spawn substrate>"),
        detail: "the shared spawn substrate's mutex is POISONED: a concurrent cycle's saga \
                 panicked while holding it, so the binding table's in-memory rows may not match \
                 disk. Every later spawn against this substrate fails here rather than proceeding \
                 on unknown invariants."
            .to_string(),
    }
}

/// Lock the substrate, rendering poisoning as a typed failure at `stage`.
///
/// The returned guard is deliberately short-lived at every call site: nothing
/// that calls a backend may hold it. Grep for this function to audit the
/// invariant — every critical section in this module starts here.
fn lock_substrate(
    substrate: &Arc<Mutex<SpawnSubstrate>>,
    stage: SpawnStage,
) -> Result<MutexGuard<'_, SpawnSubstrate>, SpawnError> {
    substrate.lock().map_err(|_| poisoned(stage))
}

/// Compensate a post-`Bound` failure that happened before a [`CycleSaga`]
/// existed — the window inside [`CycleSaga::begin`] between taking the binding
/// and having a backend thread id to build the saga around.
///
/// A SHORT critical section, entered only after the backend call that failed
/// has already returned.
fn roll_back_detached(
    substrate: &Arc<Mutex<SpawnSubstrate>>,
    worktree: &WorktreeId,
    original: SpawnError,
) -> SpawnError {
    let stage = original.stage();
    match substrate.lock() {
        Ok(mut sub) => sub.roll_back(worktree, original),
        Err(_) => SpawnError::RollbackFailed {
            stage,
            original: Box::new(original),
            rollback: poisoned_storage(),
        },
    }
}

// ============================================================================
// The saga.
// ============================================================================

/// One cycle's saga state, detached from the spawner that started it.
///
/// Carries its own `Arc<Mutex<SpawnSubstrate>>`, so a saga can be MOVED to
/// another thread and driven there. It locks the substrate only to bind,
/// settle, and roll back — **never across a backend call**. The two helpers
/// that touch a backend ([`Self::start_thread_and_turn`] and [`Self::drive`])
/// take no guard and cannot: the guard's lifetime is confined to
/// [`lock_substrate`] call sites, none of which contain a backend call.
pub struct CycleSaga {
    substrate: Arc<Mutex<SpawnSubstrate>>,
    agent: AgentId,
    worktree: WorktreeHandle,
    thread: BackendThreadId,
    binding_ref: String,
    /// The call awaiting an answer. `None` between a completed step and the
    /// next — which cannot be observed by a caller, since every step either
    /// parks or finishes.
    parked: Option<ToolCallId>,
    rounds: u32,
    /// Whether this saga's binding has already been settled — by completion,
    /// by rollback, or by a previous [`abandon`](Self::abandon).
    ///
    /// This is what makes `abandon` idempotent: a cancel racing a completion is
    /// a real sequence, and settling twice would write a second lease row for a
    /// life that ended once.
    settled: bool,
}

/// Hand-written rather than derived: the substrate handle is SHARED, so
/// printing it would print another cycle's state (and take the lock to do it,
/// from a `Debug` impl, which is exactly the wrong place to block). What a
/// reader wants from a saga is which cycle it is and where it stopped.
impl std::fmt::Debug for CycleSaga {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CycleSaga")
            .field("agent", &self.agent)
            .field("worktree", &self.worktree.id())
            .field("thread", &self.thread)
            .field("binding_ref", &self.binding_ref)
            .field("parked", &self.parked)
            .field("rounds", &self.rounds)
            .field("settled", &self.settled)
            .finish_non_exhaustive()
    }
}

impl CycleSaga {
    /// Run the saga from `Allocating` to its first stop.
    ///
    /// Returns the saga alongside the step even when the step is
    /// [`SpawnStep::Done`], so a caller can read the terminal state uniformly;
    /// [`is_finished`](Self::is_finished) says which it is.
    ///
    /// See the module docs for the stage diagram and rollback semantics. Every
    /// error returned here has already rolled back.
    pub fn begin(
        substrate: &Arc<Mutex<SpawnSubstrate>>,
        backend: &mut dyn AgentBackend,
        request: &SpawnRequest,
    ) -> Result<(Self, SpawnStep), SpawnError> {
        // --- critical section: allocate, mint, bind. No backend call here. ---
        let (worktree, agent, binding_ref, git_dir) = {
            let mut sub = lock_substrate(substrate, SpawnStage::Allocating)?;

            // 1. Allocating → WorktreeReady. Nothing is bound yet, so a failure
            //    here has nothing to compensate: no binding row is ever written.
            let worktree = sub.resolve_workspace(&request.workspace)?;

            // 2. Identity: mint the agent id, then sanitize the label into the
            //    `AgentRef` tail.
            let agent = sub.mint_agent_id();
            let binding_ref = format!(
                "agent-{}-{}",
                agent.0,
                AgentLabel::new(&request.agent_label).as_str()
            );

            // 3. Bound.
            sub.bind(worktree.id(), &binding_ref)?;

            // The linked worktree's git metadata lives in the SOURCE repo's
            // `.git`; the sandbox must admit it or no worker can ever commit.
            let git_dir = sub
                .source_repository()
                .join(".git")
                .to_string_lossy()
                .into_owned();
            (worktree, agent, binding_ref, git_dir)
        };
        // --- lock released. Everything below may block for a whole turn. ---

        // 4. ThreadAccepted. From here on every failure path must settle the
        //    binding `Released` — the worktree is retained (locked), but
        //    leaving it Active-bound to an agent that will never run is the
        //    orphan this saga exists to prevent. The saga struct is not
        //    constructed until there is a real thread id to put in it, so
        //    `CycleSaga::thread` is never transiently a placeholder.
        let thread = match backend.start_thread(&request.thread_spec()) {
            Ok(thread) => thread,
            Err(error) => {
                return Err(roll_back_detached(
                    substrate,
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
        let event = match backend.start_turn(&thread, &request.cycle_spec(cwd, vec![git_dir])) {
            Ok(event) => event,
            Err(error) => {
                return Err(roll_back_detached(
                    substrate,
                    worktree.id(),
                    SpawnError::Backend {
                        stage: SpawnStage::Running,
                        error,
                    },
                ));
            }
        };

        let mut saga = Self {
            substrate: Arc::clone(substrate),
            agent,
            worktree,
            thread,
            binding_ref,
            parked: None,
            rounds: 0,
            settled: false,
        };
        let step = saga.settle_step(event)?;
        Ok((saga, step))
    }

    /// Answer the parked tool call and drive on to the next stop.
    ///
    /// `agent` is checked against THIS saga's agent before anything is sent:
    /// answering the wrong agent is the misroute the correlation triple exists
    /// to catch, and catching it here costs nothing. With N cycles in flight
    /// that check is more load-bearing than it was with one, not less.
    pub fn answer(
        &mut self,
        backend: &mut dyn AgentBackend,
        agent: AgentId,
        call: ToolCallId,
        outcome: ToolOutcome,
    ) -> Result<SpawnStep, SpawnError> {
        if self.settled {
            return Err(SpawnError::NotRunning {
                agent,
                detail: "no agent is mid-turn".to_string(),
            });
        }
        if self.agent != agent {
            return Err(SpawnError::NotRunning {
                agent,
                detail: format!("agent {} is the one mid-turn", self.agent.0),
            });
        }
        match &self.parked {
            Some(parked) if *parked == call => {}
            Some(parked) => {
                return Err(SpawnError::NotRunning {
                    agent,
                    detail: format!(
                        "call {} is parked, not {} — refusing to answer the wrong call",
                        parked.0, call.0
                    ),
                })
            }
            None => {
                return Err(SpawnError::NotRunning {
                    agent,
                    detail: format!("no call is parked; {} answers nothing", call.0),
                })
            }
        }

        self.rounds += 1;
        if self.rounds > MAX_TOOL_ROUNDS {
            let error = SpawnError::RoundBackstop {
                agent,
                limit: MAX_TOOL_ROUNDS,
            };
            return Err(self.roll_back(error));
        }
        self.parked = None;

        // No substrate lock is held here, and none may be.
        let event = match backend.resume(ToolReply { call, outcome }) {
            Ok(event) => event,
            Err(error) => {
                return Err(self.roll_back(SpawnError::Backend {
                    stage: SpawnStage::Running,
                    error,
                }));
            }
        };
        self.settle_step(event)
    }

    /// Drive to completion, refusing every tool call the child makes.
    ///
    /// The no-tools path: a request carrying no declarations should produce no
    /// calls, and one that arrives anyway is refused rather than left parked.
    ///
    /// A COMBINATOR over [`Self::answer`], never a second saga implementation.
    ///
    /// `first` is the step [`Self::begin`] already returned. It is a parameter
    /// rather than something the saga re-derives because the first step is not
    /// reconstructible: a parked call's TOOL NAME (which the refusal text
    /// names) is not saga state, and a first step that was already
    /// [`SpawnStep::Done`] carries the only [`OneCycleRun`] there will ever be.
    /// Threading it through is what keeps this a combinator instead of a
    /// second driving path.
    pub fn run_to_completion(
        &mut self,
        backend: &mut dyn AgentBackend,
        first: SpawnStep,
    ) -> Result<OneCycleRun, SpawnError> {
        let mut step = first;
        loop {
            match step {
                SpawnStep::Done(run) => return Ok(*run),
                SpawnStep::ToolCall { agent, call } => {
                    let refusal = ToolOutcome::Refused(format!(
                        "no such tool: {} — this agent was created with no dynamic tools",
                        call.tool
                    ));
                    step = self.answer(backend, agent, call.call, refusal)?;
                }
            }
        }
    }

    pub fn agent(&self) -> AgentId {
        self.agent
    }

    pub fn worktree(&self) -> &WorktreeHandle {
        &self.worktree
    }

    /// The exact `AgentRef` string this saga's binding was taken under.
    pub fn binding_ref(&self) -> &str {
        &self.binding_ref
    }

    /// Whether this saga's life is over — completed, rolled back, or
    /// abandoned. A finished saga answers nothing and settles nothing further.
    pub fn is_finished(&self) -> bool {
        self.settled
    }

    /// Settle the binding `Released` for a cycle that is being ABANDONED
    /// (cancelled) rather than completed.
    ///
    /// Cancellation is not exempt from the rollback rule: reaping the backend
    /// stops the work, but a binding row left Active points at an agent that
    /// will never run again — the exact orphan this saga's rollback semantics
    /// exist to prevent. Retain-first is locked, so this settles and deletes
    /// NOTHING: the worktree stays registered, on disk, and rebindable.
    ///
    /// IDEMPOTENT. On a saga that already settled — completed, rolled back, or
    /// previously abandoned — this is a no-op returning `Ok(())`, never a
    /// second write and never an error. A cancel racing a completion is a real
    /// sequence, and double-writing the binding table would record two lease
    /// rows for a life that ended once.
    ///
    /// ORDER, for a caller cancelling a live cycle: reap the backend FIRST
    /// (via [`BackendCanceller`](crate::backend::BackendCanceller)), then call
    /// this. Settling first would hold the substrate lock across a reap of
    /// unknown duration, which is the invariant this module is built around.
    pub fn abandon(&mut self) -> Result<(), WorktreeError> {
        if self.settled {
            return Ok(());
        }
        let mut sub = self.substrate.lock().map_err(|_| poisoned_storage())?;
        sub.settle(self.worktree.id(), BindingState::Released)?;
        self.settled = true;
        Ok(())
    }

    /// Turn one backend event into a saga step, settling the binding when the
    /// turn ends.
    ///
    /// Success settles `Terminal` — a cycle IS this agent's whole life, so
    /// cycle completion is agent completion.
    ///
    /// A settle failure here is reported `RollbackFailed` rather than
    /// `Binding`, deliberately: `RollbackFailed` is `SpawnError`'s ONLY signal
    /// that an Active binding may still be on disk, and that is exactly what a
    /// failed terminal settle leaves behind. Returning `Binding` would satisfy
    /// the "no Active binding remains" promise this enum's docs make for every
    /// other variant, and it would be a lie. `original` and `rollback` carry
    /// the same failure because on the success path the settle is both the
    /// operation and its own compensation — there is no earlier failure to
    /// lose, and no second write worth attempting against storage that just
    /// refused one.
    fn settle_step(&mut self, event: TurnEvent) -> Result<SpawnStep, SpawnError> {
        match event {
            TurnEvent::ToolCall(call) => {
                self.parked = Some(call.call.clone());
                Ok(SpawnStep::ToolCall {
                    agent: self.agent,
                    call,
                })
            }
            TurnEvent::Completed(outcome) => {
                // --- critical section: one settle. No backend call here. ---
                {
                    let mut sub = lock_substrate(&self.substrate, SpawnStage::Running)?;
                    if let Err(rollback) = sub.settle(self.worktree.id(), BindingState::Terminal) {
                        return Err(SpawnError::RollbackFailed {
                            stage: SpawnStage::Running,
                            original: Box::new(SpawnError::Binding {
                                stage: SpawnStage::Running,
                                error: rollback.clone(),
                            }),
                            rollback,
                        });
                    }
                }
                self.settled = true;
                let receipt = SpawnReceipt {
                    agent: self.agent,
                    worktree: self.worktree.id().clone(),
                    binding_ref: self.binding_ref.clone(),
                    thread: self.thread.clone(),
                    resolved_model: outcome.resolved_model,
                    turn: outcome.turn,
                    rounds: self.rounds,
                    usage: outcome.usage,
                };
                Ok(SpawnStep::Done(Box::new(OneCycleRun {
                    run: WorkerRun {
                        agent: self.agent,
                        worktree: self.worktree.clone(),
                        thread: self.thread.clone(),
                    },
                    payload: outcome.payload,
                    receipt,
                    activity: outcome.activity,
                })))
            }
        }
    }

    /// Compensate a post-`Bound` failure and mark this saga finished.
    ///
    /// Delegates the settle to [`SpawnSubstrate::roll_back`] inside a SHORT
    /// critical section; a poisoned lock surfaces as
    /// [`SpawnError::RollbackFailed`] carrying the original, because a
    /// rollback that could not run is exactly the case that variant exists for.
    fn roll_back(&mut self, original: SpawnError) -> SpawnError {
        let stage = original.stage();
        self.parked = None;
        if self.settled {
            return original;
        }
        let mut sub = match self.substrate.lock() {
            Ok(sub) => sub,
            Err(_) => {
                return SpawnError::RollbackFailed {
                    stage,
                    original: Box::new(original),
                    rollback: poisoned_storage(),
                }
            }
        };
        let out = sub.roll_back(self.worktree.id(), original);
        // The binding was settled unless the settle itself failed, which
        // `RollbackFailed` is the loud signal for.
        self.settled = !matches!(out, SpawnError::RollbackFailed { .. });
        out
    }
}

// ============================================================================
// The spawner.
// ============================================================================

/// Owns the worktree substrate handles a coupled spawn needs, and the sagas of
/// the cycles currently in flight.
///
/// One spawner per (registry root, binding root) — `BindingTable`'s lifetime
/// flock enforces the single-owner precondition, so constructing a second
/// spawner over the same binding root fails loudly at `open`.
///
/// **N cycles at a time.** The substrate is shared behind a mutex and each
/// cycle's saga is a separate [`CycleSaga`]; a second [`begin`](Self::begin) is
/// an ordinary spawn, not a refusal. The invariant that makes that real rather
/// than nominal: **no code path holds the [`SpawnSubstrate`] lock across a
/// backend call.** A saga that did would serialize every cycle behind one
/// model turn and silently reinstate one-agent-at-a-time. Every critical
/// section in this module goes through [`lock_substrate`] (or `substrate.lock()`
/// in [`CycleSaga::abandon`], `CycleSaga::roll_back`, and
/// [`roll_back_detached`]), and none of those scopes contains a `backend.`
/// call.
///
/// A caller that drives a cycle on its OWN thread uses
/// [`begin_detached`](Self::begin_detached) and owns the saga; the map here is
/// only for cycles this spawner steps inline.
pub struct CoupledSpawner {
    substrate: Arc<Mutex<SpawnSubstrate>>,
    /// A lock-free handle to the same manager the substrate holds, so
    /// [`manager`](Self::manager) can hand out a plain reference. Sound
    /// because `WorktreeManager` is internally immutable — every method takes
    /// `&self`, ids come from a process-global atomic, and records are written
    /// by rename. The mutex exists for the `BindingTable` and the agent-id
    /// counter, not for this.
    manager: Arc<WorktreeManager>,
    /// The cycles this spawner is stepping inline, keyed by agent.
    running: HashMap<AgentId, CycleSaga>,
}

/// A borrowed view of the substrate's [`BindingTable`], held under the lock.
///
/// Returned rather than a plain `&BindingTable` because the table lives behind
/// the shared mutex. It is a live guard, so two rules apply: HOLD IT BRIEFLY
/// (keeping one across a backend call is the one thing this module forbids),
/// and do not take a second one while the first is alive — the mutex is not
/// reentrant. [`CoupledSpawner::manager`] takes no lock at all precisely so
/// that reading the registry and reading a binding in one expression cannot
/// deadlock.
pub struct BindingsRef<'a>(MutexGuard<'a, SpawnSubstrate>);

impl std::ops::Deref for BindingsRef<'_> {
    type Target = BindingTable;

    fn deref(&self) -> &BindingTable {
        self.0.bindings()
    }
}

impl CoupledSpawner {
    /// Open a spawner over an existing manager and a binding root.
    pub fn open(
        manager: WorktreeManager,
        binding_root: impl AsRef<std::path::Path>,
    ) -> Result<Self, WorktreeError> {
        let manager = Arc::new(manager);
        Ok(Self {
            substrate: Arc::new(Mutex::new(SpawnSubstrate {
                manager: Arc::clone(&manager),
                bindings: BindingTable::open(binding_root)?,
                next_agent: 0,
            })),
            manager,
            running: HashMap::new(),
        })
    }

    /// The worktree manager, without taking the substrate lock (see the field
    /// docs for why that is sound).
    pub fn manager(&self) -> &WorktreeManager {
        &self.manager
    }

    /// The binding table, under a briefly-held guard. See [`BindingsRef`].
    ///
    /// # Panics
    ///
    /// If the substrate mutex is poisoned — a concurrent cycle panicked
    /// mid-saga. This accessor is a diagnostic read with no `Result` to carry
    /// the fact, so it says so loudly rather than reading rows whose
    /// relationship to disk is unknown. Every path that can act on the failure
    /// (`begin`, `answer`, `spawn_one_cycle`, `abandon`) returns it typed
    /// instead.
    pub fn bindings(&self) -> BindingsRef<'_> {
        BindingsRef(self.substrate.lock().unwrap_or_else(|_| {
            panic!("{}", poisoned_storage());
        }))
    }

    /// A clone of the shared substrate handle — what a cycle thread carries.
    pub fn substrate(&self) -> Arc<Mutex<SpawnSubstrate>> {
        Arc::clone(&self.substrate)
    }

    /// Mint the next in-process agent identity. PROVISIONAL: not durable
    /// across restarts.
    ///
    /// # Panics
    ///
    /// If the substrate mutex is poisoned — same reasoning as
    /// [`bindings`](Self::bindings).
    pub fn mint_agent_id(&mut self) -> AgentId {
        self.substrate
            .lock()
            .unwrap_or_else(|_| panic!("{}", poisoned_storage()))
            .mint_agent_id()
    }

    /// Every agent this spawner is currently stepping, sorted.
    ///
    /// Replaces the old `running_agent() -> Option<AgentId>`, which had no
    /// meaning once more than one cycle could be in flight.
    pub fn running_agents(&self) -> Vec<AgentId> {
        let mut agents: Vec<AgentId> = self.running.keys().copied().collect();
        agents.sort_unstable();
        agents
    }

    /// What a [`SpawnError::NotRunning`] says when the addressed agent is not
    /// in the map.
    ///
    /// The EMPTY case is `"no agent is mid-turn"` verbatim: that exact string
    /// is asserted literally by
    /// `handler_resume_with_no_agent_running_is_a_drive_failure` in
    /// `tidepool-handlers`. The non-empty cases name who IS running, because a
    /// misroute a caller cannot act on is not an explicit failure — and with N
    /// cycles in flight "which ones" is the fact that makes it actionable.
    fn no_such_agent_detail(&self) -> String {
        match self.running_agents().as_slice() {
            [] => "no agent is mid-turn".to_string(),
            [one] => format!("agent {} is the one mid-turn", one.0),
            many => format!(
                "agents mid-turn are {}",
                many.iter()
                    .map(|a| format!("agent {}", a.0))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// BEGIN the coupled-spawn saga and drive the turn to its first stop:
    /// workspace, binding, thread (carrying the declared tools), turn start —
    /// then either a parked tool call or a finished cycle.
    ///
    /// A thin wrapper over [`CycleSaga::begin`] that keeps the saga in this
    /// spawner's map so [`answer`](Self::answer) can find it by agent id.
    ///
    /// See the module docs for the stage diagram and rollback semantics. Every
    /// error returned here has already rolled back.
    pub fn begin(
        &mut self,
        backend: &mut dyn AgentBackend,
        request: &SpawnRequest,
    ) -> Result<SpawnStep, SpawnError> {
        let (saga, step) = CycleSaga::begin(&self.substrate, backend, request)?;
        if !saga.is_finished() {
            self.running.insert(saga.agent(), saga);
        }
        Ok(step)
    }

    /// BEGIN a saga and hand it to the caller instead of storing it.
    ///
    /// This is what a cycle thread uses: the saga carries its own substrate
    /// handle, so it can be moved across a thread boundary and driven there
    /// while this spawner keeps serving other cycles.
    ///
    /// `&self` rather than `&mut self` — a detached saga is never entered into
    /// the inline map, so nothing here mutates.
    pub fn begin_detached(
        &self,
        backend: &mut dyn AgentBackend,
        request: &SpawnRequest,
    ) -> Result<(CycleSaga, SpawnStep), SpawnError> {
        CycleSaga::begin(&self.substrate, backend, request)
    }

    /// Answer the parked tool call for `agent` and drive that cycle on.
    ///
    /// Routing is by agent id: with N cycles in flight, answering the wrong
    /// agent must reach no backend at all, which is what the map lookup and
    /// [`CycleSaga::answer`]'s own checks jointly guarantee. A misroute is a
    /// refusal that costs the addressed cycle nothing — it stays parked and
    /// answerable — and it matters MORE now than it did with one cycle, not
    /// less.
    pub fn answer(
        &mut self,
        backend: &mut dyn AgentBackend,
        agent: AgentId,
        call: ToolCallId,
        outcome: ToolOutcome,
    ) -> Result<SpawnStep, SpawnError> {
        if !self.running.contains_key(&agent) {
            return Err(SpawnError::NotRunning {
                agent,
                detail: self.no_such_agent_detail(),
            });
        }
        let saga = self.running.get_mut(&agent).expect("checked just above");
        let result = saga.answer(backend, agent, call, outcome);
        // A saga whose life ended — completed, rolled back, or backstopped —
        // leaves the map; a refused misroute costs the running agent nothing
        // and it stays.
        if saga.is_finished() {
            self.running.remove(&agent);
        }
        result
    }

    /// The whole saga behind ONE call, refusing every tool call the child
    /// makes.
    ///
    /// A combinator over [`CycleSaga::begin`] + [`CycleSaga::run_to_completion`],
    /// holding no map entry: nothing can step this cycle from outside, so
    /// nothing needs to find it.
    pub fn spawn_one_cycle(
        &mut self,
        backend: &mut dyn AgentBackend,
        request: &SpawnRequest,
    ) -> Result<OneCycleRun, SpawnError> {
        let (mut saga, step) = CycleSaga::begin(&self.substrate, backend, request)?;
        saga.run_to_completion(backend, step)
    }

    /// Settle the current binding for `worktree` (rollback / completion
    /// helper). Exposed so tests can drive edge cases directly.
    pub fn settle(
        &mut self,
        worktree: &WorktreeId,
        state: BindingState,
    ) -> Result<(), WorktreeError> {
        self.substrate
            .lock()
            .map_err(|_| poisoned_storage())?
            .settle(worktree, state)
    }
}
