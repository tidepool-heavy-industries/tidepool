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

use tidepool_worktree::{
    AgentRef, BindingState, BindingTable, WorktreeError, WorktreeHandle, WorktreeId,
    WorktreeManager, WorktreeSpec,
};

use crate::backend::AgentBackend;
use crate::seam::{
    AgentBackendError, AgentId, BackendThreadId, CycleResultPayload, CycleSpec,
    DynamicToolDeclaration, ModelPolicy, ReasoningEffort, ThreadSpec, TokenUsage, ToolCall,
    ToolCallId, ToolOutcome, ToolReply, TurnEvent, TurnId,
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
/// to `[A-Za-z0-9._-]` with runs of `-` collapsed and the ends trimmed. An
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
    pub fn cycle_spec(&self, cwd: String) -> CycleSpec {
        CycleSpec {
            cwd,
            task: self.task.clone(),
            output_schema: self.output_schema.clone(),
            model: self.model,
            effort: self.effort,
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

/// Owns the worktree substrate handles a coupled spawn needs. One spawner per
/// (registry root, binding root) — `BindingTable`'s lifetime flock enforces
/// the single-owner precondition, so constructing a second spawner over the
/// same binding root fails loudly at `open`.
pub struct CoupledSpawner {
    manager: WorktreeManager,
    bindings: BindingTable,
    next_agent: u64,
    /// The agent currently mid-turn, if any.
    ///
    /// ONE at a time, deliberately: a spawner that silently supported two
    /// would make an untested concurrent case reachable. A second `begin`
    /// while one is running is a loud failure, not a queue.
    running: Option<RunningAgent>,
}

/// A spawn that has begun and not yet finished: the coupled pair, the binding
/// it holds, and the call it is parked on.
#[derive(Debug, Clone)]
struct RunningAgent {
    agent: AgentId,
    worktree: WorktreeHandle,
    thread: BackendThreadId,
    binding_ref: String,
    /// The call awaiting an answer. `None` between a completed step and the
    /// next — which cannot be observed by a caller, since every step either
    /// parks or finishes.
    parked: Option<ToolCallId>,
    rounds: u32,
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
            running: None,
        })
    }

    pub fn manager(&self) -> &WorktreeManager {
        &self.manager
    }

    pub fn bindings(&self) -> &BindingTable {
        &self.bindings
    }

    /// Mint the next in-process agent identity. PROVISIONAL: not durable
    /// across restarts.
    pub fn mint_agent_id(&mut self) -> AgentId {
        let id = AgentId(self.next_agent);
        self.next_agent += 1;
        id
    }

    /// The agent currently mid-turn, if any.
    pub fn running_agent(&self) -> Option<AgentId> {
        self.running.as_ref().map(|r| r.agent)
    }

    /// BEGIN the coupled-spawn saga and drive the turn to its first stop:
    /// workspace, binding, thread (carrying the declared tools), turn start —
    /// then either a parked tool call or a finished cycle.
    ///
    /// See the module docs for the stage diagram and rollback semantics. Every
    /// error returned here has already rolled back.
    pub fn begin(
        &mut self,
        backend: &mut dyn AgentBackend,
        request: &SpawnRequest,
    ) -> Result<SpawnStep, SpawnError> {
        if let Some(running) = &self.running {
            return Err(SpawnError::NotRunning {
                agent: running.agent,
                detail: format!(
                    "agent {} is still mid-turn; this spawner drives one agent at a time",
                    running.agent.0
                ),
            });
        }

        // 1. Allocating → WorktreeReady. Nothing is bound yet, so a failure
        //    here has nothing to compensate: no binding row is ever written.
        let worktree = self.resolve_workspace(&request.workspace)?;

        // 2. Identity: mint the agent id, then sanitize the label into the
        //    `AgentRef` tail.
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
        let event = match backend.start_turn(&thread, &request.cycle_spec(cwd)) {
            Ok(event) => event,
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

        self.running = Some(RunningAgent {
            agent,
            worktree,
            thread,
            binding_ref,
            parked: None,
            rounds: 0,
        });
        self.settle_step(event)
    }

    /// Answer the parked tool call and drive on to the next stop.
    ///
    /// `agent` is checked against the running agent before anything is sent:
    /// answering the wrong agent is the misroute the correlation triple exists
    /// to catch, and catching it here costs nothing.
    pub fn answer(
        &mut self,
        backend: &mut dyn AgentBackend,
        agent: AgentId,
        call: ToolCallId,
        outcome: ToolOutcome,
    ) -> Result<SpawnStep, SpawnError> {
        let Some(running) = &mut self.running else {
            return Err(SpawnError::NotRunning {
                agent,
                detail: "no agent is mid-turn".to_string(),
            });
        };
        if running.agent != agent {
            return Err(SpawnError::NotRunning {
                agent,
                detail: format!("agent {} is the one mid-turn", running.agent.0),
            });
        }
        match &running.parked {
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

        running.rounds += 1;
        if running.rounds > MAX_TOOL_ROUNDS {
            let worktree = running.worktree.id().clone();
            let error = SpawnError::RoundBackstop {
                agent,
                limit: MAX_TOOL_ROUNDS,
            };
            self.running = None;
            return Err(self.roll_back(&worktree, error));
        }
        running.parked = None;

        let event = match backend.resume(ToolReply { call, outcome }) {
            Ok(event) => event,
            Err(error) => {
                let worktree = self
                    .running
                    .as_ref()
                    .expect("running checked above")
                    .worktree
                    .id()
                    .clone();
                self.running = None;
                return Err(self.roll_back(
                    &worktree,
                    SpawnError::Backend {
                        stage: SpawnStage::Running,
                        error,
                    },
                ));
            }
        };
        self.settle_step(event)
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
        let running = self
            .running
            .as_mut()
            .expect("settle_step is only reachable with an agent running");
        match event {
            TurnEvent::ToolCall(call) => {
                running.parked = Some(call.call.clone());
                Ok(SpawnStep::ToolCall {
                    agent: running.agent,
                    call,
                })
            }
            TurnEvent::Completed(outcome) => {
                let finished = self.running.take().expect("checked just above");
                if let Err(rollback) = self
                    .bindings
                    .settle(finished.worktree.id(), BindingState::Terminal)
                {
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
                    agent: finished.agent,
                    worktree: finished.worktree.id().clone(),
                    binding_ref: finished.binding_ref,
                    thread: finished.thread.clone(),
                    resolved_model: outcome.resolved_model,
                    turn: outcome.turn,
                    rounds: finished.rounds,
                    usage: outcome.usage,
                };
                Ok(SpawnStep::Done(Box::new(OneCycleRun {
                    run: WorkerRun {
                        agent: finished.agent,
                        worktree: finished.worktree,
                        thread: finished.thread,
                    },
                    payload: outcome.payload,
                    receipt,
                    activity: outcome.activity,
                })))
            }
        }
    }

    /// The whole saga behind ONE call, refusing every tool call the child
    /// makes.
    ///
    /// The no-tools path: a request carrying no declarations should produce
    /// no calls, and one that arrives anyway is refused rather than left
    /// parked.
    pub fn spawn_one_cycle(
        &mut self,
        backend: &mut dyn AgentBackend,
        request: &SpawnRequest,
    ) -> Result<OneCycleRun, SpawnError> {
        let mut step = self.begin(backend, request)?;
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
