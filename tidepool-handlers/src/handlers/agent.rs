use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;

use tidepool_agent::backend::{AgentBackend, AgentBackendFactory, BackendCanceller};
use tidepool_agent::seam::{
    AgentActivity, AgentBackendError, AgentId, BackendThreadId, CycleResultPayload,
    DynamicToolDeclaration, ModelPolicy, ReasoningEffort, TokenUsage, ToolCallId, ToolOutcome,
};
use tidepool_agent::spawn::{
    AnswerFailure, CoupledSpawner, CycleProgress, CycleSaga, OneCycleRun, ParkedCycle,
    SpawnError as DomainSpawnError, SpawnReceipt, SpawnRequest, SpawnStage, SpawnStep,
    SpawnWorkspace, WorkerRun,
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
    AgAgentActivity, AgAgentId, AgAgentStep, AgBackendFailure, AgBackendThreadId, AgCycleId,
    AgCyclePayload, AgSpawnOutcome, AgSpawnReceipt, AgSpawnSpec, AgSpawnStage, AgSpawnWorkspace,
    AgTokenUsage, AgWorkerRun,
};

/// Reject a non-UTF-8 root once, at construction, rather than letting a
/// `to_string_lossy()` mangle it silently wherever it's later joined into
/// text that crosses to a spawned backend or to Haskell.
fn require_utf8(path: &std::path::Path) -> Result<(), DomainWorktreeError> {
    if camino::Utf8Path::from_path(path).is_none() {
        return Err(DomainWorktreeError::StorageFailure {
            path: path.to_path_buf(),
            detail: "must be valid UTF-8".to_string(),
        });
    }
    Ok(())
}

/// Confirm `backend`'s process is reaped before it drops — the NORMAL
/// (non-cancelled) teardown path. Every production site that is about to let
/// a cycle's backend go out of scope calls this first, in place of the
/// implicit `Drop` (fire-and-forget kill, no confirmation) that would
/// otherwise run.
///
/// A shutdown failure is logged, never propagated: the cycle's own
/// result — already computed by the time this runs — is the caller's real
/// answer, and a teardown hiccup must not mask it. `Drop` remains the
/// backstop for whatever this call could not confirm.
fn shutdown_backend(backend: Box<dyn AgentBackend + Send>) {
    if let Err(e) = backend.shutdown() {
        tracing::warn!("backend shutdown did not confirm a clean reap: {e}");
    }
}

// ============================================================================
// Delegate transcript persistence.
//
// Opt-in, operator-owned: `transcript_jsonl()` is read at every point a
// cycle's backend is about to be dropped for good (a terminal Done/RolledBack
// settle, or an async cycle's report arriving), and this is where those
// frames land on disk so a delegate's full turn history (including reasoning
// frames) survives past the cycle that produced it.
// ============================================================================

/// Writes one cycle's transcript frames to `delegate-<cycle_tag>.jsonl` under
/// a configured directory — or does nothing when unconfigured.
///
/// Constructed ONCE, at handler construction, from
/// `TIDEPOOL_DELEGATE_TRANSCRIPT_DIR`: unset means `dir: None`, which makes
/// every [`persist`](Self::persist) call an immediate no-op — zero behavior
/// change for every wiring that never opts in. IO failure is a `tracing::warn!`,
/// never a cycle failure: a delegate's own report already succeeded or failed
/// on its own terms by the time this runs, and a full disk must not turn that
/// into a second, unrelated failure.
struct DelegateTranscriptSink {
    dir: Option<PathBuf>,
}

impl DelegateTranscriptSink {
    fn from_env() -> Self {
        Self {
            dir: std::env::var_os("TIDEPOOL_DELEGATE_TRANSCRIPT_DIR").map(PathBuf::from),
        }
    }

    /// Persist `frames` for `cycle_tag`, one frame per line. A no-op when
    /// unconfigured. A name COLLISION (two cycles across separate handler
    /// lifetimes sharing a tag, since [`CycleId`] is minted from 0 each time)
    /// never overwrites: the write uses `create_new` and retries with a
    /// numeric suffix, so the file that got there first is never clobbered.
    fn persist(&self, cycle_tag: &str, frames: &[String]) {
        let Some(dir) = &self.dir else {
            return;
        };
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(
                "delegate transcript sink: could not create directory {}: {e}",
                dir.display()
            );
            return;
        }
        let mut contents = frames.join("\n");
        if !contents.is_empty() {
            contents.push('\n');
        }
        let mut suffix: u32 = 0;
        loop {
            let path = if suffix == 0 {
                dir.join(format!("delegate-{cycle_tag}.jsonl"))
            } else {
                dir.join(format!("delegate-{cycle_tag}-{suffix}.jsonl"))
            };
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    if let Err(e) = file.write_all(contents.as_bytes()) {
                        tracing::warn!(
                            "delegate transcript sink: could not write {}: {e}",
                            path.display()
                        );
                    }
                    return;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    suffix += 1;
                    if suffix > 10_000 {
                        tracing::warn!(
                            "delegate transcript sink: giving up finding a free name for cycle \
                             {cycle_tag} under {} after {suffix} collisions",
                            dir.display()
                        );
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "delegate transcript sink: could not create {}: {e}",
                        path.display()
                    );
                    return;
                }
            }
        }
    }
}

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

// ============================================================================
// The cycle table.
//
// One entry per cycle this handler admitted, keyed by a `CycleId` it minted.
// Two shapes, because a cycle is driven in exactly two ways: STEPPED (one stop
// at a time, inline on the caller's thread, because the tool-dispatch loop
// lives in Haskell) or ASYNC (run to completion on its own thread, then
// awaited or cancelled).
// ============================================================================

/// The handler-scoped identity of one cycle — what an authored `AgentHandle`
/// wraps, and what `SubagentAwait`/`SubagentCancel` name.
///
/// Minted monotonically from 0 and NEVER reused, including after a cycle is
/// reaped: a terminal entry keeps its id, so an await on a cycle that already
/// finished answers for that cycle rather than for whatever was admitted next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CycleId(u64);

/// The tag a [`DelegateTranscriptSink`] files this cycle's transcript under.
/// Monotonic within one handler's lifetime — "timestamp-ish ordering" for
/// free — but NOT globally unique across separate handler lifetimes (the
/// counter always starts at 0), which is exactly the collision
/// [`DelegateTranscriptSink::persist`] is built to survive.
fn cycle_tag(id: CycleId) -> String {
    id.0.to_string()
}

/// A stepped cycle's own drive state: mid-turn (holding the saga PARKED on
/// its current call, inseparably, plus the live backend that drives it) or
/// SETTLED (holding only the extracted transcript).
///
/// This is a sum, not two independently-mutable fields, on purpose: the old
/// shape (`saga: Box<CycleSaga>` next to a separately-toggled `StepBackend`)
/// let a maintenance change advance one without the other, papered over by a
/// `settle_if_finished` call that had to be remembered at every call site and
/// an `unreachable!()` for the cross-product nobody could then rule out by
/// construction. Here a `Running` entry cannot lack a backend, a `Settled`
/// entry cannot yield one, and the transition ([`Self::from_progress`]) is
/// the ONLY place a `Running` becomes a `Settled` — same idiom as
/// [`AsyncCycle`]'s `Running`/`Settled`.
///
/// A terminal table entry must hold no OS process — [`SubagentHandler::drop`]
/// only reaps ASYNC cycles, and a finished `Stepped` entry that kept its
/// backend alive (a live app-server child + its tokio runtime) forever would
/// be exactly the leak that guards against.
enum SteppedCycle {
    Running {
        /// The saga, inseparable from the call it is parked on
        /// ([`tidepool_agent::spawn::CycleProgress`]) — the same bundling
        /// that closes the cross-cycle SpawnStep/CycleSaga mismatch in
        /// `tidepool-agent`. Boxed for the same reason `Cycle::Stepped`
        /// itself is: no `SteppedCycle` value should pay for the largest
        /// variant's inline size, including transiently before it's boxed.
        parked: Box<ParkedCycle>,
        backend: Box<dyn AgentBackend + Send>,
    },
    Settled {
        /// Extracted from the backend the instant the saga finished; that
        /// backend has already been dropped.
        transcript: Vec<String>,
    },
}

impl SteppedCycle {
    /// Turn a freshly-driven [`CycleProgress`] into the next stepped-cycle
    /// state and the wire step to hand back — the ONLY constructor, so
    /// `Running`/`Settled` and their backend are always built together from
    /// the same drive.
    fn from_progress(
        progress: CycleProgress,
        backend: Box<dyn AgentBackend + Send>,
        id: CycleId,
        transcripts: &DelegateTranscriptSink,
    ) -> (Self, AgAgentStep) {
        match progress {
            CycleProgress::Done(run) => {
                let wire = step_to_wire(&SpawnStep::Done(run));
                // The backend is torn down here, in place of being kept: a
                // terminal entry must hold no OS process.
                let transcript = backend.transcript_jsonl();
                transcripts.persist(&cycle_tag(id), &transcript);
                shutdown_backend(backend);
                (SteppedCycle::Settled { transcript }, wire)
            }
            CycleProgress::Parked(parked) => {
                let step = SpawnStep::ToolCall {
                    agent: parked.agent(),
                    call: parked.call().clone(),
                };
                let wire = step_to_wire(&step);
                (SteppedCycle::Running { parked, backend }, wire)
            }
        }
    }

    /// The agent mid-turn, `None` once settled.
    fn agent(&self) -> Option<AgentId> {
        match self {
            SteppedCycle::Running { parked, .. } => Some(parked.agent()),
            SteppedCycle::Settled { .. } => None,
        }
    }

    fn transcript_jsonl(&self) -> Vec<String> {
        match self {
            SteppedCycle::Running { backend, .. } => backend.transcript_jsonl(),
            SteppedCycle::Settled { transcript } => transcript.clone(),
        }
    }
}

/// One cycle in the table.
enum Cycle {
    /// Driven one stop at a time by `SubagentBegin`/`SubagentResume`, inline on
    /// the caller's thread — the tool-dispatch path, whose loop lives in
    /// Haskell (`tidepool-agent/CLAUDE.md`, "the seam is a STEP function").
    /// Boxed so that no table entry pays for the largest variant's inline
    /// size — same reason as [`Cycle::Async`].
    Stepped(Box<SteppedCycle>),
    /// Driven to completion on its OWN thread — the async path. Awaited or
    /// cancelled; never stepped. Boxed: an [`AsyncCycle`] is several times the
    /// size of a stepped entry, and every table entry would otherwise pay for
    /// the larger of the two.
    Async(Box<AsyncCycle>),
}

impl Cycle {
    /// Whether this entry still counts against the capacity bound. A terminal
    /// entry is RETAINED (its transcript stays readable, and an await on it
    /// stays distinguishable from an await on a typo'd id) but occupies no
    /// slot.
    fn is_terminal(&self) -> bool {
        match self {
            Cycle::Stepped(stepped) => matches!(**stepped, SteppedCycle::Settled { .. }),
            Cycle::Async(cycle) => cycle.settled_terminal().is_some(),
        }
    }
}

/// What a cycle thread hands back when its saga reaches a terminal.
///
/// It carries a SAGA slot alongside the result and the BACKEND: `cancel`
/// settles the binding through the saga (which lives on the cycle thread
/// while the cycle runs) if a report races it with an unsettled one, and the
/// backend's transcript has to stay reachable from the handler after the
/// thread is gone.
struct CycleReport {
    result: Result<OneCycleRun, DomainSpawnError>,
    /// ALWAYS `None`: `CycleProgress::run_to_completion` settles the saga on
    /// every exit path (completion, or its own rollback on failure) before
    /// this thread ever gets a saga back to report, and a failure inside
    /// `CycleSaga::begin` never produced one to begin with. Kept as a slot
    /// (rather than dropped) so [`AsyncCycle::cancel`]'s defensive
    /// re-`abandon` — the one thing this field feeds — stays the same shape
    /// as the async idiom it mirrors.
    saga: Option<CycleSaga>,
    backend: Box<dyn AgentBackend + Send>,
}

/// What an async cycle finally settled to, MEMOIZED.
///
/// A second await answers the same thing rather than blocking forever on a
/// receiver whose sender is gone, and a cancelled cycle keeps a terminal that
/// says "cancelled" rather than vanishing from the table.
enum Settled {
    /// The cycle thread ran to a terminal and reported it. Boxed for the same
    /// reason [`Cycle::Async`] is: a whole `OneCycleRun` next to two small
    /// variants.
    Reported(Box<Result<OneCycleRun, DomainSpawnError>>),
    /// [`SubagentCancel`](SubagentHandler::subagent_cancel) reaped it first.
    Cancelled,
    /// The cycle produced no usable terminal: its thread died without
    /// reporting, or its cancellation could not settle the binding. Both are
    /// loud facts with no other channel to surface on — `cancel` is total by
    /// contract — so they surface on the next await.
    Lost(String),
}

impl Settled {
    fn to_wire(&self, id: CycleId) -> Result<AgSpawnOutcome, SpawnError> {
        match self {
            Settled::Reported(reported) => match &**reported {
                Ok(run) => Ok(outcome_to_wire(run)),
                Err(e) => Err(spawn_error_to_wire(e.clone())),
            },
            Settled::Cancelled => Err(SpawnError::SpawnCancelled(cycle_id_to_wire(id))),
            Settled::Lost(detail) => Err(SpawnError::SpawnDriveFailed(
                AgSpawnStage::StageRunning,
                detail.clone(),
            )),
        }
    }
}

/// What an [`AsyncCycle`] says when its thread died without ever reporting —
/// the shared wording between the blocking [`AsyncCycle::collect`] and the
/// non-blocking [`AsyncCycle::try_settle`].
fn lost_no_report(id: CycleId) -> Settled {
    Settled::Lost(format!(
        "cycle {} ended without reporting a result: its thread panicked mid-saga, so whether \
         its binding was settled is unknown",
        id.0
    ))
}

/// A cycle running on its own thread.
///
/// A plain sum rather than three `Option`s (`thread`/`saga`/`settled`) on one
/// product: the old shape let `settled: Some(_)` coexist with an unjoined
/// thread, or a joined thread with no terminal stored — combinations that
/// meant nothing, reachable only by a maintenance change getting the manual
/// bookkeeping wrong. `Running` owns exactly the resources live while the
/// thread runs; `Settled` owns exactly what survives it. The transitions
/// ([`Self::collect`]/[`Self::try_settle`]/[`Self::cancel`]) are CONSUMING —
/// `self -> Self` — so a caller holding a `Running` cannot observe a
/// half-transitioned state, and the compiler (not a convention) is what
/// stops a `Drop` from reaping something already `Settled`.
enum AsyncCycle {
    Running {
        thread: JoinHandle<()>,
        reports: Receiver<CycleReport>,
        /// Taken from the backend BEFORE the cycle thread started — once that
        /// thread is inside `start_turn` it holds `&mut` on the backend and
        /// nothing else can reach it (`AgentBackend::canceller`'s own docs).
        canceller: Box<dyn BackendCanceller>,
    },
    Settled {
        /// MEMOIZED: a second await answers the same thing rather than
        /// blocking forever on a receiver whose sender is gone, and a
        /// cancelled cycle keeps a terminal that says "cancelled" rather than
        /// vanishing from the table.
        terminal: Settled,
        /// The backend's own transcript, extracted the instant its report
        /// arrived — a settled cycle holds NO backend (and so no OS process),
        /// only what it said.
        transcript: Vec<String>,
    },
}

impl AsyncCycle {
    /// Extract an arrived report's transcript (dropping the backend that
    /// produced it — it goes out of scope at the end of this call) and its
    /// saga (the caller's — only [`cancel`](Self::cancel) needs it, to settle
    /// the binding through it; a report [`collect`](Self::collect) absorbed
    /// has nothing left to do with it). The shared tail of
    /// [`collect`](Self::collect) (which blocks for the report) and
    /// [`try_settle`](Self::try_settle) (which only acts on one that already
    /// arrived).
    fn settle_with(
        report: CycleReport,
        id: CycleId,
        transcripts: &DelegateTranscriptSink,
    ) -> (Settled, Option<CycleSaga>, Vec<String>) {
        let transcript = report.backend.transcript_jsonl();
        transcripts.persist(&cycle_tag(id), &transcript);
        shutdown_backend(report.backend);
        (
            Settled::Reported(Box::new(report.result)),
            report.saga,
            transcript,
        )
    }

    /// Block for `reports`, join `thread`, and produce the report's
    /// `Settled` alongside its saga and transcript. Never panics: a thread
    /// that died mid-saga is a [`Settled::Lost`], not a second panic here.
    /// Shared tail of [`collect`](Self::collect) and [`cancel`](Self::cancel)
    /// once nothing more needs the canceller.
    fn recv_and_join(
        thread: JoinHandle<()>,
        reports: Receiver<CycleReport>,
        id: CycleId,
        transcripts: &DelegateTranscriptSink,
    ) -> (Settled, Option<CycleSaga>, Vec<String>) {
        let out = match reports.recv() {
            Ok(report) => Self::settle_with(report, id, transcripts),
            Err(_) => (lost_no_report(id), None, Vec::new()),
        };
        // A panicked cycle thread is already accounted for above; joining it
        // is how its resources are released, not how it is diagnosed.
        let _ = thread.join();
        out
    }

    /// Block for the cycle thread's report, join the thread, and become
    /// `Settled`. A no-op — returns `self` unchanged — on a cycle already
    /// `Settled`.
    fn collect(self, id: CycleId, transcripts: &DelegateTranscriptSink) -> Self {
        match self {
            settled @ AsyncCycle::Settled { .. } => settled,
            AsyncCycle::Running {
                thread, reports, ..
            } => {
                let (terminal, _saga, transcript) =
                    Self::recv_and_join(thread, reports, id, transcripts);
                AsyncCycle::Settled {
                    terminal,
                    transcript,
                }
            }
        }
    }

    /// Non-blocking: if this cycle's thread has already sent its report,
    /// settle it now. This is what lets a completed-but-never-awaited cycle
    /// free its capacity slot on the NEXT admission — see
    /// [`SubagentHandler::admit_cycle`] — without a caller ever calling
    /// `awaitAgent`/`cancelAgent`. A no-op — stays `Running` — while the
    /// thread has not yet reported (never blocks), and a no-op on a cycle
    /// already `Settled` — so it changes nothing about what a LATER
    /// `await`/`cancel` observes: same typed terminal either way, just
    /// possibly settled earlier.
    fn try_settle(self, id: CycleId, transcripts: &DelegateTranscriptSink) -> Self {
        match self {
            settled @ AsyncCycle::Settled { .. } => settled,
            AsyncCycle::Running {
                thread,
                reports,
                canceller,
            } => {
                use std::sync::mpsc::TryRecvError;
                match reports.try_recv() {
                    Ok(report) => {
                        let (terminal, _saga, transcript) =
                            Self::settle_with(report, id, transcripts);
                        let _ = thread.join();
                        AsyncCycle::Settled {
                            terminal,
                            transcript,
                        }
                    }
                    Err(TryRecvError::Empty) => AsyncCycle::Running {
                        thread,
                        reports,
                        canceller,
                    },
                    Err(TryRecvError::Disconnected) => {
                        let _ = thread.join();
                        AsyncCycle::Settled {
                            terminal: lost_no_report(id),
                            transcript: Vec::new(),
                        }
                    }
                }
            }
        }
    }

    /// Reap this cycle: kill FIRST, then settle. A no-op — returns `self`
    /// unchanged — on a cycle already `Settled`.
    ///
    /// Order is the rule from `tidepool-agent/CLAUDE.md`: settling before the
    /// kill would hold the substrate mutex across a reap of unknown duration,
    /// which is the one thing the concurrent saga design forbids.
    fn cancel(self, id: CycleId, transcripts: &DelegateTranscriptSink) -> Self {
        match self {
            settled @ AsyncCycle::Settled { .. } => settled,
            AsyncCycle::Running {
                thread,
                reports,
                canceller,
            } => {
                canceller.cancel();
                // The thread's blocked seam call returns, its saga rolls
                // itself back, and the report arrives. Dropping the reported
                // result is deliberate: an author who cancelled gets
                // `SpawnCancelled` whether or not the cycle happened to
                // finish first, so the answer never depends on a race.
                let (reported, mut saga, transcript) =
                    Self::recv_and_join(thread, reports, id, transcripts);
                let terminal = match saga.as_mut().map(CycleSaga::abandon) {
                    Some(Err(e)) => Settled::Lost(format!(
                        "cycle {} was cancelled and its backend reaped, but settling its binding \
                         Released FAILED: {e} — the binding row may still be Active",
                        id.0
                    )),
                    // A thread that died without reporting left its binding in
                    // an unknown state; answering "cancelled" would claim a
                    // settle nobody performed, so that diagnosis survives the
                    // cancel.
                    _ => match reported {
                        lost @ Settled::Lost(_) => lost,
                        _ => Settled::Cancelled,
                    },
                };
                AsyncCycle::Settled {
                    terminal,
                    transcript,
                }
            }
        }
    }

    /// The memoized terminal, once `Settled` — `None` while still `Running`.
    fn settled_terminal(&self) -> Option<&Settled> {
        match self {
            AsyncCycle::Settled { terminal, .. } => Some(terminal),
            AsyncCycle::Running { .. } => None,
        }
    }

    fn transcript_jsonl(&self) -> Vec<String> {
        match self {
            AsyncCycle::Running { .. } => Vec::new(),
            AsyncCycle::Settled { transcript, .. } => transcript.clone(),
        }
    }
}

/// Serves `SubagentSpawn` (the whole saga behind one verb), the async trio
/// `SubagentSpawnAsync`/`SubagentAwait`/`SubagentCancel` (the same saga
/// detached onto its own thread), and `SubagentBegin`/`SubagentResume` (the
/// same saga driven one stop at a time, for an agent that holds dynamic tools).
///
/// Owns the worktree substrate handles (via [`CoupledSpawner`] — whose
/// `BindingTable` holds the single-owner lifetime flock for its binding root)
/// and a CYCLE TABLE: N cycles, each with its OWN backend, bounded by
/// [`with_cycle_capacity`](Self::with_cycle_capacity). Production wires the
/// codex adapter; every committed test wires
/// [`tidepool_agent::backend::mock::MockBackend`] — no live-model turns in
/// tests, ever.
///
/// **A parked turn lives exactly as long as this handler does.** Between a
/// `StepToolCall` and its `SubagentResume` the child's request is parked with
/// no response written, so if the eval driving the loop dies mid-dispatch the
/// call is never answered and the child's turn hangs until its own timeout.
/// The mitigation is ownership, not a protocol trick: this handler owns every
/// cycle's backend, and dropping it takes the app-server processes with it —
/// which is why [`Drop`](Self::drop) reaps the async cycles rather than
/// orphaning their threads.
///
/// Not `Clone`, deliberately (RepoEventHandler precedent): it owns the cycle
/// table and a flocked binding table, neither of which has a meaningful second
/// owner.
pub struct SubagentHandler {
    spawner: CoupledSpawner,
    /// One backend per cycle. Concurrent cycles never share one: an
    /// `AgentBackend` is a step function over ONE live thread, and two cycles
    /// sharing one would interleave their replies onto the same session.
    backends: Box<dyn AgentBackendFactory>,
    cycles: BTreeMap<CycleId, Cycle>,
    next_cycle: u64,
    /// How many NON-TERMINAL cycles may exist at once. A BOUND, not a queue:
    /// see [`with_cycle_capacity`](Self::with_cycle_capacity).
    capacity: usize,
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
    /// Where each cycle's transcript frames land once it reaches a terminal —
    /// `TIDEPOOL_DELEGATE_TRANSCRIPT_DIR`, read once here at construction, or
    /// nothing when unset.
    transcripts: DelegateTranscriptSink,
}

/// The default cycle-table bound: how many cycles may be non-terminal at once.
///
/// A ceiling an operator can see, not a tuned number — concurrency here is
/// bounded by real model spend, so the default is deliberately small and the
/// wiring raises it explicitly.
const DEFAULT_CYCLE_CAPACITY: usize = 8;

/// A factory over ONE pre-built backend: it yields that instance for the first
/// cycle and fails for every cycle after it.
///
/// This is what keeps [`SubagentHandler::new`]'s signature working now that
/// cycles take a backend each. **Its exhaustion is a fact about the WIRING,
/// not a policy refusal** — a caller that handed over a single backend
/// instance has exactly one, and a second concurrent cycle would need a second
/// one. It must never be read as the one-agent-at-a-time constraint PRD 20
/// S1-L2 deleted: that constraint is gone, and
/// [`SubagentHandler::with_backends`] is how a wiring gets N cycles.
struct OneShotBackend(Option<Box<dyn AgentBackend + Send>>);

impl AgentBackendFactory for OneShotBackend {
    fn create(&mut self) -> Result<Box<dyn AgentBackend + Send>, AgentBackendError> {
        self.0
            .take()
            .ok_or_else(|| AgentBackendError::BackendUnavailable {
                detail: "this handler was wired with ONE pre-built backend \
                         (SubagentHandler::new) and it is already in use — a second cycle needs \
                         a second backend. Wire SubagentHandler::with_backends(.., factory) to \
                         run cycles concurrently. This is a fact about the WIRING, not a refusal \
                         to run concurrent agents."
                    .to_string(),
            })
    }
}

impl SubagentHandler {
    /// `registry_root`, `worktree_root`, and `binding_root` must live OUTSIDE
    /// `source_repository` — the never-dirty-the-source rule
    /// (`tidepool-worktree/CLAUDE.md`). Fallible: opening the registry and the
    /// binding table both are, and the binding table refuses a root another
    /// live table owns.
    ///
    /// The single `backend` becomes a ONE-SHOT factory ([`OneShotBackend`]):
    /// this handler can run exactly one cycle at a time, because one backend
    /// instance is all it was given. Use [`with_backends`](Self::with_backends)
    /// to run N.
    pub fn new(
        registry_root: PathBuf,
        worktree_root: PathBuf,
        binding_root: PathBuf,
        source_repository: PathBuf,
        backend: Box<dyn AgentBackend + Send>,
    ) -> Result<Self, DomainWorktreeError> {
        Self::with_backends(
            registry_root,
            worktree_root,
            binding_root,
            source_repository,
            Box::new(OneShotBackend(Some(backend))),
        )
    }

    /// The N-cycle constructor: `backends` makes ONE backend per cycle.
    ///
    /// Same substrate rules as [`new`](Self::new). Every cycle this handler
    /// admits — stepped or async — asks `backends` for its own instance, and a
    /// factory failure is a typed `SpawnBackendFailed` at `StageAllocating`,
    /// where nothing has been allocated.
    pub fn with_backends(
        registry_root: PathBuf,
        worktree_root: PathBuf,
        binding_root: PathBuf,
        source_repository: PathBuf,
        backends: Box<dyn AgentBackendFactory>,
    ) -> Result<Self, DomainWorktreeError> {
        // `worktree.cwd()` and `source_repository().join(".git")` both cross
        // into the spawned backend's `CycleSpec` as plain text
        // (`tidepool-agent/src/spawn.rs`'s `cwd`/`git_dir`). Decode ONCE here,
        // at construction, with a typed error — the locked "decode once at
        // the OS boundary" pattern — instead of letting a non-UTF-8 root
        // silently misconfigure a backend's sandbox.
        require_utf8(&worktree_root)?;
        require_utf8(&source_repository)?;
        let registry = WorktreeRegistry::open(&registry_root)?;
        let manager =
            WorktreeManager::new(GitCli::new(), registry, worktree_root, source_repository);
        Ok(Self {
            spawner: CoupledSpawner::open(manager, binding_root)?,
            backends,
            cycles: BTreeMap::new(),
            next_cycle: 0,
            capacity: DEFAULT_CYCLE_CAPACITY,
            model: ModelPolicy::CheapPlumbing,
            effort: ReasoningEffort::Low,
            transcripts: DelegateTranscriptSink::from_env(),
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

    /// Admit at most `capacity` non-terminal cycles at once.
    ///
    /// A BOUND, not a backlog: a spawn past the cap is refused IMMEDIATELY with
    /// `SpawnCapacityExhausted`, never queued, so an operator sees the ceiling
    /// instead of an invisible backlog forming behind it. Terminal entries stay
    /// in the table but occupy no slot.
    #[must_use]
    pub fn with_cycle_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }

    /// The spawner, for post-run assertions in tests (binding state, registry
    /// lookups) without reopening flocked state.
    pub fn spawner(&self) -> &CoupledSpawner {
        &self.spawner
    }

    /// Every reachable cycle's own transcript, as opaque JSONL lines,
    /// concatenated in cycle order — empty for a backend that keeps none
    /// (every mock). The live acceptance writes this to a fixture so the
    /// recording can drive the production pump in CI afterwards.
    ///
    /// Redefined over the cycle table now that a backend is per-cycle rather
    /// than per-handler. Three consequences worth knowing before reading a
    /// fixture: an async cycle's backend lives on its own thread while it runs,
    /// so its lines appear only after that cycle is reaped (awaited or
    /// cancelled); once reaped, the backend itself is GONE (dropped at settle
    /// — see [`AsyncCycle::absorb`] / [`SteppedCycle::from_progress`]) and only
    /// its extracted transcript remains, which is what this reads for a
    /// terminal entry; and `SubagentSpawn`'s backend belongs to the call rather
    /// than to the table, so a one-call sync spawn contributes nothing here.
    /// The live acceptance drives the tool loop, whose stepped entry the table
    /// retains.
    pub fn backend_transcript_jsonl(&self) -> Vec<String> {
        self.cycles
            .values()
            .flat_map(|cycle| match cycle {
                Cycle::Stepped(stepped) => stepped.transcript_jsonl(),
                Cycle::Async(cycle) => cycle.transcript_jsonl(),
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // Table bookkeeping.
    // ------------------------------------------------------------------

    fn mint_cycle(&mut self) -> CycleId {
        let id = CycleId(self.next_cycle);
        self.next_cycle += 1;
        id
    }

    /// Refuse a cycle that would exceed the bound, BEFORE anything is
    /// allocated: no backend is made, no worktree is created, no binding row is
    /// written for a spawn that is refused.
    ///
    /// Opportunistically settles every async cycle whose thread has already
    /// sent its report but was never `await`ed or `cancel`ed (a non-blocking
    /// `try_recv` per cycle — see [`AsyncCycle::try_settle`]) BEFORE counting
    /// live cycles, so a completed-but-unawaited cycle frees its slot on the
    /// NEXT admission rather than holding it forever. This changes nothing
    /// about what a later `await`/`cancel` observes on that cycle — same
    /// typed terminal either way — only when the capacity count notices it
    /// finished.
    fn admit_cycle(&mut self) -> Result<(), SpawnError> {
        let async_ids: Vec<CycleId> = self
            .cycles
            .iter()
            .filter_map(|(id, cycle)| matches!(cycle, Cycle::Async(_)).then_some(*id))
            .collect();
        for id in async_ids {
            self.update_async(id, |c, transcripts| c.try_settle(id, transcripts));
        }
        let live = self.cycles.values().filter(|c| !c.is_terminal()).count();
        if live >= self.capacity {
            return Err(SpawnError::SpawnCapacityExhausted(self.capacity as i64));
        }
        Ok(())
    }

    /// Apply a CONSUMING [`AsyncCycle`] transition at `id`, in place. `f`'s
    /// `self -> Self` signature (`collect`/`try_settle`/`cancel`) is why this
    /// exists: those methods need OWNERSHIP to move a `Running` variant's
    /// `JoinHandle`/`Receiver` out, which a `&mut AsyncCycle` cannot give
    /// them. Removing the table entry first and reinserting the transformed
    /// result is what supplies that ownership without ever leaving the table
    /// holding a half-transitioned value. A no-op when `id` names nothing, or
    /// names a `Stepped` entry (put back untouched) — `f` runs only on an
    /// `Async` entry.
    fn update_async(
        &mut self,
        id: CycleId,
        f: impl FnOnce(AsyncCycle, &DelegateTranscriptSink) -> AsyncCycle,
    ) {
        let Some(cycle) = self.cycles.remove(&id) else {
            return;
        };
        let transcripts = &self.transcripts;
        let cycle = match cycle {
            Cycle::Async(boxed) => Cycle::Async(Box::new(f(*boxed, transcripts))),
            stepped @ Cycle::Stepped(_) => stepped,
        };
        self.cycles.insert(id, cycle);
    }

    /// One backend for one cycle. A factory failure is reported at
    /// `StageAllocating` because that is the truth: nothing has been allocated
    /// while the cycle is still being given its backend.
    fn new_backend(&mut self) -> Result<Box<dyn AgentBackend + Send>, SpawnError> {
        self.backends.create().map_err(|e| {
            SpawnError::SpawnBackendFailed(
                AgSpawnStage::StageAllocating,
                backend_failure_to_wire(e),
            )
        })
    }

    /// The agents of every STEPPED cycle still mid-turn, sorted.
    fn stepped_agents(&self) -> Vec<AgentId> {
        let mut agents: Vec<AgentId> = self
            .cycles
            .values()
            .filter_map(|c| match c {
                Cycle::Stepped(stepped) => stepped.agent(),
                _ => None,
            })
            .collect();
        agents.sort_unstable();
        agents
    }

    /// What a `NotRunning` says when no stepped cycle is running `agent`.
    ///
    /// Mirrors `CoupledSpawner::no_such_agent_detail` (private there), because
    /// the table now owns the lookup that used to happen inside the spawner's
    /// own map. The EMPTY case is `"no agent is mid-turn"` VERBATIM: that exact
    /// string is asserted by
    /// `handler_resume_with_no_agent_running_is_a_drive_failure` below.
    fn no_such_agent_detail(&self) -> String {
        match self.stepped_agents().as_slice() {
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
    ///
    /// It holds no table entry and is not counted against the cycle capacity:
    /// the whole cycle runs inside this one call, on the caller's thread, so
    /// the caller's own thread is what bounds it. Its backend is made for the
    /// call and dropped with it.
    fn subagent_spawn(
        &mut self,
        spec: AgSpawnSpec,
        schema: JsonArg,
    ) -> Result<AgSpawnOutcome, SpawnError> {
        let request = request_from_wire(spec, schema, Vec::new(), self.model, self.effort)?;
        let mut backend = self.new_backend()?;
        let run = self.spawner.spawn_one_cycle(&mut *backend, &request);
        // Confirmed-reap teardown before the backend drops, regardless of
        // outcome — this is the ordinary (non-cancelled) completion path.
        shutdown_backend(backend);
        let run = run.map_err(spawn_error_to_wire)?;
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
    ///
    /// A begun cycle takes a table slot (a `Stepped` entry holding its saga and
    /// its own backend) and is counted against the cycle capacity, exactly like
    /// an async one: a bound that covered only one of the two ways a cycle
    /// enters the table would not be a bound.
    fn subagent_begin(
        &mut self,
        spec: AgSpawnSpec,
        tools: JsonArg,
        schema: JsonArg,
    ) -> Result<AgAgentStep, SpawnError> {
        let tools = tool_declarations_from_wire(&tools.0)?;
        let request = request_from_wire(spec, schema, tools, self.model, self.effort)?;
        self.admit_cycle()?;
        let mut backend = self.new_backend()?;
        let progress = self
            .spawner
            .begin_detached(&mut *backend, &request)
            .map_err(spawn_error_to_wire)?;
        let id = self.mint_cycle();
        // A zero-round agent can finish on its very first stop —
        // `from_progress` is what builds the entry that never starts life
        // holding a live backend it will never use again.
        let (stepped, wire) = SteppedCycle::from_progress(progress, backend, id, &self.transcripts);
        self.cycles.insert(id, Cycle::Stepped(Box::new(stepped)));
        Ok(wire)
    }

    /// Serves `SubagentResume`: answer the parked tool call and drive on.
    ///
    /// `ok` false is a REFUSAL, not a transport failure — the child reads the
    /// text and reacts to it, so the call is always answered. Which agent and
    /// which call are checked before anything reaches the backend: the table
    /// routes by agent id, and [`CycleSaga::answer`] rechecks the agent and the
    /// parked call. A mismatch is `SpawnDriveFailed`, because the backend did
    /// nothing wrong — and with N cycles in flight, answering the wrong one
    /// must reach no backend at all.
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
        let Some(id) = self.stepped_cycle_of(agent) else {
            return Err(spawn_error_to_wire(DomainSpawnError::NotRunning {
                agent,
                detail: self.no_such_agent_detail(),
            }));
        };
        let Some(Cycle::Stepped(stepped)) = self.cycles.remove(&id) else {
            unreachable!("`stepped_cycle_of` only ever names a Stepped entry");
        };
        let SteppedCycle::Running {
            parked,
            mut backend,
        } = *stepped
        else {
            unreachable!("`stepped_cycle_of` only ever names a Running entry");
        };
        match parked.answer(&mut *backend, agent, ToolCallId(call), outcome) {
            Ok(progress) => {
                // Builds the next entry (dropping the backend if this stop
                // finished the saga) and the wire step from the SAME drive,
                // so `Running` and `Settled` can never drift apart from what
                // actually happened.
                let (next, wire) =
                    SteppedCycle::from_progress(progress, backend, id, &self.transcripts);
                self.cycles.insert(id, Cycle::Stepped(Box::new(next)));
                Ok(wire)
            }
            Err(failure) => match *failure {
                // The saga was never touched — put the SAME entry back
                // exactly as it was, so a caller that answered the wrong
                // call can retry with the right one instead of losing a
                // perfectly good cycle.
                (e, AnswerFailure::StillParked(parked)) => {
                    self.cycles.insert(
                        id,
                        Cycle::Stepped(Box::new(SteppedCycle::Running { parked, backend })),
                    );
                    Err(spawn_error_to_wire(e))
                }
                // The saga rolled itself back before returning (a
                // round-backstop trip or a backend failure) — a terminal
                // entry must hold no OS process, so the backend is dropped
                // in place of being kept.
                (e, AnswerFailure::RolledBack) => {
                    let transcript = backend.transcript_jsonl();
                    self.transcripts.persist(&cycle_tag(id), &transcript);
                    shutdown_backend(backend);
                    self.cycles.insert(
                        id,
                        Cycle::Stepped(Box::new(SteppedCycle::Settled { transcript })),
                    );
                    Err(spawn_error_to_wire(e))
                }
            },
        }
    }

    /// The cycle a stepped saga for `agent` lives in, if one is still mid-turn.
    ///
    /// A FINISHED saga does not match: its entry is retained for its
    /// transcript, but answering it is the same sequencing failure as answering
    /// an agent that never ran — which is what the spawner's own map said when
    /// it removed finished sagas.
    fn stepped_cycle_of(&self, agent: AgentId) -> Option<CycleId> {
        self.cycles.iter().find_map(|(id, cycle)| match cycle {
            Cycle::Stepped(stepped) if stepped.agent() == Some(agent) => Some(*id),
            _ => None,
        })
    }

    /// Serves `SubagentSpawnAsync`: the same saga as
    /// [`subagent_spawn`](Self::subagent_spawn), detached onto its own thread,
    /// returning the minted `CycleId` as soon as the cycle is ADMITTED.
    ///
    /// The whole saga — including `CycleSaga::begin`, which blocks on the
    /// backend's first turn — runs on the cycle thread; nothing about the spawn
    /// blocks the caller. The thread carries its own clone of the shared
    /// substrate handle and calls [`CycleSaga::begin`] directly (the same
    /// constructor `CoupledSpawner::begin_detached` delegates to; a
    /// `&CoupledSpawner` cannot cross into a `'static` thread).
    ///
    /// Order matters twice here: capacity is checked before ANY allocation, so
    /// a refused spawn creates no backend, no worktree, and no binding row; and
    /// the canceller is taken BEFORE the thread starts, because once that
    /// thread is inside `start_turn` it holds `&mut` on the backend and nothing
    /// else can reach it.
    fn subagent_spawn_async(
        &mut self,
        spec: AgSpawnSpec,
        schema: JsonArg,
    ) -> Result<AgCycleId, SpawnError> {
        self.admit_cycle()?;
        let request = request_from_wire(spec, schema, Vec::new(), self.model, self.effort)?;
        let backend = self.new_backend()?;
        let canceller = backend.canceller();
        let substrate = self.spawner.substrate();
        let (reports, receiver): (Sender<CycleReport>, Receiver<CycleReport>) = channel();
        let id = self.mint_cycle();

        let thread = std::thread::Builder::new()
            .name(format!("tidepool-cycle-{}", id.0))
            .spawn(move || {
                let mut backend = backend;
                let result = match CycleSaga::begin(&substrate, &mut *backend, &request) {
                    Err(e) => Err(e),
                    Ok(progress) => progress.run_to_completion(&mut *backend),
                };
                // `CycleProgress::run_to_completion` settles the saga on
                // every exit path before returning (completion, or its own
                // rollback on failure) — there is never a live saga left to
                // hand back for `cancel`'s defensive re-`abandon`.
                let saga = None;
                // A receiver dropped before the report lands means the handler
                // itself is gone; there is nobody left to tell.
                let _ = reports.send(CycleReport {
                    result,
                    saga,
                    backend,
                });
            })
            .map_err(|e| {
                SpawnError::SpawnDriveFailed(
                    AgSpawnStage::StageAllocating,
                    format!("could not start a cycle thread: {e}"),
                )
            })?;

        self.cycles.insert(
            id,
            Cycle::Async(Box::new(AsyncCycle::Running {
                thread,
                reports: receiver,
                canceller,
            })),
        );
        Ok(cycle_id_to_wire(id))
    }

    /// Serves `SubagentAwait`: BLOCK until this cycle reaches a terminal, then
    /// answer with its outcome or its typed failure.
    ///
    /// Blocking is no new hazard here: `EffectHandler::handle` is synchronous
    /// and `SubagentSpawn` already blocks for a whole cycle, so this needs
    /// neither a tokio dependency nor an async seam.
    ///
    /// The result is MEMOIZED, so a second await answers the same thing rather
    /// than blocking on a receiver whose sender is gone. A cancelled cycle
    /// answers `SpawnCancelled`; an id this handler never minted — or one that
    /// names a stepped cycle, which is driven by `SubagentResume` and not by
    /// this verb — is `SpawnDriveFailed` at `StageRunning`, that variant's
    /// documented meaning (the caller sequenced the loop wrongly; the backend
    /// did nothing).
    fn subagent_await(&mut self, cycle: AgCycleId) -> Result<AgSpawnOutcome, SpawnError> {
        let id = cycle_id_from_wire(cycle).ok_or_else(|| no_such_cycle(cycle))?;
        match self.cycles.get(&id) {
            None => return Err(no_such_cycle(cycle)),
            Some(Cycle::Stepped(_)) => {
                return Err(SpawnError::SpawnDriveFailed(
                    AgSpawnStage::StageRunning,
                    format!(
                        "cycle {} is a stepped tool-dispatch cycle: drive it with agentResumeRaw, \
                         not with awaitAgent",
                        id.0
                    ),
                ))
            }
            Some(Cycle::Async(_)) => {}
        }
        self.update_async(id, |c, transcripts| c.collect(id, transcripts));
        let Some(Cycle::Async(async_cycle)) = self.cycles.get(&id) else {
            unreachable!(
                "just confirmed an Async entry at this id, and update_async never removes one"
            );
        };
        #[allow(
            clippy::expect_used,
            reason = "collect always leaves the entry Settled"
        )]
        async_cycle
            .settled_terminal()
            .expect("collect always leaves the entry Settled")
            .to_wire(id)
    }

    /// Serves `SubagentCancel`: reap the cycle's backend, join its thread, and
    /// settle its binding `Released`.
    ///
    /// TOTAL by contract — see [`Self::cancel_cycle`] for what that costs and
    /// what it buys.
    fn subagent_cancel(
        &mut self,
        cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        cycle: AgCycleId,
    ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
        self.cancel_cycle(cycle);
        cx.respond(())
    }

    /// The body of `SubagentCancel`, separated from its dispatch wrapper so the
    /// handler's own tests can drive it without an `EffectContext`.
    ///
    /// TOTAL: an unknown id, an already-cancelled cycle, an already-awaited
    /// one, and a stepped one are all no-ops. Never panics and never blocks
    /// forever — the join is preceded by the kill that makes the cycle thread's
    /// seam call return.
    ///
    /// **Cancel SETTLES; it does not merely kill.** Reaping the backend stops
    /// the work; leaving the binding row `Active` would point at an agent that
    /// will never run again, which is exactly the orphan the saga's rollback
    /// semantics exist to prevent. Retain-first is locked: nothing is deleted.
    ///
    /// A cancelled cycle is RETAINED in the table as a terminal entry carrying
    /// `SpawnCancelled` rather than dropped — dropping it would make a later
    /// await indistinguishable from an await on a typo'd id, which is the one
    /// thing the `SpawnDriveFailed` spelling is supposed to mean.
    ///
    /// A stepped cycle is a deliberate no-op: it is driven inline by
    /// `SubagentResume`, its `CycleId` is never handed to an author, and
    /// killing a backend out from under a parked authored handler is not what
    /// `cancelAgent` promises.
    fn cancel_cycle(&mut self, cycle: AgCycleId) {
        let Some(id) = cycle_id_from_wire(cycle) else {
            return;
        };
        self.update_async(id, |c, transcripts| c.cancel(id, transcripts));
    }
}

/// Dropping the handler reaps every async cycle it still owns.
///
/// This is what keeps the ownership bound honest now that a cycle runs on its
/// own thread: without it, dropping the handler would leave a live thread
/// holding a live backend (and a child parked on an unanswered tool call) with
/// nothing left that could ever reach either. Same order as an explicit
/// cancel — kill, join, settle — so a dropped handler leaves no binding row
/// `Active` either.
///
/// The join is bounded by the kill actually reaping, which is why
/// `tidepool-agent/CLAUDE.md` forbids a backend that owns a process from
/// keeping the default no-op canceller: a canceller that returns without
/// reaping would turn this into a hang.
impl Drop for SubagentHandler {
    fn drop(&mut self) {
        // Drain rather than `update_async` per id: the table is about to be
        // dropped along with `self`, so there is nothing to reinsert into.
        let transcripts = &self.transcripts;
        for (id, cycle) in std::mem::take(&mut self.cycles) {
            match cycle {
                Cycle::Async(boxed) => {
                    let _ = boxed.cancel(id, transcripts);
                }
                // A stepped cycle still mid-turn when the handler itself
                // drops: no thread to join, but its backend still deserves a
                // confirmed reap rather than falling through to `Drop`'s
                // fire-and-forget kill.
                Cycle::Stepped(stepped) => {
                    if let SteppedCycle::Running { backend, .. } = *stepped {
                        shutdown_backend(backend);
                    }
                }
            }
        }
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

fn cycle_id_to_wire(id: CycleId) -> AgCycleId {
    AgCycleId { raw: id.0 as i64 }
}

/// A wire `CycleId` back into the handler's own. `None` is "never minted" — a
/// negative id, which cannot be one this handler handed out. The caller renders
/// it with [`no_such_cycle`], the same terminal an unknown id gets: from an
/// author's side both are the same mistake.
fn cycle_id_from_wire(id: AgCycleId) -> Option<CycleId> {
    u64::try_from(id.raw).ok().map(CycleId)
}

/// The terminal an await on an id this handler never minted resolves to.
///
/// `SpawnDriveFailed` at `StageRunning` is that variant's documented meaning:
/// the caller sequenced the loop wrongly and the backend did nothing. It is
/// deliberately NOT what a cancelled cycle answers — `SpawnCancelled` is a
/// different constructor precisely so "I cancelled this" and "this handle was
/// never real" can be told apart by case.
fn no_such_cycle(id: AgCycleId) -> SpawnError {
    SpawnError::SpawnDriveFailed(
        AgSpawnStage::StageRunning,
        format!(
            "no such cycle {}: this handler never minted it, or it belongs to another \
             resident cycle",
            id.raw
        ),
    )
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

    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use parking_lot::{Condvar, Mutex};
    use tidepool_agent::backend::mock::{MockBackend, MockFailure, MockStep};
    use tidepool_agent::seam::{CycleSpec, ThreadSpec, ToolCall, ToolReply, TurnEvent, TurnId};
    use tidepool_worktree::create::WorktreeHandle;
    use tidepool_worktree::id::{BranchName, GitOid, WorktreeId};
    use tidepool_worktree::registry::{WorktreeOrigin, WorktreeReceipt, WorktreeRecordStatus};
    use tidepool_worktree::testing::TestRepo;
    use tidepool_worktree::Binding;
    use tidepool_worktree::BindingState;

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

        /// The N-cycle wiring: one handler over a queue of pre-built backends.
        fn handler_with(&self, backends: Box<dyn AgentBackendFactory>) -> SubagentHandler {
            SubagentHandler::with_backends(
                self.registry_root(),
                self.worktree_root(),
                self.binding_root(),
                self.repo.path().to_path_buf(),
                backends,
            )
            .expect("open the subagent handler over the temp substrate")
        }

        /// The full persisted lease history for `id`, read straight off disk.
        ///
        /// `BindingTable::current` only ever answers with an ACTIVE binding, so
        /// distinguishing "settled Released" from "settled Terminal" (both
        /// leave `current` empty) needs the raw file — the same read
        /// `subagent_one_cycle.rs` does for the same reason. Plain file reads
        /// never contend with the table's exclusive owner lock.
        fn binding_history(&self, id: &WorktreeId) -> Vec<Binding> {
            let path = self.binding_root().join(format!("{}.json", id.as_str()));
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|e| panic!("reading binding history at {path:?}: {e}"));
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|e| panic!("parsing binding history at {path:?}: {e}"))
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

    /// `worktree.cwd()` and `source_repository().join(".git")` both cross
    /// into a spawned backend's `CycleSpec` as plain text
    /// (`tidepool-agent/src/spawn.rs`) — a non-UTF-8 root must be rejected
    /// here, at construction, as a typed error rather than silently
    /// misconfiguring every cycle's sandbox.
    #[cfg(unix)]
    #[test]
    fn with_backends_rejects_non_utf8_worktree_root() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let roots = tempfile::TempDir::new().expect("create the substrate roots");
        // `0x80` alone is not a valid UTF-8 lead byte.
        let bad_name = OsString::from_vec(vec![b'w', b't', 0x80]);
        let bad_worktree_root = roots.path().join(PathBuf::from(bad_name));

        match SubagentHandler::new(
            roots.path().join("registry"),
            bad_worktree_root,
            roots.path().join("bindings"),
            roots.path().join("source"),
            Box::new(MockBackend::scripted([])),
        ) {
            Ok(_) => panic!("a non-UTF-8 worktree_root must be rejected, not silently accepted"),
            Err(err) => assert!(
                matches!(err, DomainWorktreeError::StorageFailure { .. }),
                "{err:?}"
            ),
        }
    }

    // ------------------------------------------------------------------
    // Concurrency fixtures: a rendezvous latch and a backend that announces
    // when its turn starts and ends. No test below sleeps, and none assumes
    // anything about thread scheduling beyond what these pin.
    // ------------------------------------------------------------------

    /// Threads ARRIVE; the test WAITS for a count.
    ///
    /// Timed out rather than unbounded, so a rendezvous that can never happen
    /// (three cycles that are secretly serialized, say) FAILS with what did
    /// arrive instead of hanging the suite. The timeout is a liveness backstop,
    /// not a timing assumption: a correct implementation reaches every
    /// rendezvous immediately.
    #[derive(Default)]
    struct Latch {
        arrivals: Mutex<Vec<usize>>,
        changed: Condvar,
    }

    impl Latch {
        fn arrive(&self, who: usize) {
            self.arrivals.lock().push(who);
            self.changed.notify_all();
        }

        fn arrivals(&self) -> Vec<usize> {
            self.arrivals.lock().clone()
        }

        /// Block until at least `n` arrivals, or fail naming what did arrive.
        fn wait_for(&self, n: usize, what: &str) {
            let mut arrivals = self.arrivals.lock();
            while arrivals.len() < n {
                let timeout = self
                    .changed
                    .wait_for(&mut arrivals, Duration::from_secs(30));
                assert!(
                    !timeout.timed_out() || arrivals.len() >= n,
                    "timed out waiting for {n} {what}; only {:?} arrived",
                    *arrivals
                );
            }
        }
    }

    /// [`MockBackend`] wrapped to announce when its turn STARTS and when it
    /// COMPLETES, so a test can pin order by rendezvous instead of by sleeping.
    ///
    /// `canceller` forwards to the mock's own control: cancelling this wrapper
    /// really does make a blocked `MockStep::Blocks` return, which is what a
    /// cancellation test needs to be about the handler rather than about the
    /// wrapper.
    struct GatedBackend {
        inner: MockBackend,
        who: usize,
        started: Arc<Latch>,
        completed: Arc<Latch>,
    }

    impl AgentBackend for GatedBackend {
        fn start_thread(
            &mut self,
            spec: &ThreadSpec,
        ) -> Result<BackendThreadId, AgentBackendError> {
            self.inner.start_thread(spec)
        }

        fn start_turn(
            &mut self,
            thread: &BackendThreadId,
            spec: &CycleSpec,
        ) -> Result<TurnEvent, AgentBackendError> {
            self.started.arrive(self.who);
            let event = self.inner.start_turn(thread, spec);
            if matches!(event, Ok(TurnEvent::Completed(_))) {
                self.completed.arrive(self.who);
            }
            event
        }

        fn resume(&mut self, reply: ToolReply) -> Result<TurnEvent, AgentBackendError> {
            self.inner.resume(reply)
        }

        fn canceller(&self) -> Box<dyn BackendCanceller> {
            self.inner.canceller()
        }
    }

    /// A factory over a fixed list of pre-built backends, handed out in order —
    /// the N-cycle wiring in test form. It counts its calls, so a test can
    /// assert that a REFUSED spawn never asked for a backend at all.
    struct QueuedBackends {
        queue: VecDeque<Box<dyn AgentBackend + Send>>,
        created: Arc<AtomicUsize>,
    }

    impl AgentBackendFactory for QueuedBackends {
        fn create(&mut self) -> Result<Box<dyn AgentBackend + Send>, AgentBackendError> {
            self.created.fetch_add(1, Ordering::SeqCst);
            self.queue
                .pop_front()
                .ok_or_else(|| AgentBackendError::BackendUnavailable {
                    detail: "the test wired fewer backends than it spawned cycles".to_string(),
                })
        }
    }

    /// N gated backends, each scripted to BLOCK once and then complete with its
    /// own payload — plus the controls that release them and the latches that
    /// say when each started and finished.
    struct Fleet {
        controls: Vec<Arc<tidepool_agent::backend::mock::MockControl>>,
        started: Arc<Latch>,
        completed: Arc<Latch>,
        created: Arc<AtomicUsize>,
        factory: Option<Box<dyn AgentBackendFactory>>,
    }

    impl Fleet {
        fn new(n: usize) -> Self {
            let started = Arc::new(Latch::default());
            let completed = Arc::new(Latch::default());
            let created = Arc::new(AtomicUsize::new(0));
            let mut controls = Vec::new();
            let mut queue: VecDeque<Box<dyn AgentBackend + Send>> = VecDeque::new();
            for who in 0..n {
                let inner = MockBackend::scripted([
                    MockStep::Blocks,
                    MockStep::Completes(CycleResultPayload::Structured(
                        serde_json::json!({ "summary": format!("cycle {who}") }),
                    )),
                ]);
                controls.push(inner.control());
                queue.push_back(Box::new(GatedBackend {
                    inner,
                    who,
                    started: Arc::clone(&started),
                    completed: Arc::clone(&completed),
                }));
            }
            Self {
                controls,
                started,
                completed,
                created: Arc::clone(&created),
                factory: Some(Box::new(QueuedBackends { queue, created })),
            }
        }

        fn factory(&mut self) -> Box<dyn AgentBackendFactory> {
            self.factory.take().expect("the factory is taken once")
        }

        /// The payload cycle `who`'s backend completes with.
        fn payload(who: usize) -> AgCyclePayload {
            AgCyclePayload::PayloadStructured(serde_json::json!({
                "summary": format!("cycle {who}")
            }))
        }
    }

    /// Spawn one async cycle at the shared fixture spec.
    fn spawn_async(handler: &mut SubagentHandler, label: &str) -> Result<AgCycleId, SpawnError> {
        handler.subagent_spawn_async(
            new_worktree_spec(label, "run concurrently"),
            JsonArg(sample_schema()),
        )
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
    // Named gates that drive the real saga, through
    // `CoupledSpawner::spawn_one_cycle`.
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

    // ==================================================================
    // The async trio: `SubagentSpawnAsync` / `SubagentAwait` /
    // `SubagentCancel`, over the cycle table. Every row here pins order by
    // MockControl rendezvous — no sleeps, and no assumption about thread
    // scheduling beyond what a release or a cancel makes true.
    // ==================================================================

    /// THREE cycles in flight under ONE handler. Each backend blocks inside its
    /// own turn, so the third `started` arrival is unreachable unless all three
    /// cycles really are running at once — the assertion is the rendezvous, not
    /// a duration.
    #[test]
    fn handler_runs_three_async_cycles_concurrently() {
        let fx = Fixture::new();
        let mut fleet = Fleet::new(3);
        let started = Arc::clone(&fleet.started);
        let mut handler = fx.handler_with(fleet.factory()).with_cycle_capacity(3);

        let cycles: Vec<AgCycleId> = (0..3)
            .map(|i| {
                spawn_async(&mut handler, &format!("worker-{i}"))
                    .expect("a spawn under the cap is admitted immediately")
            })
            .collect();
        assert_eq!(
            cycles,
            vec![
                AgCycleId { raw: 0 },
                AgCycleId { raw: 1 },
                AgCycleId { raw: 2 }
            ],
            "cycle ids are minted monotonically from 0 and handed back at ADMISSION"
        );

        started.wait_for(3, "cycle turns to start");

        // Three distinct worktrees, three distinct bindings — the substrate
        // served all three without any of them waiting on another's turn.
        assert_eq!(
            handler
                .spawner()
                .manager()
                .list()
                .expect("list the registry")
                .len(),
            3,
            "each concurrent cycle got its own worktree"
        );
        assert_eq!(fx.persisted_binding_files().len(), 3);

        for (who, control) in fleet.controls.iter().enumerate() {
            control.release();
            let outcome = handler
                .subagent_await(cycles[who])
                .expect("a released cycle completes");
            assert_eq!(outcome.outcome_payload, Fleet::payload(who));
        }
    }

    /// Completion order is not an input to any result: release 2, then 0, then
    /// 1, await in SPAWN order, and every handle still carries its own cycle's
    /// payload.
    #[test]
    fn handler_await_order_is_independent_of_completion_order() {
        let fx = Fixture::new();
        let mut fleet = Fleet::new(3);
        let started = Arc::clone(&fleet.started);
        let completed = Arc::clone(&fleet.completed);
        let mut handler = fx.handler_with(fleet.factory()).with_cycle_capacity(3);

        let cycles: Vec<AgCycleId> = (0..3)
            .map(|i| {
                spawn_async(&mut handler, &format!("worker-{i}")).expect("admitted immediately")
            })
            .collect();
        started.wait_for(3, "cycle turns to start");

        // Pinned one at a time: each release is followed by the rendezvous that
        // says that cycle finished, so the order is a fact rather than a hope.
        for (n, who) in [2usize, 0, 1].into_iter().enumerate() {
            fleet.controls[who].release();
            completed.wait_for(n + 1, "cycle turns to complete");
        }
        assert_eq!(
            completed.arrivals(),
            vec![2, 0, 1],
            "the cycles completed in an order the test chose, not the spawn order"
        );

        for (who, cycle) in cycles.iter().enumerate() {
            let outcome = handler
                .subagent_await(*cycle)
                .expect("every cycle has already finished");
            assert_eq!(
                outcome.outcome_payload,
                Fleet::payload(who),
                "cycle {who}'s handle answers with cycle {who}'s payload"
            );
        }
    }

    /// Cancel REAPS and SETTLES. Reaping the backend is half of it; the other
    /// half is that no binding row is left `Active` pointing at an agent that
    /// will never run again — asserted on the binding STATE, from disk.
    #[test]
    fn handler_cancel_reaps_the_backend_and_settles_the_binding_released() {
        let fx = Fixture::new();
        let mut fleet = Fleet::new(1);
        let started = Arc::clone(&fleet.started);
        let mut handler = fx.handler_with(fleet.factory());

        let cycle = spawn_async(&mut handler, "worker").expect("admitted immediately");
        started.wait_for(1, "the cycle's turn to start");

        // Blocked inside the seam call. Only a canceller can make it return —
        // a flag the blocked thread would have to check is not cancellation.
        handler.cancel_cycle(cycle);

        let worktrees = handler
            .spawner()
            .manager()
            .list()
            .expect("list the registry");
        assert_eq!(worktrees.len(), 1, "the cancelled cycle got one worktree");
        let worktree = worktrees[0].receipt.worktree_id.clone();

        assert!(
            handler.spawner().bindings().current(&worktree).is_none(),
            "a cancelled cycle leaves NO active binding"
        );
        assert_eq!(
            fx.binding_history(&worktree)
                .last()
                .expect("one binding row")
                .state(),
            BindingState::Released,
            "cancel settles Released — stopped waiting, rebindable — never left Active"
        );

        // Retain-first: cancellation deletes nothing.
        let found = handler
            .spawner()
            .manager()
            .lookup(&worktree)
            .expect("lookup must not fail: the worktree is retained")
            .expect("the worktree is still registered");
        assert!(
            found.cwd().exists(),
            "the worktree is retained on disk: {:?}",
            found.cwd()
        );

        assert_eq!(
            handler.subagent_await(cycle),
            Err(SpawnError::SpawnCancelled(cycle)),
            "the cancelled cycle is RETAINED as a terminal entry carrying its own \
             constructor — not dropped, which would make this indistinguishable from a \
             typo'd handle"
        );
    }

    /// One row of the cancel/await race matrix. Both verbs can arrive in either
    /// order against the same handle, and neither order may hang or panic.
    #[derive(Debug, Clone, Copy)]
    enum Race {
        CancelThenAwait,
        AwaitThenCancel,
        CancelThenCancel,
        AwaitThenAwait,
        UnknownHandle,
    }

    #[test]
    fn handler_cancel_await_races_reach_typed_terminals() {
        for row in [
            Race::CancelThenAwait,
            Race::AwaitThenCancel,
            Race::CancelThenCancel,
            Race::AwaitThenAwait,
            Race::UnknownHandle,
        ] {
            let fx = Fixture::new();
            let mut fleet = Fleet::new(1);
            let started = Arc::clone(&fleet.started);
            let mut handler = fx.handler_with(fleet.factory());
            let cycle = spawn_async(&mut handler, "worker").expect("admitted immediately");
            started.wait_for(1, "the cycle's turn to start");

            match row {
                Race::CancelThenAwait => {
                    handler.cancel_cycle(cycle);
                    assert_eq!(
                        handler.subagent_await(cycle),
                        Err(SpawnError::SpawnCancelled(cycle)),
                        "{row:?}"
                    );
                }
                Race::AwaitThenCancel => {
                    fleet.controls[0].release();
                    let awaited = handler
                        .subagent_await(cycle)
                        .expect("the released cycle completes");
                    handler.cancel_cycle(cycle);
                    assert_eq!(
                        handler.subagent_await(cycle),
                        Ok(awaited),
                        "{row:?}: cancelling a cycle that already answered is a no-op — it \
                         does not rewrite the result the author already has"
                    );
                }
                Race::CancelThenCancel => {
                    handler.cancel_cycle(cycle);
                    handler.cancel_cycle(cycle);
                    assert_eq!(
                        handler.subagent_await(cycle),
                        Err(SpawnError::SpawnCancelled(cycle)),
                        "{row:?}: the second cancel is a no-op, not a second settle"
                    );
                }
                Race::AwaitThenAwait => {
                    fleet.controls[0].release();
                    let first = handler.subagent_await(cycle);
                    let second = handler.subagent_await(cycle);
                    assert!(first.is_ok(), "{row:?}: {first:?}");
                    assert_eq!(
                        first, second,
                        "{row:?}: a second await answers the SAME memoized result, never a \
                         lost-receiver panic"
                    );
                }
                Race::UnknownHandle => {
                    for ghost in [AgCycleId { raw: 4242 }, AgCycleId { raw: -1 }] {
                        let err = handler
                            .subagent_await(ghost)
                            .expect_err("no such cycle was ever minted");
                        assert!(
                            matches!(
                                &err,
                                SpawnError::SpawnDriveFailed(AgSpawnStage::StageRunning, detail)
                                    if detail.contains(&format!("no such cycle {}", ghost.raw))
                            ),
                            "{row:?}: an unknown handle is a DRIVE failure naming it: {err:?}"
                        );
                        // Total: cancelling an unknown handle reports nothing
                        // and does nothing.
                        handler.cancel_cycle(ghost);
                    }
                    fleet.controls[0].release();
                    assert!(
                        handler.subagent_await(cycle).is_ok(),
                        "{row:?}: the real cycle is untouched by traffic against ghost handles"
                    );
                }
            }
        }
    }

    /// The cap is a BOUND, not a backlog: the spawn past it is refused
    /// immediately and NOTHING is allocated for it — the same disk-level claim
    /// `handler_rejects_path_unsafe_existing_id_before_touching_disk` makes.
    #[test]
    fn handler_table_full_is_typed_and_allocates_nothing() {
        let fx = Fixture::new();
        let mut fleet = Fleet::new(2);
        let started = Arc::clone(&fleet.started);
        let created = Arc::clone(&fleet.created);
        let mut handler = fx.handler_with(fleet.factory()).with_cycle_capacity(2);

        let first = spawn_async(&mut handler, "worker-0").expect("under the cap");
        let second = spawn_async(&mut handler, "worker-1").expect("at the cap");
        started.wait_for(2, "both admitted cycles to start");

        let refused = spawn_async(&mut handler, "worker-2").expect_err("the table is full");
        assert_eq!(
            refused,
            SpawnError::SpawnCapacityExhausted(2),
            "a spawn past the cap is refused with the cap, never queued behind it"
        );

        assert_eq!(
            created.load(Ordering::SeqCst),
            2,
            "the refused spawn never even asked the factory for a backend"
        );
        assert_eq!(
            fx.persisted_binding_files().len(),
            2,
            "no binding row was written for the refused spawn: {:?}",
            fx.persisted_binding_files()
        );
        assert_eq!(
            handler
                .spawner()
                .manager()
                .list()
                .expect("list the registry")
                .len(),
            2,
            "no worktree was registered for the refused spawn"
        );

        // Reaping one frees its slot — the bound is a ceiling on cycles in
        // flight, not a one-time budget.
        fleet.controls[0].release();
        handler
            .subagent_await(first)
            .expect("the released cycle completes");
        let err =
            spawn_async(&mut handler, "worker-3").expect_err("this test wired only two backends");
        assert!(
            matches!(
                err,
                SpawnError::SpawnBackendFailed(AgSpawnStage::StageAllocating, _)
            ),
            "with a slot free the refusal comes from the BACKEND, not from the cap — the two \
             have to stay distinguishable: {err:?}"
        );

        fleet.controls[1].release();
        handler
            .subagent_await(second)
            .expect("the second released cycle completes");
    }

    /// `SubagentHandler::new` wires ONE pre-built backend, so a second cycle
    /// has nothing to run on. The refusal must name the WIRING — it is not the
    /// one-agent-at-a-time constraint this lane deleted, and an operator who
    /// reads it as one would go looking for a policy that no longer exists.
    #[test]
    fn handler_one_shot_backend_refusal_names_the_wiring() {
        let fx = Fixture::new();
        let mut handler = fx.handler(MockBackend::completing(CycleResultPayload::Absent));

        handler
            .subagent_spawn(
                new_worktree_spec("first", "run"),
                JsonArg(serde_json::Value::Null),
            )
            .expect("the one wired backend runs the first cycle");

        let err = handler
            .subagent_spawn(
                new_worktree_spec("second", "run"),
                JsonArg(serde_json::Value::Null),
            )
            .expect_err("the one-shot factory has nothing left to hand out");

        match err {
            SpawnError::SpawnBackendFailed(
                AgSpawnStage::StageAllocating,
                AgBackendFailure::BackendUnavailable(detail),
            ) => {
                assert!(
                    detail.contains("with_backends"),
                    "the refusal points at the N-cycle constructor: {detail}"
                );
                assert!(
                    detail.contains("not a refusal to run concurrent agents"),
                    "the refusal says what it is NOT, so it is never read as the deleted \
                     one-at-a-time constraint: {detail}"
                );
            }
            other => panic!("expected a BackendUnavailable at Allocating, got {other:?}"),
        }
    }

    // ==================================================================
    // Cycle hygiene: cancel during the connect window, backend drop at
    // settle, and opportunistic capacity settling.
    // ==================================================================

    /// A backend whose `transcript_jsonl` is a fixed script, and whose `Drop`
    /// reports itself — so a test can assert BOTH that the extracted
    /// transcript survived a settle and that the backend object itself was
    /// actually deallocated (never merely un-referenced).
    struct DropSignal {
        inner: MockBackend,
        transcript: Vec<String>,
        dropped: Arc<AtomicBool>,
        /// Set by `shutdown` (the confirmed-reap teardown), independent of
        /// `dropped` (`Drop`, the fire-and-forget backstop) — the two tests
        /// this distinction is for want to see teardown happen via the
        /// explicit call, not merely that the value eventually deallocated.
        shutdown_called: Arc<AtomicBool>,
    }

    impl DropSignal {
        /// The common case: a test that only cares about `Drop`, not
        /// `shutdown` — a fresh, unshared flag for the field nobody reads.
        fn new(inner: MockBackend, transcript: Vec<String>, dropped: Arc<AtomicBool>) -> Self {
            Self {
                inner,
                transcript,
                dropped,
                shutdown_called: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    impl AgentBackend for DropSignal {
        fn start_thread(
            &mut self,
            spec: &ThreadSpec,
        ) -> Result<BackendThreadId, AgentBackendError> {
            self.inner.start_thread(spec)
        }

        fn start_turn(
            &mut self,
            thread: &BackendThreadId,
            spec: &CycleSpec,
        ) -> Result<TurnEvent, AgentBackendError> {
            self.inner.start_turn(thread, spec)
        }

        fn resume(&mut self, reply: ToolReply) -> Result<TurnEvent, AgentBackendError> {
            self.inner.resume(reply)
        }

        fn transcript_jsonl(&self) -> Vec<String> {
            self.transcript.clone()
        }

        fn canceller(&self) -> Box<dyn BackendCanceller> {
            self.inner.canceller()
        }

        fn shutdown(self: Box<Self>) -> Result<(), AgentBackendError> {
            self.shutdown_called.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn one_shot_queue(backend: Box<dyn AgentBackend + Send>) -> Box<dyn AgentBackendFactory> {
        let mut queue: VecDeque<Box<dyn AgentBackend + Send>> = VecDeque::new();
        queue.push_back(backend);
        Box::new(QueuedBackends {
            queue,
            created: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// A settled ASYNC cycle drops its backend (no OS process survives a
    /// reap) but keeps what it said, extracted at the instant it settled.
    #[test]
    fn handler_settled_async_cycle_drops_its_backend_but_keeps_the_transcript() {
        let fx = Fixture::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let shutdown_called = Arc::new(AtomicBool::new(false));
        let mut backend = DropSignal::new(
            MockBackend::completing(CycleResultPayload::Absent),
            vec!["{\"frame\":1}".to_string(), "{\"frame\":2}".to_string()],
            Arc::clone(&dropped),
        );
        backend.shutdown_called = Arc::clone(&shutdown_called);
        let mut handler = fx.handler_with(one_shot_queue(Box::new(backend)));

        let cycle = spawn_async(&mut handler, "worker").expect("admitted immediately");
        handler
            .subagent_await(cycle)
            .expect("the mock completes immediately");

        assert!(
            shutdown_called.load(Ordering::SeqCst),
            "a settled cycle must confirm its backend's reap via shutdown, not just drop"
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "a settled cycle must drop its backend — no OS process may outlive settle"
        );
        assert_eq!(
            handler.backend_transcript_jsonl(),
            vec!["{\"frame\":1}".to_string(), "{\"frame\":2}".to_string()],
            "the transcript survives the backend's drop — extracted at settle"
        );
    }

    /// The same guarantee on the STEPPED path: once `SubagentResume` drives a
    /// saga to `Done`, its backend is dropped even though the table entry is
    /// retained.
    #[test]
    fn handler_settled_stepped_cycle_drops_its_backend_but_keeps_the_transcript() {
        let fx = Fixture::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let shutdown_called = Arc::new(AtomicBool::new(false));
        let mut backend = DropSignal::new(
            MockBackend::scripted([
                MockStep::Calls {
                    tool: "ask_parent".to_string(),
                    arguments: serde_json::json!({ "q": "which file?" }),
                },
                MockStep::Completes(CycleResultPayload::Absent),
            ]),
            vec!["{\"frame\":1}".to_string()],
            Arc::clone(&dropped),
        );
        backend.shutdown_called = Arc::clone(&shutdown_called);
        let mut handler = fx.handler_with(one_shot_queue(Box::new(backend)));

        let step = handler
            .subagent_begin(
                new_worktree_spec("reviewer", "summarize the diff"),
                JsonArg(one_declaration()),
                JsonArg(sample_schema()),
            )
            .expect("the scripted turn parks on a tool call");
        let AgAgentStep::StepToolCall(agent, call, ..) = step else {
            panic!("expected a parked tool call, got {step:?}");
        };
        assert!(
            !dropped.load(Ordering::SeqCst),
            "a mid-turn cycle must keep its backend"
        );

        handler
            .subagent_resume(
                agent,
                call,
                true,
                JsonArg(serde_json::json!({ "file": "notes.md" })),
            )
            .expect("answering the parked call drives the turn to done");

        assert!(
            shutdown_called.load(Ordering::SeqCst),
            "a finished stepped cycle must confirm its backend's reap via shutdown, not just drop"
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "a finished stepped cycle must drop its backend — the table entry is retained, \
             the OS process is not"
        );
        assert_eq!(
            handler.backend_transcript_jsonl(),
            vec!["{\"frame\":1}".to_string()],
            "the transcript survives the backend's drop — extracted at settle"
        );
    }

    /// The synchronous, single-call path (`SubagentSpawn`) confirms its
    /// backend's reap via `shutdown` before returning, on ORDINARY
    /// completion — not merely relying on the implicit `Drop` at the end of
    /// the call. This is the specific gap the containment review flagged:
    /// `subagent_spawn`'s backend used to fall through to fire-and-forget
    /// `Drop` with nothing confirming the reap.
    #[test]
    fn handler_subagent_spawn_confirms_backend_shutdown_on_ordinary_completion() {
        let fx = Fixture::new();
        let dropped = Arc::new(AtomicBool::new(false));
        let shutdown_called = Arc::new(AtomicBool::new(false));
        let mut backend = DropSignal::new(
            MockBackend::completing(CycleResultPayload::Absent),
            vec!["{\"frame\":1}".to_string()],
            Arc::clone(&dropped),
        );
        backend.shutdown_called = Arc::clone(&shutdown_called);
        let mut handler = fx.handler_with(one_shot_queue(Box::new(backend)));

        handler
            .subagent_spawn(
                new_worktree_spec("worker", "run"),
                JsonArg(serde_json::Value::Null),
            )
            .expect("the mock completes immediately");

        assert!(
            shutdown_called.load(Ordering::SeqCst),
            "subagent_spawn must confirm its backend's reap via shutdown before it drops, on \
             the ordinary completion path"
        );
        assert!(dropped.load(Ordering::SeqCst));
    }

    // ==================================================================
    // Delegate transcript persistence: TIDEPOOL_DELEGATE_TRANSCRIPT_DIR.
    // ==================================================================

    /// A cycle that reaches a terminal writes its full frame transcript to
    /// `delegate-<cycle_tag>.jsonl` under the configured directory.
    #[test]
    fn delegate_transcript_sink_persists_a_settled_async_cycle() {
        let fx = Fixture::new();
        let out_dir = tempfile::TempDir::new().expect("create the transcript output dir");
        std::env::set_var("TIDEPOOL_DELEGATE_TRANSCRIPT_DIR", out_dir.path());

        let backend = DropSignal::new(
            MockBackend::completing(CycleResultPayload::Absent),
            vec!["{\"frame\":1}".to_string(), "{\"frame\":2}".to_string()],
            Arc::new(AtomicBool::new(false)),
        );
        let mut handler = fx.handler_with(one_shot_queue(Box::new(backend)));

        let cycle = spawn_async(&mut handler, "worker").expect("admitted immediately");
        handler
            .subagent_await(cycle)
            .expect("the mock completes immediately");

        std::env::remove_var("TIDEPOOL_DELEGATE_TRANSCRIPT_DIR");

        let path = out_dir.path().join(format!("delegate-{}.jsonl", cycle.raw));
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading persisted transcript at {path:?}: {e}"));
        assert_eq!(contents, "{\"frame\":1}\n{\"frame\":2}\n");
    }

    /// Two cycles that land on the SAME tag (a fresh handler's `CycleId`
    /// counter always starts at 0) never clobber each other's file — the
    /// second gets a unique suffix instead of overwriting the first.
    #[test]
    fn delegate_transcript_sink_never_overwrites_a_name_collision() {
        let out_dir = tempfile::TempDir::new().expect("create the transcript output dir");
        std::env::set_var("TIDEPOOL_DELEGATE_TRANSCRIPT_DIR", out_dir.path());

        let fx_one = Fixture::new();
        let backend_one = DropSignal::new(
            MockBackend::completing(CycleResultPayload::Absent),
            vec!["{\"frame\":\"first\"}".to_string()],
            Arc::new(AtomicBool::new(false)),
        );
        let mut handler_one = fx_one.handler_with(one_shot_queue(Box::new(backend_one)));
        let cycle_one = spawn_async(&mut handler_one, "worker-one").expect("admitted immediately");
        handler_one
            .subagent_await(cycle_one)
            .expect("the mock completes immediately");

        let fx_two = Fixture::new();
        let backend_two = DropSignal::new(
            MockBackend::completing(CycleResultPayload::Absent),
            vec!["{\"frame\":\"second\"}".to_string()],
            Arc::new(AtomicBool::new(false)),
        );
        let mut handler_two = fx_two.handler_with(one_shot_queue(Box::new(backend_two)));
        let cycle_two = spawn_async(&mut handler_two, "worker-two").expect("admitted immediately");
        handler_two
            .subagent_await(cycle_two)
            .expect("the mock completes immediately");

        std::env::remove_var("TIDEPOOL_DELEGATE_TRANSCRIPT_DIR");

        assert_eq!(
            cycle_one.raw, cycle_two.raw,
            "both cycles are the first their own handler ever minted, so they share a tag"
        );

        let first_path = out_dir
            .path()
            .join(format!("delegate-{}.jsonl", cycle_one.raw));
        let first = std::fs::read_to_string(&first_path)
            .unwrap_or_else(|e| panic!("reading {first_path:?}: {e}"));
        assert_eq!(
            first, "{\"frame\":\"first\"}\n",
            "the first cycle's file is never overwritten by the second"
        );

        let mut entries: Vec<String> = std::fs::read_dir(out_dir.path())
            .expect("read the transcript dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert_eq!(entries.len(), 2, "both cycles left a file: {entries:?}");
        let first_name = format!("delegate-{}.jsonl", cycle_one.raw);
        let second_name = entries
            .iter()
            .find(|n| **n != first_name)
            .unwrap_or_else(|| panic!("expected a uniquely-suffixed second file: {entries:?}"));
        let second = std::fs::read_to_string(out_dir.path().join(second_name))
            .unwrap_or_else(|e| panic!("reading {second_name}: {e}"));
        assert_eq!(second, "{\"frame\":\"second\"}\n");
    }

    /// Retries a spawn until it is admitted or a bounded deadline passes —
    /// tolerating the tiny window between a released cycle's backend
    /// returning and its own thread finishing the send that lands its report
    /// in the channel `try_settle` drains. `yield_now` rather than a sleep:
    /// there is no real I/O on either side of this race, so a spin is cheap
    /// and the bound is a liveness backstop, not a timing assumption.
    fn spawn_async_eventually(handler: &mut SubagentHandler, label: &str) -> AgCycleId {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match spawn_async(handler, label) {
                Ok(id) => return id,
                Err(SpawnError::SpawnCapacityExhausted(_))
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::yield_now();
                }
                Err(e) => panic!("spawn_async_eventually({label}) timed out or failed: {e:?}"),
            }
        }
    }

    /// The capacity bound counts NON-TERMINAL cycles, but a cycle whose
    /// thread already finished and was simply never `await`ed is not
    /// "running" in any sense an operator cares about. Admission must
    /// opportunistically drain such reports so eight completed-but-unawaited
    /// cycles free their slots for a ninth spawn.
    #[test]
    fn handler_admit_opportunistically_settles_finished_unawaited_cycles() {
        let fx = Fixture::new();
        let mut fleet = Fleet::new(9);
        let started = Arc::clone(&fleet.started);
        let mut handler = fx.handler_with(fleet.factory()).with_cycle_capacity(8);

        let cycles: Vec<AgCycleId> = (0..8)
            .map(|i| spawn_async(&mut handler, &format!("worker-{i}")).expect("under the cap"))
            .collect();
        started.wait_for(8, "all eight cycles to start");

        // Release all eight and let them run to completion WITHOUT awaiting
        // any of them — they sit in the table as completed-but-unawaited.
        for control in &fleet.controls[..8] {
            control.release();
        }

        // A ninth spawn must eventually be admitted: opportunistic settling
        // during admission drains the finished cycles' reports and frees
        // their slots, even though nothing ever called `awaitAgent`.
        let ninth = spawn_async_eventually(&mut handler, "worker-8");

        // Every one of the eight is still answerable with its own payload —
        // opportunistic settling must not change what `await` sees.
        for (who, cycle) in cycles.iter().enumerate() {
            let outcome = handler.subagent_await(*cycle).expect(
                "opportunistic settling during admission must not change the typed terminal \
                 an await later sees",
            );
            assert_eq!(outcome.outcome_payload, Fleet::payload(who));
        }

        fleet.controls[8].release();
        handler
            .subagent_await(ninth)
            .expect("the ninth cycle, admitted after opportunistic settling, completes normally");
    }

    /// Blocks [`SlowConnectBackend::start_turn`] until the test releases it —
    /// standing in for the real Codex adapter's process-spawn + handshake
    /// window, which a pid-based canceller cannot interrupt before the pid is
    /// known. Plain `Mutex`+`Condvar`, deliberately NOT `MockControl`: unlike
    /// a scripted `MockStep::Blocks`, this wait must NOT itself observe
    /// cancellation — a real handshake keeps running to completion regardless
    /// of a cancel racing it.
    #[derive(Default)]
    struct ConnectGate {
        released: Mutex<bool>,
        changed: Condvar,
    }

    impl ConnectGate {
        fn block_until_released(&self) {
            let mut released = self.released.lock();
            while !*released {
                self.changed.wait(&mut released);
            }
        }

        fn release(&self) {
            *self.released.lock() = true;
            self.changed.notify_all();
        }
    }

    /// Sets `cancel_requested` and announces having done so — the test
    /// rendezvous that makes "cancel registers before the connect window
    /// closes" a fact rather than a hope. Deliberately does NOT reach into
    /// the inner mock's own control: nothing here may rescue a hang via that
    /// path, or the test would pass whether or not the connect-window check
    /// under test actually runs.
    struct SlowConnectCanceller {
        cancel_requested: Arc<AtomicBool>,
        registered: Arc<Latch>,
    }

    impl BackendCanceller for SlowConnectCanceller {
        fn cancel(&self) {
            self.cancel_requested.store(true, Ordering::SeqCst);
            self.registered.arrive(0);
        }
    }

    /// Models `CodexAgentBackend::start_turn`'s own shape: block through the
    /// connect window, THEN check whether a cancel arrived during it, and
    /// bail out BEFORE ever reaching the (potentially arbitrarily long)
    /// actual turn if it did.
    struct SlowConnectBackend {
        inner: MockBackend,
        cancel_requested: Arc<AtomicBool>,
        cancel_registered: Arc<Latch>,
        gate: Arc<ConnectGate>,
        entered_connect: Arc<Latch>,
        entered_turn: Arc<AtomicUsize>,
    }

    impl AgentBackend for SlowConnectBackend {
        fn start_thread(
            &mut self,
            spec: &ThreadSpec,
        ) -> Result<BackendThreadId, AgentBackendError> {
            self.inner.start_thread(spec)
        }

        fn start_turn(
            &mut self,
            thread: &BackendThreadId,
            spec: &CycleSpec,
        ) -> Result<TurnEvent, AgentBackendError> {
            self.entered_connect.arrive(0);
            self.gate.block_until_released();
            if self.cancel_requested.load(Ordering::SeqCst) {
                return Err(AgentBackendError::BackendUnavailable {
                    detail: "cycle cancelled during connect: never reached its turn".to_string(),
                });
            }
            self.entered_turn.fetch_add(1, Ordering::SeqCst);
            self.inner.start_turn(thread, spec)
        }

        fn resume(&mut self, reply: ToolReply) -> Result<TurnEvent, AgentBackendError> {
            self.inner.resume(reply)
        }

        fn canceller(&self) -> Box<dyn BackendCanceller> {
            Box::new(SlowConnectCanceller {
                cancel_requested: Arc::clone(&self.cancel_requested),
                registered: Arc::clone(&self.cancel_registered),
            })
        }
    }

    /// The race matrix's missing row: a cancel that arrives while a cycle is
    /// still "connecting" must reach a typed terminal without ever waiting
    /// out the (here: indefinite) actual turn — pinning the fix for the
    /// review finding that `SubagentCancel` during the connect window
    /// degraded into blocking for a full model turn.
    #[test]
    fn handler_cancel_during_connect_reaches_typed_terminal_without_a_full_turn_wait() {
        let fx = Fixture::new();
        let gate = Arc::new(ConnectGate::default());
        let entered_connect = Arc::new(Latch::default());
        let entered_turn = Arc::new(AtomicUsize::new(0));
        let cancel_requested = Arc::new(AtomicBool::new(false));
        let cancel_registered = Arc::new(Latch::default());
        let backend = SlowConnectBackend {
            inner: MockBackend::completing(CycleResultPayload::Absent),
            cancel_requested: Arc::clone(&cancel_requested),
            cancel_registered: Arc::clone(&cancel_registered),
            gate: Arc::clone(&gate),
            entered_connect: Arc::clone(&entered_connect),
            entered_turn: Arc::clone(&entered_turn),
        };
        let mut handler = fx.handler_with(one_shot_queue(Box::new(backend)));

        let cycle = spawn_async(&mut handler, "worker").expect("admitted immediately");
        entered_connect.wait_for(1, "the cycle to reach the simulated connect window");

        let elapsed = std::thread::scope(|scope| {
            let cancel_thread = scope.spawn(|| {
                let start = std::time::Instant::now();
                handler.cancel_cycle(cycle);
                start.elapsed()
            });
            // Only release the connect window once the cancel has DEFINITELY
            // registered — otherwise a release winning the race would let
            // connect finish uncancelled and the test would pass for the
            // wrong reason.
            cancel_registered.wait_for(1, "the cancel to register before connect closes");
            gate.release();
            cancel_thread.join().expect("cancel_cycle must not panic")
        });

        assert!(
            elapsed < Duration::from_secs(5),
            "cancel-during-connect must not block for anything resembling a full model turn: \
             {elapsed:?}"
        );
        assert_eq!(
            entered_turn.load(Ordering::SeqCst),
            0,
            "a cancel requested during connect must stop the cycle BEFORE it ever reaches its \
             (potentially very long) actual turn"
        );
        assert_eq!(
            handler.subagent_await(cycle),
            Err(SpawnError::SpawnCancelled(cycle)),
            "cancel-during-connect still reaches a typed terminal"
        );
    }
}
