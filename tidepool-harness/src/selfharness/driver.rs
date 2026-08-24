//! The runtime driver spine — the outer `render`/`loop` alternation over the
//! OUTER Harness-monad resident session, servicing each `runLLMTurn` hole by
//! driving a per-loop answerer [`crate::harness::Harness`] node (an ordinary
//! Agent node, reusing `run_to_hole_or_done`) to a `finalize` and delivering
//! the result back in-heap to resume `loop`.
//!
//! ONE SESSION (the one-session collapse, `plans/one-session.md`): the outer
//! session and every loop's answerer node share the SAME resident machine,
//! not two separate sessions bridged by value. The outer session is
//! node-less and registry-owned ([`Harness::adopt_session`]); each answerer
//! node ATTACHES to it ([`Harness::force_attached`]) instead of getting a
//! session of its own, running its turns as a per-loop REALM on the shared
//! machine ([`Harness::set_node_realm`], applied at every checkout by
//! `Harness::run_checked_out`). Heaps are never copied and a `finalize`d
//! value never round-trips through JSON to cross: a data answer crosses via
//! [`Harness::take_finalized_value_keep_open`] (bridged, as before); a
//! `finalize`d CLOSURE crosses via
//! [`Harness::take_finalized_handle_keep_open`] — a
//! [`tidepool_codegen::jit_machine::ValueHandle`] over the payload's own
//! machine-side root, never deep-forced or serialized — delivered straight
//! into the OUTER session's parked `runLLMTurn` continuation via
//! [`ResidentSession::resume`] (data) or [`ResidentSession::resume_handle`]
//! (handle). This is what makes `runLLMTurn @(State -> State)` work
//! end-to-end: the closure is born in, and never leaves, the loop's own heap.
//! Retiring an answerer node at loop end is realm SCOPE EXIT
//! ([`Harness::terminate_node`] → `close_realm`), never session removal —
//! the outer session outlives every answerer it hosts, its own lifetime
//! bounded only by periodic machine ROTATION at a fragment ceiling
//! ([`Self::machine_maintenance`]), not by any one loop's answerer.
//!
//! # Async turn loop, sync-blocking operator gate
//!
//! [`SelfHarnessDriver::run_loop`]/[`SelfHarnessDriver::run_one_loop_iteration`] are
//! `async fn` and `.await` the nested [`Harness`]'s turn loop
//! ([`service_typed_request_suspension`](SelfHarnessDriver::service_typed_request_suspension)) directly
//! — every entry point here must still be called from a thread with an
//! ACTIVE tokio runtime (`#[tokio::main]`/`#[tokio::test(flavor =
//! "multi_thread")]`), because the [`crate::selfharness::operator::OperatorGate`]
//! park (`present_form`, which also covers the between-loops gate — see
//! [`SelfHarnessDriver::between_loops_gate`]) is SYNC-BLOCKING by frozen contract
//! (a web gate parks a channel), so a call into it from this async code runs
//! under `tokio::task::block_in_place` — a genuinely blocking call yielding
//! the tokio worker to other tasks, not a sync-to-async bridge — which
//! requires the multi-thread runtime flavor. The resident JIT run/resume
//! calls the loop also drives are CPU-blocking and sit inside these `async
//! fn`s unchanged (they already blocked a tokio worker before this
//! conversion); see [`Self::drive_agent_session_to_finalize`]'s doc for why they
//! are not `spawn_blocking`'d.
//!
//! # Boot fold and entry selection (PRD 20 S1-L5)
//!
//! A run's durable journal is READ here, and only here: `record`
//! (`Tidepool.Journal`) stays write-only on the authored surface.
//! [`SelfHarnessDriver::open_run_journal`] loads and folds every SEGMENT a
//! run id owns (see [`crate::selfharness::resume`]'s module doc) to the last
//! entry per `(kind, key)`, and builds the appending handler over this
//! process's own freshly allocated segment, seeded past what every existing
//! segment already holds — one seam, one `AcquiredLease`, so the fold and the
//! appends cannot desync. The FIRST cycle after boot then consumes that fold
//! ([`SelfHarnessDriver::take_loop_entry`]): a non-empty one compiles the
//! wider `Loaded.resumeLoop __selfHarnessResume __selfHarnessState` entry
//! against a harness that declares it, and an empty one compiles exactly the
//! `Loaded.loop __selfHarnessState` entry every harness has always compiled.
//! A non-empty fold against a harness with NO `resumeLoop` is refused at
//! bootstrap ([`DriverError::ResumeEntryMissing`]) rather than silently
//! redoing finished work.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use parking_lot::Mutex;
use serde_json::Value as Json;
use tidepool_bridge::ToCore;
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{ResidentHole, ResidentOutcome};

use crate::engine::{
    self, ClassifiedSuspension, CompiledTurn, EngineConfig, EngineError, InvocationExit,
    SuspensionRouting, TurnOutcome,
};
use crate::harness::{AnswerContract, Harness, HarnessError, Session, OUTER_REALM};
use crate::log::Actor;
use crate::selfharness::harness_source::HarnessSource;
use crate::selfharness::lifecycle::SelfHarnessState;
use crate::selfharness::observer::{AskId, Event, FormSource, Observer};
use crate::selfharness::operator::{
    DelegationPhase, FieldShape, FormShape, OperatorGate, StdinGate,
};
use crate::selfharness::persistence::{self, PersistenceError};
use crate::selfharness::state_cross;
use crate::timing;
use crate::tree::{FanBadge, HoleId, NodeId};

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("self-harness driver: {0}")]
    Session(String),
    #[error(transparent)]
    Agent(#[from] HarnessError),
    #[error(transparent)]
    Classify(#[from] engine::ClassifyError),
    /// The prior loop's `State` JSON failed the author's `FromJSON State`
    /// instance when re-spliced — a distinct, actionable failure (the
    /// author's `ToJSON`/`FromJSON State` are not inverse) rather than an
    /// opaque "loop run failed". Detected via
    /// [`state_cross::STATE_DECODE_SENTINEL`]. Carries the Aeson decode error.
    #[error("self-harness State decode failed (ToJSON/FromJSON State not inverse): {0}")]
    StateDecode(String),
    /// `State` json save/restore failed (disk full, permissions, or a
    /// malformed persisted file) — distinct from [`DriverError::StateDecode`],
    /// which is the author's `FromJSON State` instance rejecting otherwise
    /// well-formed JSON.
    #[error("self-harness persistence: {0}")]
    Persistence(#[from] PersistenceError),
    /// The driver is [`SelfHarnessState::Poisoned`]: a prior cycle failed and
    /// recovery could not rebuild a usable outer session. Every public entry
    /// point returns this instead of running.
    #[error("self-harness driver poisoned, recovery failed: {0}")]
    Poisoned(String),
    /// The boot fold ([`Self::open_run_journal`](SelfHarnessDriver::open_run_journal))
    /// found recorded steps, but the harness declares no `resumeLoop` entry to
    /// inject them through — so a run would silently REDO finished work. Refused
    /// at boot, before any cycle runs, naming both: the harness that needs
    /// the entry and where the run's segments (see [`crate::selfharness::resume`])
    /// that carry the entries live.
    ///
    /// This refusal IS the PRD's "resume should not be able to forget to look"
    /// property, realized as a boot failure rather than as a type.
    #[error(
        "self-harness resume: {journal} has {entries} recorded step(s) for run {run_id}, but \
         {harness} declares no `resumeLoop :: ResumeFold -> State -> Harness State` to inject \
         them through — refusing to redo finished work. Add the entry (`resumeLoop _ = loop` is \
         the honest opt-out), or retire the run's lease to start a fresh one."
    )]
    ResumeEntryMissing {
        harness: String,
        journal: String,
        run_id: String,
        entries: usize,
    },
    /// Loading a run's journal segments failed — either enumerating them
    /// ([`PersistenceError`], an I/O failure against the log dir itself) or
    /// loading one ([`tidepool_handlers::JournalLoadError`] — real corruption,
    /// a torn line that isn't a segment's own final one).
    #[error("self-harness run journal: {0}")]
    RunJournal(#[from] crate::selfharness::resume::RunJournalError),
    /// The boot fold's JSON failed `Tidepool.Resume`'s `FromJSON ResumeFold`
    /// when re-spliced — distinct from [`DriverError::StateDecode`], which is
    /// the AUTHOR's instance rejecting their own state. This one means the
    /// driver's encoder
    /// ([`crate::selfharness::resume::ResumeFold::to_json`]) and the stdlib's
    /// hand-written decoder disagree on the wire contract: a Tidepool bug, not
    /// an authoring one. Detected via
    /// [`state_cross::RESUME_DECODE_SENTINEL`].
    #[error("self-harness resume fold decode failed (driver/Tidepool.Resume wire mismatch): {0}")]
    ResumeDecode(String),
}

/// Map an outer-session run error string to a typed [`DriverError`]: a message
/// carrying [`state_cross::STATE_DECODE_SENTINEL`] becomes
/// [`DriverError::StateDecode`], everything else a generic
/// [`DriverError::Session`] with `ctx` for locus.
/// A message carrying [`state_cross::RESUME_DECODE_SENTINEL`] likewise becomes
/// [`DriverError::ResumeDecode`] — the two sentinels are distinct prefixes
/// precisely so the two failures stay distinguishable (see
/// [`state_cross::RESUME_DECODE_SENTINEL`]'s doc).
fn map_run_error(ctx: &str, msg: String) -> DriverError {
    if let Some(idx) = msg.find(state_cross::STATE_DECODE_SENTINEL) {
        let detail = &msg[idx + state_cross::STATE_DECODE_SENTINEL.len()..];
        DriverError::StateDecode(detail.trim().to_string())
    } else if let Some(idx) = msg.find(state_cross::RESUME_DECODE_SENTINEL) {
        let detail = &msg[idx + state_cross::RESUME_DECODE_SENTINEL.len()..];
        DriverError::ResumeDecode(detail.trim().to_string())
    } else {
        DriverError::Session(format!("{ctx}: {msg}"))
    }
}

/// One full `render` → `loop` → (service each `runLLMTurn` hole) → `render`
/// cycle's outcome — [`SelfHarnessDriver::run_one_loop_iteration`]'s return value,
/// what a spine test asserts against. `state_json` is what the caller
/// persists and threads into the NEXT cycle's `prior_state`.
#[derive(Debug, Clone)]
pub struct LoopIterationOutcome {
    /// [`SelfHarnessDriver::render_framing`]'s composed text BEFORE this
    /// cycle's `loop` ran — the prompt the loop's `runLLMTurn` answerer(s)
    /// implicitly worked under.
    pub prompt_before: String,
    /// `loop`'s returned `State`, serialized ([`state_cross::state_out`]).
    pub state_json: Json,
    /// [`SelfHarnessDriver::render_framing`]'s composed text AFTER this
    /// cycle's `loop` completed — reflects the new `State` reaching the next
    /// render.
    pub prompt_after: String,
    /// The runtime-owned emergency compaction turn's `Text`, if this
    /// cycle's answerer session crossed the configured context-window
    /// threshold MID-LOOP; `None` otherwise. When set, the loop CONTINUED
    /// under the summary (in-place relief — no abort). `prompt_after` already
    /// reflects it (rendered with the updated `lastCompaction`) — this field
    /// is what a caller/test asserts against directly, and what
    /// [`SelfHarnessDriver::run_one_loop_iteration`] carries forward as the NEXT
    /// cycle's `lastCompaction` (`self.last_compaction`, not a threaded
    /// parameter — see that method's doc).
    pub compaction: Option<String>,
}

/// The outer session's own bootstrapped [`crate::harness::Session`] plus the
/// [`EngineConfig`] it was compiled against (kept alongside it — every later
/// fragment compile needs the same `extract_bin`/`include`/decls) and the
/// harness module's name (every later fragment's `qualified ... as Loaded`
/// import, see [`SelfHarnessDriver::compile_outer`]).
/// How a serviced `runLLMTurn` hole's answer travels back into the loop's
/// parked continuation: a bridged data value (the pre-collapse path, still
/// right for data), or a machine-side handle whose payload is DELIVERED
/// verbatim on the shared heap — the closure path (pillar B; the reason the
/// collapse exists).
pub enum FinalAnswer {
    Value(Value),
    Handle(tidepool_runtime::session::RootCustody),
}

// --- Green threads (PRD 20 S1-L4) ---------------------------------------
//
// The scheduler is entirely LOCAL to one `run_loop_fragment_inner` call —
// every thread a loop spawns is structured-concurrency-scoped to that one
// `loop` fragment run; nothing here survives as driver state across loops.

/// Which control-flow chain a suspension belongs to. Chain is invariant
/// across a resume (resuming a hole continues the SAME chain into whatever
/// it suspends on next); only starting a freshly spawned thread introduces a
/// new one. Needed because `AsyncDoneWith`'s own leading `Int` field is
/// always the dummy `0` `asyncSpawn` bakes in (the wrapping closure is built
/// before its real thread id is known, `tidepool-mcp/src/effect_defs.rs`) —
/// the driver identifies which thread settled by WHICH chain reached
/// `AsyncDoneWith`, never by decoding that field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GreenChain {
    /// The outer `loop`'s own top-level continuation, or transitively
    /// whatever spawned the thread that (recursively) spawned this chain.
    Primary,
    Thread(i64),
}

/// The driver's bookkeeping for one green thread: which realm its frames
/// park under (the unit `cancel` closes) and its terminal-state sum.
struct GreenThread {
    realm: tidepool_codegen::jit_machine::RealmId,
    state: GreenThreadState,
}

enum GreenThreadState {
    Running,
    Settled(GreenResult),
    Cancelled,
}

/// A settled green thread's result, as the thread table holds it.
///
/// Deliberately NOT [`FinalAnswer`]: that type's `Handle` arm carries a
/// custody token, which is right at the finalize seam where the delivery IS
/// the ownership transfer. A thread's result is read as many times as it is
/// waited on (`poll` then `wait`; two waiters on one thread), and its root is
/// owned by the SESSION realm so it outlives the thread's own realm closing.
/// So the table holds the root itself and each delivery borrows it — see
/// [`tidepool_runtime::session::ResidentSession::resume_handle_borrowed`].
/// Holding a `RootCustody` here and copying it out per waiter would hand the
/// same root to several owners; the token makes that a compile error, which
/// is how this distinction was found.
enum GreenResult {
    Value(Value),
    Root(tidepool_codegen::jit_machine::ValueHandle),
}

/// One already-produced suspension (or completion) waiting to be classified
/// and serviced — the scheduler's FIFO ready queue. `chain` is threaded
/// through unchanged so a later `AsyncDoneWith`/wake can attribute correctly;
/// order is the driver's business only — the representation-pinning
/// contract (`plans/self-iterating-harness/20-s1l4-green-threads.md`) is
/// that resuming ready work in EITHER order produces identical results, so
/// this queue just picks one (FIFO).
struct GreenReady {
    chain: GreenChain,
    outcome: ResidentOutcome,
}

/// How a serviced green suspension's own resume is DELIVERED — the one
/// genuinely plane-specific seam in green servicing (dup-e finding 2).
/// Thread frames and the authored outer loop resume RAW on the shared
/// machine session and re-enter the ready queue; the answerer NODE's own
/// chain must resume through the node-aware path
/// (`Harness::resume_with_value`/`resume_with_borrowed_root`), which
/// restores the node's runtime resource scope and keeps its pending record
/// truthful. Everything ABOVE this seam — constructor decode, thread-table
/// transitions, realm minting, waiter wakes — is ONE implementation
/// ([`SelfHarnessDriver::service_green_hole`]), not two.
enum GreenDelivery<'a> {
    Raw,
    Node { node: NodeId, hole: &'a HoleId },
}

/// What crosses on a green resume: a bridged value, or a borrowed
/// session-owned root (a settled thread's in-heap result).
enum GreenAnswer {
    Value(Value),
    BorrowedRoot(tidepool_codegen::jit_machine::ValueHandle),
}

/// What one popped ready item resolved to, and what the scheduler owes it in
/// response — the classified-hole dispatcher's return value instead of each
/// arm independently pushing to `ready`/breaking the loop. A new arm that
/// computes a next outcome and forgets to hand it back through one of these
/// is a compile error: it has nothing else to return. (The motivating
/// incident this subsumes: a merge brought in arms written against an older
/// loop shape whose `outcome = ...` fed the next iteration; the resume was
/// computed correctly and then silently dropped, the only signal a `value
/// assigned to outcome is never read` warning that adding `mut` would have
/// shipped past.)
///
/// [`SuspensionRouting::Green`] is the deliberate exception, checked and rejected
/// before this was written for the rest: a single Green suspension can
/// settle into zero, one, or two ready continuations (a spawn resumes the
/// spawner AND starts the new thread; a join with no terminal candidate
/// parks with none and touches no hole; a settle or cancel can wake an
/// arbitrary number of parked waiters), and it mutates the thread table and
/// waiter map alongside `ready`. No two-or-three-variant sum expresses
/// "zero to N pushes plus a table mutation" without degrading to a `Vec` or
/// a payload-free `Handled` marker that types nothing a `Result<(),
/// DriverError>` didn't already type — so
/// [`SelfHarnessDriver::service_green_hole`] keeps owning `ready`/the
/// thread table/the waiter map directly instead of returning one of these.
enum ServicedSuspension {
    /// The popped item was itself terminal — the PRIMARY chain's `loop` has
    /// finished.
    Completed { result: Value, table: DataConTable },
    /// The hole was resumed; its next outcome re-enters the ready queue
    /// under the same chain.
    Resumed(GreenReady),
    /// The hole was left parked, unresumed — reinserted into the ready
    /// queue so a later iteration revisits it (`RepoEventAwait`'s
    /// empty-poll case).
    LeaveParked(GreenReady),
}

/// Deep sentinel scan mirroring [`Harness::finalize_is_closure`] — whether
/// `request`'s field `idx` carries the tolerant suspend bridge's
/// `CLOSURE_SENTINEL` placeholder (a live closure kept in-heap) rather than
/// plain data. Green threads bypass the `Harness` node/convo abstraction
/// (they run on the shared OUTER session directly), so this operates on the
/// raw suspended request instead of a node's stashed pending state.
fn green_field_is_closure(request: &Value, idx: usize) -> bool {
    tidepool_codegen::heap_bridge::field_contains_closure_sentinel(request, idx)
}

/// Pull a plain `Int` field out of a Green request Con — every
/// thread-id-shaped field (`AsyncJoinAnyWith`'s elements, `AsyncStatusWith`/
/// `AsyncResultWith`/`AsyncCancelWith`'s leading arg) shares this decode.
fn green_int_field(request: &Value, idx: usize, table: &DataConTable) -> i64 {
    let Value::Con(_, fields) = request else {
        return 0;
    };
    fields
        .get(idx)
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_i64())
        .unwrap_or(0)
}

/// Pull an `[Int]` field out of a Green request Con (`AsyncJoinAnyWith`'s
/// sole field).
fn green_int_list_field(request: &Value, idx: usize, table: &DataConTable) -> Vec<i64> {
    let Value::Con(_, fields) = request else {
        return Vec::new();
    };
    fields
        .get(idx)
        .map(|p| tidepool_runtime::value_to_json(p, table, 0))
        .and_then(|j| j.as_array().cloned())
        .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default()
}

/// Round-scoped scheduler state for green threads spawned by an ANSWERER
/// window's own block (`async (fork @T brief)` and friends) — the
/// answerer-plane sibling of the loop-scoped locals
/// [`SelfHarnessDriver::run_loop_fragment_inner`] owns for the AUTHORED
/// outer loop. The plane split mirrors the fork machinery's own
/// ([`SelfHarnessDriver::drive_fork_child_agent_session`] vs
/// [`SelfHarnessDriver::service_outer_fanout`]): thread chains are serviced
/// by the SAME [`SelfHarnessDriver::service_green_hole`] (raw session
/// resumes — correct for thread frames, which live under their own realms),
/// while every resume of the NODE's own turn goes through the node-aware
/// path (`Harness::resume_with_value`/`resume_with_borrowed_root`), which
/// restores the node's realm/scope and keeps its pending-hole record
/// truthful — a raw `with_session` resume would run the node's continuation
/// under `OUTER_REALM` and leave its bookkeeping stale.
///
/// ROUND-scoped, exactly as the outer scheduler is fragment-scoped: one
/// round = one compile = one `DataConTable`/asks sidecar shared by every
/// chain, which is what lets thread suspensions classify against the node's
/// pending artifacts. A thread still running when its round ends is SWEPT
/// (realm closed, frames dropped) — spawn and wait belong in the same
/// block, and the corrective prompt says so when anything was dropped.
struct ModelRoundGreenThreadScheduler {
    threads: HashMap<i64, GreenThread>,
    /// THREAD-chain joiners only. The node's own `wait` never registers
    /// here — [`SelfHarnessDriver::service_green_hole`]'s
    /// `wake_green_waiters` resumes waiters RAW, which must never touch the
    /// node chain; the node's blocked join is instead re-checked (a pure
    /// winner scan, no session touch) each scheduler iteration.
    waiters: HashMap<i64, Vec<(GreenChain, String)>>,
    ready: VecDeque<GreenReady>,
    next_tid: i64,
    next_thread_realm: u64,
}

/// Answerer-plane thread realms mint their low bits from this process-wide
/// sequence, starting at `1 << 32` — disjoint by construction from the outer
/// scheduler's per-fragment counters (which start at 1 and stay tiny), so an
/// answerer window's threads can never collide with the authored loop's own
/// live threads on the one shared session. Each round grabs a `1 << 20`
/// block; no round comes near exhausting one.
static GREEN_ROUND_REALM_SEQ: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1 << 32);

impl ModelRoundGreenThreadScheduler {
    fn new() -> Self {
        ModelRoundGreenThreadScheduler {
            threads: HashMap::new(),
            waiters: HashMap::new(),
            ready: VecDeque::new(),
            next_tid: 1,
            next_thread_realm: GREEN_ROUND_REALM_SEQ
                .fetch_add(1 << 20, std::sync::atomic::Ordering::Relaxed),
        }
    }
}

/// How [`SelfHarnessDriver::service_green_round`] hands control back to
/// the answerer dispatcher: the node's own turn parked on a non-Green hole
/// (route it), ran to completion without finalizing (corrective retry), a
/// thread's fork was refused by the session's fork budget (abort the
/// block, corrective retry naming the budget), the block misused the
/// async surface (abort the block, corrective retry naming the mistake),
/// or a thread's fork CHILD ran and ended in `InvocationExit` — round
/// exhaustion, a non-answer ending, its own provider call failing — rather
/// than finalizing (abort the block, corrective retry naming the child by
/// its path; the child's own node already retired via `node_failed` before
/// this variant is ever produced).
enum GreenRoundExit {
    NodeParked,
    NodeDone,
    /// Carries the ACTUAL refusal text `check_fork_budgets` built —
    /// `fork_subtree_refusal` when the tree-wide cap fired,
    /// `fork_budget_refusal` when the per-window pool did — never rebuilt
    /// downstream (F8: rebuilding always guessed `fork_budget_refusal`,
    /// misreporting a subtree exhaustion as a per-window one).
    ForkBudgetRefused {
        msg: String,
    },
    AsyncMisuse {
        msg: String,
    },
    /// A thread's fork child ended in `InvocationExit` — carries the plain-
    /// language corrective [`fork_child_failure_corrective`] built. Sibling
    /// threads in the round are unaffected up to this point (they already
    /// ran and, if they finalized, their own node already reports so via
    /// `node_finalized`) — only the round's OUTSTANDING threads are swept
    /// when this abort fires, same as [`Self::ForkBudgetRefused`].
    ForkChildFailed {
        msg: String,
    },
}

/// One serviced green suspension's outcome, distinguishing MODEL-ATTRIBUTABLE
/// misuse (`asyncResult` before a settle, a `wait` on a dropped handle, …)
/// from a mechanism failure: the typed-request agent's scheduler path turns `Misuse` into a
/// block-abort + corrective (the same loud-refusal shape the fork budget
/// uses — one model slip must not end the whole run), while the AUTHORED
/// outer plane maps it back to a hard error (authored code fails loud, it
/// is not coached).
enum GreenHoleServiced {
    /// Serviced; the bool is the old return — `true` = the node chain is
    /// blocked on a join with no terminal candidate.
    Proceed(bool),
    Misuse(String),
}

/// One serviced THREAD-chain ready item's outcome — the thread-plane
/// sibling of [`GreenHoleServiced`], widened with the fork-budget refusal
/// that thread forks can hit and the fork-child-failure corrective.
enum ThreadServiced {
    Continue,
    /// The ACTUAL refusal text `check_fork_budgets` built — see
    /// [`GreenRoundExit::ForkBudgetRefused`]'s doc (F8).
    BudgetRefused {
        msg: String,
    },
    Misuse(String),
    /// A thread's fork child ended in `InvocationExit` — see
    /// [`GreenRoundExit::ForkChildFailed`]'s doc.
    ChildFailed {
        msg: String,
    },
}

/// One answerer WINDOW's fork budget — total children across all rounds,
/// drawn on by direct `fork`/`forkAll` servicing ([`SelfHarnessDriver::drain_answerer_fork`])
/// and green-thread forks ([`SelfHarnessDriver::service_thread_ready`])
/// alike. Spending happens BEFORE the spawn, so the refusal costs nothing.
struct ForkBudget {
    cap: u32,
    spent: u32,
}

impl ForkBudget {
    /// How many children `routing` would spawn: a single fork is 1, a fanout
    /// its fan (`Bounded`/`Dynamic` badges fall back to the decoded prompt
    /// count — the number of children that would actually be driven).
    fn cost(routing: &SuspensionRouting) -> u32 {
        match routing {
            SuspensionRouting::Fork { fan: None, .. } => 1,
            SuspensionRouting::Fork {
                fan: Some(FanBadge::Exact { n }),
                ..
            } => *n,
            SuspensionRouting::Fork { prompts, .. } => prompts.len() as u32,
            _ => 0,
        }
    }

    /// Spend `cost` children if the pool covers them; `false` (nothing
    /// spent) when it doesn't.
    fn try_spend(&mut self, cost: u32) -> bool {
        if self.spent.saturating_add(cost) > self.cap {
            return false;
        }
        self.spent += cost;
        true
    }
}

/// What [`SelfHarnessDriver::drive_agent_session_to_finalize`] does when its node
/// suspends on something other than `finalize` — the one axis the sol
/// cross-family review's finding 4 confirmed genuinely differs between the
/// driver's model-session pumps (everything else — round caps, provider
/// handling, compile correctives, finalize detection, event emission — is
/// now the ONE shared loop).
#[derive(Debug, Clone, Copy)]
enum AgentSessionExitPolicy {
    /// The reused single-hole answerer, a sequential branch/branch-fanout
    /// child, and a recursive fork child: service every suspension this
    /// driver knows how to (`askUser`, `note`, `fork`, green threads) via the
    /// ordinary dispatcher.
    Interactive,
    /// A concurrent `runLLMTurnFork`/`runLLMTurnFanout` child
    /// ([`SelfHarnessDriver::drive_fanout_child`], v1 scope): `finalize`
    /// only. No operator-gate serialization or fork bookkeeping across
    /// siblings racing the same machine, so any other suspension folds
    /// straight to [`InvocationExit::NotFinalized`] DATA at this child's own
    /// position instead of being serviced. `idx` names the child in the
    /// resulting message.
    FinalizeOnly { idx: usize },
}

/// Put ONE child answer into the shape the parked fork continuation
/// expects, against the caller-supplied round table (unlike
/// `Harness::wrap_fork_answer`, which derives its table from node-pending
/// state) — a thin `DriverError` wrapper over the ONE shared implementation,
/// [`engine::wrap_fork_answer`] (sol cross-family review finding 9d).
fn wrap_fork_value(
    source: engine::ForkSource,
    value: Value,
    table: &DataConTable,
) -> Result<Value, DriverError> {
    engine::wrap_fork_answer(source, value, table).map_err(|e| DriverError::Session(e.to_string()))
}

/// Drive `count` children CONCURRENTLY up to `cap` at once
/// (`buffer_unordered`), then re-sort the results back to DECLARATION order
/// — completion order is nondeterministic and must never be observable in
/// the resumed answer. `child` is called once per index; a free function
/// (no `self`) so the caller supplies whatever `&self`-reachable state each
/// child needs via its own capture, same reasoning
/// [`SelfHarnessDriver::drive_fanout_child`]'s doc gives for why this is
/// never `Arc<Self>`/`tokio::spawn`. THE ordering/concurrency shell
/// [`SelfHarnessDriver::service_outer_fanout`] uses.
async fn drive_concurrent<T, F, Fut>(cap: usize, count: usize, child: F) -> Vec<(usize, T)>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let child = &child;
    let mut results: Vec<(usize, T)> = stream::iter(0..count)
        .map(|idx| async move { (idx, child(idx).await) })
        .buffer_unordered(cap)
        .collect()
        .await;
    results.sort_by_key(|(idx, _)| *idx);
    results
}

/// The character budget a fork child's derived label slug truncates to —
/// short enough that a long brief still reads as one path segment, long
/// enough to stay recognizable alongside a sibling's. See
/// [`SelfHarnessDriver::fork_child_label`].
const FORK_LABEL_SLUG_BUDGET: usize = 24;

/// A fork child's own GUI path segment: `f<idx>-<slug>`, where `slug` is an
/// ASCII, lowercase, hyphen-joined prefix of the fork's authored BRIEF (not
/// the composed hole card) — non-alphanumeric runs collapse to one hyphen,
/// leading/trailing hyphens are trimmed, and a brief with no alphanumeric
/// content at all (or an empty one) falls back to the bare index so the
/// segment is never empty. See [`SelfHarnessDriver::fork_child_label`] for
/// how this combines with the parent's own path.
fn fork_child_path_segment(idx: u32, brief: &str) -> String {
    let mut slug = String::new();
    let mut pending_hyphen = false;
    for c in brief.chars() {
        if slug.len() >= FORK_LABEL_SLUG_BUDGET {
            break;
        }
        if c.is_ascii_alphanumeric() {
            if pending_hyphen && !slug.is_empty() {
                slug.push('-');
            }
            pending_hyphen = false;
            slug.push(c.to_ascii_lowercase());
        } else {
            pending_hyphen = true;
        }
    }
    if slug.is_empty() {
        format!("f{idx}")
    } else {
        format!("f{idx}-{slug}")
    }
}

impl SelfHarnessDriver {
    /// The step-2 spawn admission: per-session fan pool AND whole-subtree
    /// descendant budget, checked (and the subtree spent) atomically at the
    /// one moment children are about to exist. `Some(corrective)` = refused,
    /// nothing spent; `None` = both budgets debited, spawn may proceed.
    ///
    /// The subtree reservation is a compare-exchange loop, not a
    /// check-then-act (F10): two concurrent sharers of one `fork_subtree`
    /// counter (a window driving its own fork children concurrently) can no
    /// longer both observe headroom and both add past the cap — each
    /// attempt re-reads the counter on a lost race and re-checks against the
    /// cap before retrying. `budget` (the per-window pool) is `&mut`, so it
    /// has no such race — but its check-and-spend still runs AFTER the
    /// subtree reservation, so a window-budget refusal rolls the subtree
    /// reservation back rather than leaving it charged for a child that
    /// will never spawn.
    fn check_fork_budgets(
        &self,
        budget: &mut ForkBudget,
        cost: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
        ty_label: &str,
    ) -> Option<String> {
        use std::sync::atomic::Ordering;
        let mut spent = fork_subtree.load(Ordering::Relaxed);
        loop {
            let new_spent = spent.saturating_add(cost);
            if new_spent > self.fork_subtree_cap {
                return Some(fork_subtree_refusal(
                    spent,
                    self.fork_subtree_cap,
                    cost,
                    ty_label,
                ));
            }
            match fork_subtree.compare_exchange_weak(
                spent,
                new_spent,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => spent = actual,
            }
        }
        if !budget.try_spend(cost) {
            // Roll back: the subtree reservation was provisional on the
            // window pool also covering `cost`, and it doesn't.
            fork_subtree.fetch_sub(cost, Ordering::Relaxed);
            return Some(fork_budget_refusal(
                budget.spent,
                budget.cap,
                cost,
                ty_label,
            ));
        }
        None
    }
}

/// The refusal corrective when the WHOLE fork tree's descendant budget is
/// spent — distinct from the per-session pool below, so the model knows
/// the boundary is tree-wide, not something a deeper fork escapes.
fn fork_subtree_refusal(spent: u32, cap: u32, needed: u32, ty_label: &str) -> String {
    let ty_disp = display_ty(ty_label);
    format!(
        "Fork budget exhausted for this WHOLE tree of sessions: {spent} of {cap} \
         descendant sessions are already spawned across all depths, and that block \
         needed {needed} more. The block was ABORTED (top-level declarations from \
         earlier rounds persist; the aborted block's bindings are lost). Do not \
         fork again anywhere in this tree — finalize with what you have: evaluate \
         `finalize @{ty_disp} value`."
    )
}

/// The refusal corrective for a fork that would exceed the window's budget:
/// what happened, what survives, and the one useful next step. `cap == 0`
/// (the `fork_depth >= max_fork_depth` case, `drive_agent_session_to_finalize`)
/// is a DEPTH refusal, not a per-session pool refusal — depth-1..7 children
/// fork fine, so the reason must be the tree's depth, never "nested forking
/// is not supported" (false, and contradicts the Fork card).
fn fork_budget_refusal(spent: u32, cap: u32, needed: u32, ty_label: &str) -> String {
    let ty_disp = display_ty(ty_label);
    if cap == 0 {
        return format!(
            "Forking is not available in THIS session: the fork tree has reached its \
             maximum depth, so this session must answer its own brief directly. The \
             block was ABORTED (top-level declarations from earlier rounds persist; \
             the aborted block's bindings are lost). Answer with what you can \
             establish yourself: evaluate `finalize @{ty_disp} value`."
        );
    }
    format!(
        "Fork budget exhausted: this session has spawned {spent} of its {cap} fork \
         children, and that block needed {needed} more, so the block was ABORTED \
         (top-level declarations from earlier rounds persist; the aborted block's \
         bindings are lost). Do not fork again — finalize with what you have: \
         evaluate `finalize @{ty_disp} value`."
    )
}

/// The corrective when a fork child ends in `InvocationExit` — round
/// exhaustion, a non-answer ending, or its own provider call failing —
/// rather than finalizing: what happened, what survives, and the one
/// useful next step, mirroring [`fork_budget_refusal`]'s shape. Plain
/// composed-industry-terms language only (docs/GLOSSARY.md's prompt
/// rules): `path` names the child by its derived GUI/tree path, never an
/// internal identifier like `InvocationExit` or a constructor name.
fn fork_child_failure_corrective(path: &str, exit: &InvocationExit, ty_label: &str) -> String {
    let ty_disp = display_ty(ty_label);
    let what = match exit {
        InvocationExit::RoundsExhausted(_) => {
            "exhausted its model rounds without finalizing an answer"
        }
        InvocationExit::NotFinalized(_) => "ended its session without finalizing an answer",
        InvocationExit::Cancelled(_) => "was cancelled before it could finalize an answer",
        InvocationExit::RuntimeFailure(_) => "hit a runtime failure in its own session",
    };
    format!(
        "The forked child at {path} {what}. Its result is lost, and this block was \
         ABORTED (top-level declarations from earlier rounds persist; the aborted \
         block's bindings are lost). You can re-fork with an adjusted brief, or \
         proceed without that child's result: evaluate `finalize @{ty_disp} value`."
    )
}

/// The plain-language round-progress summary for a `NoBlock` reply — the
/// round dispatcher's third arm, alongside a compile success/failure.
const NO_HASKELL_BLOCK_ROUND_ERROR: &str = "reply had no haskell block";

/// The completed block's rendered value, capped for a round-complete
/// message — GHCi shows you what you evaluated, and so does this window:
/// the wave-per-round idiom (fork a wave, end the round, shape the next
/// wave from what came back) only works if the model can SEE its bound
/// results, not just compute on them blind.
fn rendered_result_snippet(rendered: &str) -> String {
    const CAP: usize = 1500;
    if rendered.chars().count() <= CAP {
        rendered.to_string()
    } else {
        let head: String = rendered.chars().take(CAP).collect();
        format!("{head}\n… (truncated)")
    }
}

/// The corrective line appended when a round ends with green threads still
/// running — the round-scoped structured-concurrency contract, stated at the
/// moment it bit rather than left to be rediscovered.
fn dropped_threads_warning(dropped: usize) -> String {
    format!(
        "Note: {dropped} async thread(s) from that block were still running and were \
         DROPPED — their handles are now dead. `async` and the `wait` that collects \
         it belong in the SAME ```haskell block; results you already bound with \
         `<-` persist and remain usable."
    )
}

struct OuterSession {
    /// The SHARED session's registry id (one-session collapse): the outer
    /// render/loop fragments AND every answerer node's turns run on this one
    /// machine — the driver holds the id, the registry holds the machine,
    /// every access goes through the checkout discipline
    /// ([`crate::harness::Harness::with_session`]).
    sid: tidepool_repr::SessionId,
    cfg: EngineConfig,
    module_name: String,
}

/// The outer Harness-monad's OWN decl list — `Eff '[RunLLMTurn, AskUser]`,
/// distinct from the nested Agent's full stack (`crate::engine`'s private
/// `agent_decls`). `RunLLMTurn` is the loop's model-spawning verb (no BASE
/// effects on the outer row in v1); `AskUser` is
/// added so an AUTHORED `loop` can present a typed operator form directly —
/// `Tidepool.Form`'s `askUser` is auto-imported into every outer compile
/// whenever `AskUser` is in this decl list (see
/// `tidepool_mcp::pragmas_and_imports`), and the driver services the resulting
/// suspension via [`SelfHarnessDriver::service_outer_askuser_hole`]. This does
/// NOT add `AskUser` to `Tidepool.Harness`/`HarnessEff` (whose row stays
/// `'[RunLLMTurn]`, now stale-but-unused): the reference `loop :: State ->
/// Harness State` uses only `runLLMTurn`, and a loop that wants a form imports
/// `Tidepool.Form` and relies on `askUser`'s `Member AskUser` constraint
/// unifying against this wider row.
/// INTERPOSED EFFECTS FIRST — this ordering is load-bearing, not stylistic.
/// `EngineConfig::from_decls` takes the index of the FIRST interposed decl as
/// the suspend threshold, so with `RunLLMTurn` at index 0 the outer session's
/// handled prefix is EMPTY and every effect (including `Subagent`) SUSPENDS
/// to the driver. Putting a handled effect before `RunLLMTurn` would give the
/// SHARED machine a non-empty established prefix, silently dispatching the
/// answerer realms' `AskUser`/`Fork` (tags 0/1) into handler slots — a
/// capability-boundary break. Pinned by `outer_row_suspends_everything`.
///
/// `worktree_decl` is a HARD companion of `subagent_decl`: Subagent's
/// type_defs reference `WorktreeSpec`/`WorktreeId`/`WorktreeHandle`, and its
/// `renderSpawnError` helper calls Worktree's `renderWorktreeError` — helper
/// emission is row-membership-gated, so Worktree must be IN the row, not just
/// in vocab.
///
/// S1-L1 (`plans/self-iterating-harness/20-exomonad-v3-prd.md`) widens this
/// with `Console`/`RepoEvent`/`Exec`: an authored `loop` can now `say`,
/// drive managed worktrees AND observe their repository events, and run
/// shell commands — every one of them suspension-serviced by
/// [`SelfHarnessDriver::service_outer_effect`], exactly like `Subagent`.
///
/// The run-journal lane (PRD 20 S1-L5/S1-L1 wiring) widens it once more with
/// `Journal`: an authored `loop` can now `record` a durable progress step,
/// serviced the same suspension way through
/// [`SelfHarnessDriver::service_outer_effect`]/[`SelfHarnessDriver::set_journal_handler`].
/// `Journal` is deliberately absent from `tidepool-handlers`'
/// `base_effects!`/`handler_for!` row (opt-in, like `Worktree`/`RepoEvent`/
/// `Subagent`) — which journal file a run appends to, and folding it at
/// boot, is a driver/binary wiring concern, not a base-stack default.
///
/// The READ half is wired now (S1-L5 wave 1): [`SelfHarnessDriver::open_run_journal`]
/// loads and folds a run's journal at boot and the first cycle after boot
/// injects it through the harness's `resumeLoop`. `record` is untouched by
/// that and stays WRITE-ONLY on the authored surface — nothing in this row
/// reads a journal.
///
/// `Journal`'s membership here also carries `Tidepool.Resume` onto every outer
/// compile's import list (its `EffectDecl::extra_imports`), which is what puts
/// `Resume.ResumeFold` in scope for the `__selfHarnessResume` splice.
///
/// The green-threads lane (PRD 20 S1-L4,
/// `plans/self-iterating-harness/20-s1l4-green-threads.md`) widens it once
/// more with `Green`, placed LAST so `RunLLMTurn` keeps index 0. Under the
/// registry representation (threads park as new continuations in the
/// session's multi-hole registry, PRD 20 lines 255-267), `Green` is NOT
/// serviced through [`SelfHarnessDriver::service_outer_effect`]'s mechanical
/// decode-dispatch-convert shape the way `Console`/`Worktree`/`RepoEvent`/
/// `Exec`/`Journal` are: an `async` suspension needs the spawned thread
/// body's `ValueHandle` taken off the spawner's parked frame and a NEW
/// suspension-capable top-level run started under its own realm — driver
/// machinery in the `RunLLMTurn`/`AskUser` class ([`engine::classify_hole`]/
/// [`SuspensionRouting`]), not the `OuterEffectKind`/`dispatch_outer_effect` class.
/// [`SelfHarnessDriver::service_green_hole`] is that servicing: a driver-
/// owned thread table + waiter map + FIFO ready queue, scoped to one
/// `run_loop_fragment_inner` call (structured concurrency — nothing survives
/// past the `loop` fragment that spawned it).
fn outer_decls() -> Vec<tidepool_mcp::EffectDecl> {
    OuterRow::new(TurnHeadDecl::run_llm_turn())
        .push(tidepool_mcp::askuser_decl())
        .push(tidepool_mcp::console_decl())
        .push(tidepool_mcp::worktree_decl())
        .push(tidepool_mcp::event_decl())
        .push(tidepool_mcp::exec_decl())
        .push(tidepool_mcp::subagent_decl())
        .push(tidepool_mcp::journal_decl())
        .push(tidepool_mcp::green_decl())
        .into_decls()
}

/// The ONE template every outer-session fragment compile
/// ([`SelfHarnessDriver::compile_outer`] — the `render`/`loop` entries) goes
/// through: UNPAGINATED, for the same reason
/// [`engine::template_turn_for_fused`] states for the fused cycle entry.
/// Every outer entry's JSON is DRIVER-CONSUMED, never displayed — the loop
/// entry's output round-trips back in as the next cycle's `State`, and the
/// render entry feeds the framing/operator page whole.
///
/// The paginated template (`engine::template_turn_for`) wraps the result in
/// `paginateResult 4096`, whose oversized branch on a Console-bearing row
/// (which [`outer_decls`] is) calls `putStrLn` — a suspension. For the
/// POST-loop render that suspension hit `render_framing_with`'s purity
/// refusal the first time a real fold pushed the rendered framing past 4096
/// bytes, failing the cycle AFTER its turn had already completed — and,
/// because the checkpoint commits after that render, discarding the finished
/// turn: a deterministic crash loop redoing (and re-billing) the same turn
/// forever. Pinned by `outer_template_is_unpaginated` in this module's tests.
fn outer_template(stack: &str, code: &str, imports: &str, helpers: &str) -> String {
    engine::template_turn_for_fused(&outer_decls(), stack, code, imports, helpers, &[])
}

/// A decl permitted to occupy [`OuterRow`]'s HEAD slot. The only constructor
/// is [`Self::run_llm_turn`], which calls `tidepool_mcp::runllmturn_decl()`
/// directly (no parameter) — so a `TurnHeadDecl` is never anything other
/// than the real `RunLLMTurn` decl. This is what makes index-0 displacement
/// UNWRITABLE rather than merely pinned by a regression test: there is no
/// value of this type that could wrap a different decl, and [`OuterRow`]
/// only ever renders its head first.
struct TurnHeadDecl(tidepool_mcp::EffectDecl);

impl TurnHeadDecl {
    fn run_llm_turn() -> Self {
        TurnHeadDecl(tidepool_mcp::runllmturn_decl())
    }
}

/// A NonEmpty-shaped builder for the outer row: a `head` slot only
/// [`TurnHeadDecl`] can occupy, plus an ordinary `tail`. `RunLLMTurn` must be
/// first — see [`outer_decls`]'s doc for why (the interposed-effect suspend
/// threshold, `EngineConfig::from_decls`) — and this makes that constructional
/// rather than a fact only a pin test (`outer_row_suspends_everything`)
/// happens to keep true: [`Self::into_decls`] always renders `head` before
/// `tail`, and nothing in this module can construct an `OuterRow` without one.
struct OuterRow {
    head: TurnHeadDecl,
    tail: Vec<tidepool_mcp::EffectDecl>,
}

impl OuterRow {
    fn new(head: TurnHeadDecl) -> Self {
        OuterRow {
            head,
            tail: Vec::new(),
        }
    }

    fn push(mut self, decl: tidepool_mcp::EffectDecl) -> Self {
        self.tail.push(decl);
        self
    }

    fn into_decls(self) -> Vec<tidepool_mcp::EffectDecl> {
        std::iter::once(self.head.0).chain(self.tail).collect()
    }
}

/// The nested answerer Agent's scoped decl row: `[AskUser, Fork, ReadState, Green, Finalize]`.
/// It declares no base effects (`Console`/`KV`/`Fs`/`Http`/`Exec`/`Git`/
/// `Time`/`Meta`) and no `RunLLMTurn`/`Ask`, so an answerer turn compiles
/// against a `Tidepool.Effects` that never defines those verbs — the answerer
/// structurally cannot run a shell command, read files, hit the network, or
/// suspend an in-context `runLLMTurn`. Its whole surface: `askUser` (present a
/// typed form to a human operator, riding `AskUser`), `fork`/`forkAll`
/// (spawn bounded, RECURSIVE sub-answerers, riding `Fork` — the driver
/// services the resulting suspension via [`Self::drive_fork_child_agent_session`],
/// which compiles a fork child against this SAME row, full pump included:
/// a child can `askUser`, `fork` again, and go multi-round, bounded only by
/// the spawn-time budgets in [`Self::check_fork_budgets`] (depth and
/// per-window/per-subtree fan-out), not by row shape), `Tidepool.Async` over
/// `Green` (green threads — the composed idiom `async (fork @T brief)` parks
/// a fork in a thread of its own, so several forks can be outstanding before
/// the first `wait`; serviced by the answerer-plane green scheduler in
/// [`SelfHarnessDriver::drive_agent_session_to_finalize`]), and `finalize` (the
/// answer path).
///
/// `Green` grants NO new external capability: a green thread's body can only
/// perform effects already in this row.
///
/// `AskUser` comes first because [`EngineConfig::from_decls`] takes the first
/// interposed effect as the suspend threshold; `Fork`/`Green`/`Finalize` land
/// at or past it regardless of position.
pub fn typed_request_agent_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![
        tidepool_mcp::askuser_decl(),
        tidepool_mcp::fork_decl(),
        tidepool_mcp::readstate_decl(),
        tidepool_mcp::green_decl(),
        tidepool_mcp::finalize_decl(),
    ]
}

/// [`typed_request_agent_decls`] with `Subagent` and `Worktree` PREPENDED, in that
/// order (PRD 21 C5) — the row a recursive-companion branch-node window
/// compiles against when paired with
/// [`crate::engine::EngineConfig::with_delegate_wrap`]. Both reused
/// verbatim (no new Rust registry row); prepended, not appended, and in
/// THIS order, because `Tidepool.Agent.Delegate.runDelegate`'s own
/// signature (`Eff (Delegate ': effs) a -> Eff (Subagent ': Worktree ':
/// effs) a`, freer-simple `reinterpret2`) re-adds them at the HEAD of
/// whatever row it runs in, in that exact order — for that to line up with
/// `type M`, `Subagent` then `Worktree` must be `type M`'s own first two
/// entries.
///
/// `Worktree` rides in the ROW for real, not merely as vocabulary: `Subagent`'s
/// own auto-import (`extra_imports_for!(Subagent)`,
/// `tidepool-mcp/src/effect_defs.rs`) always pulls in
/// `Tidepool.Agent.Spawn`, which imports `Tidepool.Worktree
/// (renderWorktreeError)` — and `Tidepool.Worktree.hs` is a whole module GHC
/// must typecheck to import anything from it, including its own `M`-typed
/// bindings (`worktreeBranch`, `worktreeHead`), which need `Worktree`
/// genuinely present. `runDelegate`'s `reinterpret2` is what keeps this from
/// widening what the MODEL's own block can reach: freshly re-added effects
/// on a `reinterpret`/`reinterpret2` call's OUTPUT are never members of the
/// row its ARGUMENT (the model's block) is checked against — see
/// `Tidepool.Agent.Delegate`'s module doc.
///
/// Does NOT widen `typed_request_agent_decls()` itself — every other harness (dev-tree,
/// the general Agent stack) keeps compiling exactly as before.
pub fn typed_request_agent_decls_with_delegate() -> Vec<tidepool_mcp::EffectDecl> {
    let mut decls = vec![tidepool_mcp::subagent_decl(), tidepool_mcp::worktree_decl()];
    decls.extend(typed_request_agent_decls());
    decls
}

fn not_bootstrapped() -> DriverError {
    DriverError::Session(
        "outer session not bootstrapped (call run_loop/run_one_loop_iteration)".into(),
    )
}

/// Default emergency-compaction threshold — 80% of the CONTEXT-WINDOW budget
/// ([`EngineConfig::context_window_tokens`], NOT `max_tokens`, which is the
/// 2048 per-turn *output* cap). Overridable per driver via
/// [`SelfHarnessDriver::set_compaction_threshold_percent`] (e.g. a test
/// driving a low threshold to trip compaction deterministically off a
/// single small turn's usage).
const DEFAULT_COMPACTION_THRESHOLD_PERCENT: u64 = 80;

/// The forced compaction turn's target summary length, expressed as a
/// fraction of the context-window budget (a quarter — big enough to carry
/// real context, small enough to be a genuine compaction rather than a
/// re-summarized transcript).
const COMPACTION_TARGET_DIVISOR: u32 = 4;

/// Per-hole SOFT cap: after this many model rounds on a
/// single `runLLMTurn` hole that did NOT finalize, nudge the answerer once
/// ("approaching max tool calls, finalize now with `@T`") and keep driving.
const TYPED_REQUEST_AGENT_NUDGE_ROUNDS: u32 = 16;

/// Per-hole HARD cap: after this many non-finalize model rounds on one hole,
/// hard-fail the `runLLMTurn` effect with a [`DriverError`].
const TYPED_REQUEST_AGENT_MAX_ROUNDS: u32 = 32;

/// Cap on CONSECUTIVE `askUser` re-presentations within the servicing of ONE
/// hole: `askUser` re-prompts by RECURSION on a decode failure — no
/// `Either` — and the frozen headless `StdinGate::present_form` returns an EMPTY
/// JSON object on EOF rather than erroring, so a non-interactive gate with
/// closed stdin composes into an unbounded hot loop that NEITHER
/// `TYPED_REQUEST_AGENT_MAX_ROUNDS` nor `LOOP_INFERENCE_CALL_CAP` catches (both only
/// count `drive_turn` model rounds, and a form resume deliberately does not
/// count as one). This counter is a SEPARATE, independent budget: it
/// increments each time the answerer re-suspends on another `AskUser` hole
/// without making progress, and resets the moment a resume yields anything
/// else (a `Finalize` suspension, a plain completion, a compile error to
/// correct). Past the cap, [`SelfHarnessDriver::drive_agent_session_to_finalize`]
/// hard-fails the hole with a [`DriverError::Session`] naming the cause,
/// rather than spinning at full CPU. 8 leaves ample room for genuine operator
/// typos while making a broken/closed gate terminate loudly and fast.
const ASKUSER_MAX_REPROMPTS: u32 = 8;

/// Per-LOOP hard cap on TOTAL model inference calls across every hole + round.
/// Keeps a misbehaving harness from running away regardless of per-hole
/// budgets or compaction.
const LOOP_INFERENCE_CALL_CAP: u32 = 1024;

/// Default TOTAL fork children one answerer window may spawn across its
/// whole life (all rounds, direct + green-thread forks — one pool). Each
/// child is a real multi-round model window, so this is a genuine resource
/// budget, not a style preference; the refusal on the (N+1)th is loud
/// (block aborted, corrective naming the budget), never a silent drop.
/// How deep model-driven forking may nest (fork-subsumes-split step 2,
/// operator numbers 2026-08-22: 8/32): a session at depth 8 may not fork
/// further. Depth alone is not the real bound — the subtree budget below
/// is — but it caps pathological chains.
const DEFAULT_MAX_FORK_DEPTH: u32 = 8;

/// Total DESCENDANT sessions one top-level agent session's whole fork tree
/// may spawn, counted atomically at every spawn across all depths and both
/// fork styles (direct + green-thread). The real resource bound.
const DEFAULT_FORK_SUBTREE_CAP: u32 = 32;

/// Operator decision 2026-08-22: total-per-node, matching the companion's
/// own `maxFanOut`-shaped budgeting one level up; raised 8 → 32 the same
/// day when multi-WAVE forking (fork, fold, fork again within one window)
/// became the taught idiom — two or three waves of a handful of children
/// each must fit without tuning.
const DEFAULT_FORK_BUDGET_PER_SESSION: u32 = 32;

/// Default cap on how many `RunLLMTurn` fanout/fork children
/// ([`SelfHarnessDriver::service_outer_fanout`]) may be concurrently
/// mid-window (PRD 20 S1-L4 — "concurrent cognition windows") — each in its
/// own freshly-minted answerer realm on the shared outer machine. Only
/// machine occupancy serializes past this point (a window spends most of
/// its wall time in provider inference, off-machine, with nothing checked
/// out); this bounds how many windows may be open — and contending for the
/// machine when their turn comes — at once. Configurable via
/// [`SelfHarnessDriver::set_concurrency_cap`].
const DEFAULT_CONCURRENCY_CAP: usize = 8;

/// How long a fanout/fork child backs off after losing the shared outer
/// machine's checkout race to a sibling child before retrying —
/// [`HarnessError::TurnInFlight`] between siblings means "a sibling
/// currently holds the machine," not a real conflict: the
/// [`crate::registry::SessionRegistry`] refuses a contested checkout
/// immediately rather than queuing it, so the driver supplies the wait.
const CHECKOUT_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(3);

/// Bound on checkout-contention retries (a couple of minutes of backoff at
/// [`CHECKOUT_RETRY_BACKOFF`]) so a genuinely wedged machine fails loud
/// instead of spinning forever.
const CHECKOUT_RETRY_MAX_ATTEMPTS: u32 = 20_000;

/// Retry `attempt` while it keeps losing the shared outer machine's
/// checkout race to a sibling fanout/fork child
/// ([`HarnessError::TurnInFlight`]) — the ONLY error this retries; any
/// other error (a real compile/session fault) propagates immediately. Used
/// for the SYNC `Harness` calls a fanout child's round loop makes that are
/// safe to retry as a whole (`take_finalized_value_keep_open`'s checkout is
/// its first observable effect — nothing has happened yet if it loses the
/// race). `Harness::drive_turn` is deliberately NEVER retried this way — it
/// has already called the provider and appended the assistant reply to the
/// transcript BEFORE its own checkout could contend, so retrying the whole
/// call would re-call the provider and corrupt the transcript; see
/// `Harness::checkout_run_retrying`'s doc for the actual fix (the
/// checkout itself waits, opted into per-node via
/// `Harness::set_retry_checkout_on_contention`).
async fn retry_on_turn_in_flight<T>(
    mut attempt: impl FnMut() -> Result<T, HarnessError>,
) -> Result<T, HarnessError> {
    for _ in 0..CHECKOUT_RETRY_MAX_ATTEMPTS {
        match attempt() {
            Err(HarnessError::TurnInFlight(_)) => {
                tokio::time::sleep(CHECKOUT_RETRY_BACKOFF).await;
            }
            other => return other,
        }
    }
    attempt()
}

/// [`retry_on_turn_in_flight`]'s async-attempt sibling (F4): the SAME
/// policy — retry ONLY [`HarnessError::TurnInFlight`], same backoff/attempt
/// bound — for a `Harness` call that is itself `async fn`
/// (`resume_with_value`/`resume_with_borrowed_root`/`answer_dialog`). Each of
/// those acquires a `TurnLease` before its own checkout, and the lease's
/// `Drop` releases it on every `Err` return (including `TurnInFlight`), so
/// retrying the WHOLE call is exactly as safe as retrying the sync sibling's
/// callers: the checkout is still the first observable effect. These sites
/// are reached by BOTH sequentially- and concurrently-driven callers
/// (`drive_agent_session_to_finalize` is the one pump both a sequential
/// answerer node and a concurrent fork/fanout child run through) — a
/// sequential caller's checkout never actually contends, so this is a no-op
/// there; a concurrent sibling's benign contention now waits instead of
/// hard-failing the whole fanout.
async fn retry_on_turn_in_flight_async<T, Fut>(
    mut attempt: impl FnMut() -> Fut,
) -> Result<T, HarnessError>
where
    Fut: std::future::Future<Output = Result<T, HarnessError>>,
{
    for _ in 0..CHECKOUT_RETRY_MAX_ATTEMPTS {
        match attempt().await {
            Err(HarnessError::TurnInFlight(_)) => {
                tokio::time::sleep(CHECKOUT_RETRY_BACKOFF).await;
            }
            other => return other,
        }
    }
    attempt().await
}

/// The narrow answerer instruction appended after `render`'s output to form
/// the per-loop answerer session's system message. Scoped to the answerer's
/// surface — `askUser`, `fork`/`forkAll`, `finalize` — not the full eval
/// surface [`crate::engine::SYSTEM_FRAMING`] advertises. This is
/// belt-and-braces, not the enforcement mechanism: the scoped stack
/// ([`typed_request_agent_decls`]) is what makes any verb this framing omits fail to
/// compile.
///
/// The per-verb signatures/examples are NOT hand-narrated here: they fold
/// over `decls` — the answerer's ACTUAL configured compiling row
/// ([`Harness::cfg`]'s own `decls`, which is [`typed_request_agent_decls`]
/// widened to [`typed_request_agent_decls_with_delegate`] on the delegating
/// path) — via [`engine::available_effects_section`] — the same
/// [`tidepool_mcp::EffectDecl::prompt_card`]/`description` single source the
/// eval tool description is assembled from — so a row with a different
/// effect set gets a correspondingly different cheatsheet, sent ONCE per
/// loop in the system framing rather than re-narrated every hole. Passing
/// the row explicitly (rather than re-deriving the plain roster in here)
/// is what keeps this framing from omitting a delegating window's own
/// widened row.
fn typed_request_agent_framing_suffix(
    decls: &[tidepool_mcp::EffectDecl],
    fork_budget: u32,
    fork_subtree_cap: u32,
) -> String {
    format!(
        "---\n\
         You are the answering agent for a self-iterating harness loop. The system \
         context above is your working brief (it is re-rendered from the loop's durable \
         State each loop). Each request below asks you for ONE typed value.\n\
         \n\
         Your runnable output is fenced ```haskell blocks: every block in your reply \
         runs, in order, as one sequence — later blocks see earlier blocks' \
         declarations and bindings, so a `data` type declared in one block is usable \
         by `askUser`/`finalize` in the next block of the SAME reply. A value you \
         bind with `x <- …` persists into your NEXT round like GHCi, so you can \
         branch on it.\n\
         \n\
         THIS IS A MULTI-ROUND SESSION, NOT A ONE-SHOT. You have up to {} model \
         rounds (a reminder arrives at round {}), and the EXPECTED shape of a \
         non-trivial request is several of them: orient and define, fork a batch \
         of sub-answerers, read what came back, fork the next batch (or delegate \
         follow-up work) from what you learned, consult the operator where their \
         steer would genuinely change your answer — and only then finalize. A \
         one-round finalize on a question that deserved exploration is an \
         under-served request; keep going while each round is still improving \
         the answer, and finalize the moment one isn't. Rounds \
         accumulate: bindings and `let` helpers from earlier rounds stay in scope. \
         A block that is ONLY top-level declarations (type signatures, function \
         definitions, data types) persists beyond this agent session — for your \
         own later rounds, and for every session forked BENEATH you \
         (ancestry-scoped: descendants inherit your declarations, siblings never \
         do; the protocol above states the rule). The root session's \
         declarations persist for every later loop iteration: your growing \
         library. Define what you will want again.\n\
         \n\
         THE OPERATOR CANNOT INITIATE: they see your notes and the forms you \
         present, and between loop iterations they may attach a message that \
         arrives in your framing. If you want their input NOW, present a form \
         (`askUser`/`choose`); their silence during your session is structural, \
         not meaningful.\n\
         \n\
         {}\n\
         \n\
         Your block is a PROGRAM, not a single question: sequence several \
         consultations in one `do` block and branch on earlier answers with \
         ordinary `case`/`if` — each runs without another model round. Plan \
         the whole consultation up front when the branches are predictable; \
         end the round without finalizing only when an answer genuinely needs \
         fresh judgment. Bind results, then `finalize`.\n\
         \n\
         CONCURRENT DELEGATION: `Tidepool.Async` is the `Control.Concurrent.Async` \
         surface (`async`/`wait`/`waitCatch`/`waitEither`/`waitBoth`/`waitAny`/\
         `race`/`concurrently`/`mapConcurrently`) — your instincts for it apply. \
         It composes with `fork`: `async (fork @T brief)` parks the fork in a \
         green thread, so several forks can be outstanding before the first \
         `wait`:\n\
         \n\
         ```haskell\n\
         import Tidepool.Fork (fork)\n\
         \n\
         do\n\
         \x20\x20ha <- async (fork @Plan \"design the schema\")\n\
         \x20\x20hb <- async (fork @Plan \"design the API\")\n\
         \x20\x20(a, b) <- waitBoth ha hb\n\
         \x20\x20finalize @Plan (mergePlans a b)\n\
         ```\n\
         \n\
         Spawn and wait in the SAME block — threads do not survive their block, \
         though their WAITED results (bound with `<-`) do. Batches compose two \
         ways: within one block, fork a batch, wait for it, fold the results in \
         ordinary Haskell, and fork the next batch from what you computed; or \
         one batch per round, ending the round after the waits so YOUR OWN \
         judgment (not just dataflow) shapes the next batch's briefs from the \
         bound results. This session may spawn at most {} fork children in total \
         (`fork` costs 1, `forkAll` its list length; direct and async forks draw \
         on the same pool), and the WHOLE tree of sessions under one request \
         shares a descendant budget of {} across all depths — so budget your \
         fan where the question genuinely splits, and prefer briefs a child can \
         answer without forking further. One past either budget is refused and \
         the block aborted.\n\
         \n\
         When you have the answer, COMMIT it by evaluating `finalize @T value`. This \
         ends the session and hands the typed value back to the loop. `T` is the type \
         named in the request. Do not call any other effect to answer; `finalize` is \
         how you resolve the request.",
        TYPED_REQUEST_AGENT_MAX_ROUNDS,
        TYPED_REQUEST_AGENT_NUDGE_ROUNDS,
        engine::available_effects_section(decls),
        fork_budget,
        fork_subtree_cap
    )
}

/// A compound answer type, parenthesized for splicing after `@` in prompt
/// text — `finalize @State -> State` is ill-typed ADVICE; `finalize
/// @(State -> State)` is what compiles.
fn display_ty(ty_label: &str) -> String {
    if ty_label.contains(' ') {
        format!("({ty_label})")
    } else {
        ty_label.to_string()
    }
}

fn turn_outcome_tag(o: &TurnOutcome) -> &'static str {
    match o {
        TurnOutcome::Completed { .. } => "Completed",
        TurnOutcome::Suspended { .. } => "Suspended",
        TurnOutcome::NoBlock { .. } => "NoBlock",
    }
}

/// The [`FormShape`] [`SelfHarnessDriver::between_loops_gate`] presents
/// through the operator gate: a single-field record — ONE optional `steer`
/// text field — with the whole question ("Turn N complete — start turn
/// N+1?") carried as the ROOT shape's `doc`, same as
/// `Harness::escalate_to_operator`'s `AllocateMore`/`Abort` form carries its
/// stuck-node reason there. `iteration` is [`SelfHarnessDriver::iteration`]
/// (the count of turns already completed, restored across a restart), so the
/// title is accurate on both a live loop and a freshly restarted one.
fn between_loops_gate_shape(iteration: u64) -> FormShape {
    FormShape::Product {
        type_key: "BetweenTurns".to_string(),
        constructor: "BetweenTurns".to_string(),
        fields: vec![FieldShape {
            key: "steer".to_string(),
            shape: FormShape::Optional(Box::new(FormShape::String)),
            doc: Some(
                "Optional message for the next turn — leave blank to just continue.".to_string(),
            ),
        }],
        doc: Some(format!(
            "Turn {iteration} complete — start turn {next}?",
            next = iteration + 1
        )),
    }
}

/// The outer driver: owns the Harness-monad's own resident session
/// (`Eff '[RunLLMTurn]`, distinct from any Agent node's session) plus a
/// nested [`Harness`] used ONLY to answer `runLLMTurn` holes by driving an
/// Agent turn loop to `finalize`. One driver per running self-harness
/// process (`tidepool-selfharness`).
/// Default fragment ceiling for the shared machine before rotation
/// (`TIDEPOOL_MACHINE_FRAGMENT_CEILING` overrides): each answerer round
/// compiles ~1 fragment, so this is hundreds of loops of headroom while
/// still bounding the never-reclaimed executable memory. Tuned from
/// [`Event::MachineStats`] evidence, per the plan's locked decision.
const DEFAULT_FRAGMENT_CEILING: u64 = 4096;

pub struct SelfHarnessDriver {
    /// The Harness-monad resident session. `None` before bootstrap.
    outer: Option<OuterSession>,
    /// Monotonic per-loop realm counter: each loop's answerer node gets its
    /// own realm on the SHARED machine (structured-concurrency scope; closed
    /// at retirement). Distinct from `iteration` (which restarts restore).
    /// Atomic (not a plain `u64`) because concurrent fanout/fork children
    /// (S1-L4, [`Self::service_outer_fanout`]) each mint their OWN realm
    /// from `&self`, one per window, alongside the single reused
    /// [`Self::answerer`]'s realm.
    iteration_realm: AtomicU64,
    /// The living session values the LAST machine rotation lost — surfaced
    /// once in the next render (legible loss, one-session plan Phase 4),
    /// then cleared.
    last_rotation_losses: Option<Vec<String>>,
    /// The operator's between-loops message — the optional "steering" field
    /// of [`Self::between_loops_gate`]'s form — threaded into the NEXT
    /// cognition window's framing as their utterance, then cleared. Their one
    /// channel for initiating.
    pending_operator_input: Option<String>,
    /// The current cycle's ENTRY state (what `getStateJson` serves): durable
    /// state as of the window's start — this window's edit and operator
    /// ingestion are deliberately not in it (documented semantics of the
    /// ReadState effect). `None` on the very first cycle (initialState —
    /// served as JSON `null`, which the authored `getStateJson` docs cover).
    loop_state_json: Option<Json>,
    /// The nested multi-node orchestrator that answers a `runLLMTurn` hole
    /// by driving an Agent turn loop (`run_to_hole_or_done`) to a
    /// `finalize`. Shared, not owned exclusively, so a future GUI/inspector
    /// can observe the same node tree.
    agent: Arc<Harness>,
    lifecycle: SelfHarnessState,
    observer: Arc<dyn Observer>,
    /// The LATEST emergency-compaction `Text`, fed as the NEXT
    /// [`Self::run_one_loop_iteration`] call's `lastCompaction` — driver-owned state
    /// rather than a threaded parameter, since the *runtime* (not the
    /// caller) owns the compaction lifecycle. Updated MID-LOOP by
    /// [`Self::maybe_compact_answerer`] the moment a
    /// compaction fires (the loop then CONTINUES under the summary). `None`
    /// until the first compaction fires.
    last_compaction: Option<String>,
    /// The compaction `Text` produced DURING the current cycle's loop, if one
    /// fired (in-place mid-loop relief) — distinct from
    /// [`Self::last_compaction`], which also carries a PRIOR cycle's summary
    /// forward. Reset (`take`n) into [`LoopIterationOutcome::compaction`] at the end of
    /// [`Self::run_one_loop_iteration`], so a test asserts on THIS cycle's compaction,
    /// not a stale carried-forward one. Set by [`Self::maybe_compact_answerer`].
    cycle_compaction: Option<String>,
    /// The emergency-compaction threshold, as a percentage of the CONTEXT-
    /// WINDOW budget ([`EngineConfig::context_window_tokens`], default
    /// [`DEFAULT_COMPACTION_THRESHOLD_PERCENT`]). Configurable via
    /// [`Self::set_compaction_threshold_percent`]. Checked MID-LOOP against the
    /// answerer session's real accumulated context ([`Harness::node_usage`]),
    /// not against `max_tokens` after the loop.
    compaction_threshold_percent: u64,
    /// The CURRENT loop's answerer system framing: `render`'s pre-loop output
    /// followed by [`typed_request_agent_framing_suffix`]. Set in
    /// [`Self::run_one_loop_iteration`] right after the pre-loop `render`, read when the
    /// answerer session is created. `None` before the first loop's render.
    answerer_framing: Option<String>,
    /// The CURRENT loop's single render-seeded answerer node: created
    /// ONCE per loop in [`Self::run_loop_fragment`], reused for every
    /// `runLLMTurn` hole so hole #2's answerer sees hole #1's exchange (the
    /// accumulating context window — the fused hylo intermediate). Retired
    /// (dropped) at loop end so the next loop gets a fresh render-seeded
    /// session. `None` between loops. Always [`AgentSessionMode::ReusableLoop`]
    /// — see that type's doc for the distinction it exists to enforce.
    answerer: Option<AgentSessionMode>,
    /// Total model inference calls across the CURRENT loop's holes + rounds:
    /// reset in [`Self::run_loop_fragment`], incremented
    /// per answerer `drive_turn`. The loop hard-stops with a [`DriverError`]
    /// if it reaches [`LOOP_INFERENCE_CALL_CAP`]. Atomic (not a plain `u32`)
    /// because concurrent fanout/fork children (S1-L4,
    /// [`Self::service_outer_fanout`]) each increment it from `&self`
    /// alongside the single reused [`Self::answerer`]'s rounds — one shared
    /// budget regardless of how many windows are open at once.
    loop_inference_calls: AtomicU32,
    /// Per-hole soft cap (nudge threshold), default [`TYPED_REQUEST_AGENT_NUDGE_ROUNDS`].
    /// Configurable via [`Self::set_answerer_round_caps`] so a test can trip
    /// the nudge/hard-fail deterministically with a few small scripted turns
    /// instead of the full 16/32 (each round is a real GHC compile).
    answerer_nudge_rounds: u32,
    /// Per-hole hard cap, default [`TYPED_REQUEST_AGENT_MAX_ROUNDS`]. See
    /// [`Self::set_answerer_round_caps`].
    answerer_max_rounds: u32,
    /// Per-loop total inference-call cap, default
    /// [`LOOP_INFERENCE_CALL_CAP`] (1024). Configurable via
    /// [`Self::set_loop_inference_call_cap`] so a test can prove a specific
    /// model call — e.g. the compaction summarize turn — counts
    /// against it with a small cap instead of scripting 1024 real turns.
    loop_inference_call_cap: u32,
    /// Fork budget: the TOTAL number of fork CHILDREN one answerer window
    /// may spawn across its whole life (all rounds; `fork` costs 1,
    /// `forkAll`/fanout cost their fan) — direct forks and green-thread
    /// forks draw on the ONE pool. Default
    /// [`DEFAULT_FORK_BUDGET_PER_SESSION`]; configurable via
    /// [`Self::set_fork_budget_per_window`]. The (N+1)th child is a loud
    /// refusal (the block is aborted, the session survives with a corrective
    /// naming the budget), never a silent drop.
    fork_budget_per_window: u32,
    /// Step-2 caps ([`DEFAULT_MAX_FORK_DEPTH`]/[`DEFAULT_FORK_SUBTREE_CAP`]),
    /// settable for tests (a depth/subtree refusal is provable with tiny
    /// caps instead of scripting 8 nested GHC sessions).
    max_fork_depth: u32,
    fork_subtree_cap: u32,
    /// The concurrency cap for concurrently-serviced fanout/fork
    /// `RunLLMTurn` windows (PRD 20 S1-L4,
    /// [`Self::service_outer_fanout`]) — default [`DEFAULT_CONCURRENCY_CAP`]
    /// (8). Only machine occupancy serializes turns past this point; this
    /// bounds how many children may be mid-window (a provider call in
    /// flight, or contending for the shared machine) at once. Configurable
    /// via [`Self::set_concurrency_cap`].
    concurrency_cap: usize,
    /// The checkpoint file path: [`Self::restore`] reads it on start, and a
    /// completed cycle commits a fresh [`persistence::Checkpoint`] here (see
    /// [`Self::commit_checkpoint`]) — the one place a checkpoint is ever
    /// written, so a killed-and-restarted process resumes from a state and a
    /// summary that were always committed together. Default
    /// [`persistence::default_checkpoint_path`]; override via
    /// [`Self::set_checkpoint_path`] (mainly for tests, which point it at a
    /// scratch dir rather than the real cache dir).
    checkpoint_path: PathBuf,
    /// The generation of the last checkpoint this driver committed or
    /// restored — `None` before either has happened. A commit passes this as
    /// [`persistence::Checkpoint::committed`]'s `previous`, which derives the
    /// next generation rather than accepting one directly, so generation
    /// increases by exactly one per committed cycle and stays monotonic
    /// across a restart (restore adopts the reloaded generation first).
    checkpoint_generation: Option<persistence::CheckpointGeneration>,
    /// The last [`persistence::Checkpoint`] this driver committed or
    /// restored. `None` before either a commit or a restore has happened —
    /// [`Self::run_loop`] reads exactly that to decide whether this is a
    /// first-ever run (no checkpoint at all, skip straight into the loop) or
    /// a restart with prior history (gate before the next turn — the uniform
    /// restart rule, see [`Self::run_loop`]'s doc).
    last_checkpoint: Option<persistence::Checkpoint>,
    /// The number of loop cycles completed so far — a runtime fact, NOT part
    /// of the authored `State` (`plans/self-iterating-harness/
    /// 15-generic-surface-wave.md`, "Runtime context is the runtime's job").
    /// `0` before any cycle has completed. Incremented once per successful
    /// [`Self::run_one_loop_iteration`], right after that cycle's `loop` completes;
    /// fed into [`Self::render_framing`]'s composed loop-metadata line and
    /// persisted in the checkpoint envelope ([`Self::commit_checkpoint`]) —
    /// never in `state_json` — so a restart resumes counting from the right
    /// number ([`Self::restore`]).
    iteration: u64,
    /// The operator-input seam: the driver blocks on this for `askUser`
    /// form presentation
    /// ([`Self::drive_agent_session_to_finalize`]) and the between-loops human
    /// checkpoint ([`Self::between_loops_gate`]). Sync-blocking by design
    /// (the frozen `OperatorGate` contract, `selfharness/operator.rs`).
    /// Default [`StdinGate`] (headless behavior); override via
    /// [`Self::set_gate`] (a web/GUI implementation, or a scripted test gate).
    gate: Arc<dyn OperatorGate>,
    /// Monotonic id source for [`Event::FormPresented`]/[`Event::FormSubmitted`]
    /// (see [`AskId`]'s doc for why this is one global counter rather than
    /// one per [`FormSource`]). Atomic because [`Self::present_askuser_form`]
    /// mints an id from `&self`. `0` is never minted — the first presentation
    /// gets `AskId(1)`, so `AskId(0)` stays a clean "not recorded" sentinel
    /// for an event logged before this field existed.
    ask_id_counter: AtomicU64,
    /// The driver-owned handler set for every outer-row effect that isn't
    /// `RunLLMTurn`/`AskUser`/`Subagent` (which have their own dedicated
    /// servicing paths) — `Console`/`Worktree`/`RepoEvent`/`Exec`/`Journal`.
    /// Each is DRIVER-owned, never a handler stack on the outer session,
    /// whose handled prefix must stay empty on the shared machine (see
    /// [`outer_decls`]); a suspension against an unwired handler fails
    /// LOUDLY with the wiring instruction, never a hang. Wire one via
    /// [`Self::set_console_handler`]/[`Self::set_worktree_handler`]/
    /// [`Self::set_event_handler`]/[`Self::set_exec_handler`]/
    /// [`Self::set_journal_handler`]. `Mutex`-wrapped for the same
    /// `&self`-dispatch reason [`Self::subagent`] is — see that field's doc
    /// for why `Subagent` is deliberately NOT one of these six.
    handlers: Mutex<OuterHandlers>,
    /// The driver-owned `Subagent` handler — held in its OWN lock, separate
    /// from [`Self::handlers`]. A `SubagentAwait`/coupled `SubagentSpawn`
    /// dispatch can block [`Self::service_outer_subagent`] for a whole agent
    /// cycle (~30–120s live, 64.8s observed): bundling it into
    /// `Mutex<OuterHandlers>` would make that one long block also starve
    /// every unrelated `Console`/`Worktree`/`RepoEvent`/`Exec`/`Journal`
    /// suspension serviced concurrently on another turn, since all six would
    /// wait on the SAME lock for entirely unrelated state. A dedicated lock
    /// bounds the blast radius to Subagent alone. Two siblings delegating in
    /// the same bulk window (`Self::drain_note_holes`'s concurrent
    /// `runLLMTurnBranchFanout` case) still serialize on THIS lock — the same
    /// synchronous handler this driver has always called, never made to run
    /// two dispatches at once — that part is unchanged. Wire it via
    /// [`Self::set_subagent_handler`].
    subagent: Mutex<Option<tidepool_handlers::SubagentHandler>>,
    /// This boot's run-journal fold, waiting to be injected — set by
    /// [`Self::open_run_journal`], `None` when no journal was opened (or when
    /// only the append sink was wired via [`Self::set_journal_handler`]).
    ///
    /// ONE-SHOT: the FIRST cycle after boot `take()`s it
    /// ([`Self::take_loop_entry`]); every later cycle compiles the ordinary
    /// `loop` entry. The fold describes what a CRASHED process had already
    /// done, so re-handing it to a later cycle would be handing it stale news
    /// — and having exactly one moment it can be consumed is what makes the
    /// injection trivially idempotent.
    resume: Option<PendingResume>,
    /// PRD 21 C5 GUI lane: which per-node operator-gate label a
    /// `runLLMTurnBranchLabeled`/`runLLMTurnBranchFanout` child window
    /// carries — populated right when that window's `NodeId` is minted and
    /// removed once it finishes (success, exit, or closure — every path), at
    /// which point [`crate::selfharness::operator::OperatorGate::retire_node`]
    /// is called on the default gate. [`Self::present_askuser_form`]/
    /// [`Self::announce_note`] look a node up here to resolve
    /// [`crate::selfharness::operator::OperatorGate::node_gate`] instead of
    /// the default gate; a node absent here (every unlabeled node, including
    /// the outer loop's own asks and the ordinary per-loop answerer) always
    /// falls back to the default gate — byte-identical to before this field
    /// existed. `Mutex`-wrapped (not a plain map behind `&mut self`):
    /// concurrent fork siblings (`Self::drive_fork_child_agent_session`)
    /// each insert/remove their own entry from `&self`, so a bare `HashMap`
    /// would need `&mut self` at exactly the point several siblings are
    /// running at once.
    node_labels: Mutex<HashMap<NodeId, String>>,
    /// Fork-subsumes-split step 3 (seam map §7.1): the next `idx` to assign
    /// a fork child of a given PARENT, for that child's derived GUI label
    /// (`f<idx>-<slug>` — see [`Self::drive_fork_child_agent_session`]'s doc).
    /// Assigned INSIDE `drive_fork_child_agent_session` rather than threaded in
    /// from a caller: both of its call sites (`drain_answerer_fork` and
    /// `service_thread_ready`'s async fork arm) already pass a fixed
    /// argument list, and the latter is root's concurrent territory this
    /// change must not touch. A per-parent (not global) counter also keeps
    /// labels unique across repeated forks from the same parent over its
    /// lifetime, not just within one `forkAll` batch. `Mutex`-wrapped for
    /// the same concurrent-siblings reason as `node_labels`. Purged at the
    /// same point `node_labels` is, whenever a node that COULD have been a
    /// parent retires (`abort_unguarded_child`,
    /// `drive_fork_child_agent_session`'s exit, `retire_typed_request_agent`)
    /// — a node with no entry here is a harmless no-op remove, so purging
    /// unconditionally at every retirement point is exactly as cheap as
    /// checking first (sol cross-family review finding 11: this map was
    /// previously append-only for the harness's whole life).
    fork_child_seq: Mutex<HashMap<NodeId, u32>>,
}

/// A boot fold and where its segments live, held together so the
/// [`DriverError::ResumeEntryMissing`] refusal can name them and the two can
/// never desync.
struct PendingResume {
    fold: crate::selfharness::resume::ResumeFold,
    log_dir: PathBuf,
    segment_count: usize,
}

/// See [`SelfHarnessDriver::handlers`]'s doc.
#[derive(Default)]
struct OuterHandlers {
    console: Option<tidepool_handlers::ConsoleHandler>,
    worktree: Option<tidepool_handlers::WorktreeHandler>,
    event: Option<tidepool_handlers::RepoEventHandler>,
    exec: Option<tidepool_handlers::ExecHandler>,
    journal: Option<tidepool_handlers::JournalHandler>,
}

/// Which of the two answerer-window modes a node is running under — the
/// path review's own negative evidence (P2.2/P3 typestate opportunity):
/// [`Self::run_loop_fragment`]'s per-loop answerer (kept open across every
/// ordinary `runLLMTurn` hole, its cumulative transcript IS the specified
/// context window) and [`Self::drive_fork_child_agent_session`]'s fork child
/// (attached fresh, retired after exactly one result) share the same
/// finalize-driving code (`drive_agent_session_to_finalize`) but must NOT
/// share their post-finalize behavior — merging them would either discard
/// the loop's accumulating window between ordinary holes or leak a one-shot
/// fork child past its single result. Previously distinguished only by
/// comments and which local variable a raw `NodeId` happened to live in;
/// now a real two-variant sum whose own methods refuse the wrong mode
/// instead of silently reusing/discarding the wrong window.
///
/// The `OneShotBranch` half is superseded at its one call site by
/// [`BranchAgentSessionGuard`], a consuming guard over the same three fields — this
/// sum is what proves the two modes are typed as distinct in the first
/// place; `BranchAgentSessionGuard` is the deeper, ownership-tracked treatment of the
/// one-shot half alone.
#[derive(Debug, Clone, Copy)]
enum AgentSessionMode {
    /// [`Self::answerer`]'s mode: never retired between holes, only ever
    /// read via [`Self::retire_typed_request_agent`] at loop end.
    ReusableLoop {
        node: NodeId,
        realm: tidepool_codegen::jit_machine::RealmId,
    },
    /// A `runLLMTurnBranch` child's mode: answers exactly once, then is
    /// frozen and retired.
    OneShotBranch {
        node: NodeId,
        realm: tidepool_codegen::jit_machine::RealmId,
        scope: tidepool_codegen::scope::ScopeId,
    },
}

impl AgentSessionMode {
    fn node(&self) -> NodeId {
        match self {
            Self::ReusableLoop { node, .. } | Self::OneShotBranch { node, .. } => *node,
        }
    }

    /// Only a `ReusableLoop` lease may take a finalized answer and stay
    /// open for the NEXT hole — the "keep-open" family
    /// ([`Harness::take_finalized_value_keep_open`]/
    /// [`Harness::take_finalized_handle_keep_open`]) is meaningless applied
    /// to a one-shot branch, which never sees a second hole. Bug class
    /// removed: a future refactor reading [`Self::answerer`] and treating
    /// it as reusable when it actually held a one-shot branch's lease now
    /// hard-errors here instead of silently reusing a window that should
    /// have been retired.
    fn require_reusable(&self) -> Result<NodeId, DriverError> {
        match self {
            Self::ReusableLoop { node, .. } => Ok(*node),
            Self::OneShotBranch {
                node, realm, scope, ..
            } => Err(DriverError::Session(format!(
                "window lease: node {node:?} (realm {realm:?}, scope {scope:?}) is a \
                 one-shot branch window, which cannot be kept open as the loop's reusable \
                 answerer"
            ))),
        }
    }

    /// Only a `OneShotBranch` lease may be frozen-then-retired — the
    /// loop's reusable answerer must never be discarded between ordinary
    /// holes (that would silently reset the specified context window the
    /// path review's own negative evidence calls load-bearing). Bug class
    /// removed: retiring/recreating the accumulating loop answerer as if
    /// it were a one-shot branch now hard-errors instead of quietly
    /// dropping the loop's accumulated context mid-cycle.
    fn require_one_shot(
        &self,
    ) -> Result<
        (
            NodeId,
            tidepool_codegen::jit_machine::RealmId,
            tidepool_codegen::scope::ScopeId,
        ),
        DriverError,
    > {
        match self {
            Self::OneShotBranch { node, realm, scope } => Ok((*node, *realm, *scope)),
            Self::ReusableLoop { node, realm } => Err(DriverError::Session(format!(
                "window lease: node {node:?} (realm {realm:?}) is the loop's reusable \
                 answerer, which cannot be frozen-then-retired as if it were a one-shot branch"
            ))),
        }
    }
}

/// A one-shot fork child window's transaction — the deeper, ownership-tracked
/// treatment of [`AgentSessionMode::OneShotBranch`] alone (P3.2's typestate
/// opportunity). Retiring used to be a hand-written four-site discipline per
/// caller (a mechanism error, a non-finalize exit, a closure rejection, and
/// success), linked only by sequencing and a bare `NodeId` a reader had to
/// trust every future error arm would remember to terminate. This guard
/// makes retiring exactly once, on every exit, structural instead:
/// [`Self::finalize_fork_data`] retires on success, [`Self::fold_exit`]
/// retires then produces the exit, and `Drop` retires an unfinished window —
/// the mechanism-failure `?` early return that used to need its OWN
/// hand-written `terminate_node` call now needs none.
///
/// Non-Clone: at most one guard exists per child window.
struct BranchAgentSessionGuard {
    agent: Arc<Harness>,
    node: NodeId,
    realm: tidepool_codegen::jit_machine::RealmId,
    scope: tidepool_codegen::scope::ScopeId,
    retired: bool,
}

// Every field here (`Arc`, `NodeId`, `RealmId`, `ScopeId`, `bool`) is
// independently Clone, so a `#[derive(Clone)]` would compile silently — and
// then a clone's `retired` flag would diverge from the original's, letting
// `finalize_fork_data`/`fold_exit` and the panic-safety `Drop` each believe
// THEY own retiring the window, double-retiring the node this guard exists
// to retire exactly once.
static_assertions::assert_not_impl_any!(BranchAgentSessionGuard: Clone, Copy);

impl BranchAgentSessionGuard {
    /// Mint a guard from an already-established [`AgentSessionMode::OneShotBranch`]
    /// — `require_one_shot` refuses to hand back node/realm/scope if `lease`
    /// were ever (by a future refactor) the loop's reusable answerer instead
    /// of a one-shot child's own, so this is where that check is
    /// load-bearing.
    fn from_lease(lease: AgentSessionMode, agent: Arc<Harness>) -> Result<Self, DriverError> {
        let (node, realm, scope) = lease.require_one_shot()?;
        Ok(Self {
            agent,
            node,
            realm,
            scope,
            retired: false,
        })
    }

    /// Success (fork-subsumes-split step 3, seam map §7.10): a fork answer
    /// is bare data. A successful fork child's durable ending must be
    /// `NodeDone`, recorded BEFORE retirement — `terminate_node` alone would
    /// mark it `NodeCancelled`, an accidental mismatch the seam map calls
    /// out by name. Retries the finalized-value take across a
    /// `TurnInFlight` race (the original one-shot fork path's own
    /// discipline). Consumes the window.
    async fn finalize_fork_data(mut self) -> Result<(Value, String), HarnessError> {
        let node = self.node;
        let (value, rendered) =
            retry_on_turn_in_flight(|| self.agent.take_finalized_value_keep_open(node)).await?;
        let _ = self
            .agent
            .tree()
            .node_done(node, "fork answer delivered".to_string());
        self.agent.terminate_node(node, "fork child retired")?;
        self.retired = true;
        Ok((value, rendered))
    }

    /// A failure ATTRIBUTABLE TO THIS CHILD's window (round exhaustion, a
    /// non-finalize suspension, a closure answer this driver cannot
    /// carry): retire with `reason`, producing nothing further. Consumes
    /// the window (see `drive_fork_child_agent_session`'s own doc for what
    /// happens AFTER this): a mechanism problem (closure, dispatcher-contract
    /// violation) still turns into a hard `Err`, but a fork child's own
    /// `InvocationExit` (round exhaustion, a non-answer ending, its provider
    /// call failing) becomes a plain-language corrective instead — fork
    /// children are not branch positions with a typed `Left` to fold into,
    /// so the corrective is delivered by aborting the block that was
    /// consuming this child, not by folding a `Left`. The retirement itself
    /// (this method) is identical either way.
    fn fold_exit(mut self, reason: &str) {
        let _ = self.agent.terminate_node(self.node, reason);
        self.retired = true;
    }
}

impl Drop for BranchAgentSessionGuard {
    /// Covers exactly the mechanism-failure path: a caller returns `Err(e)`
    /// via `?` before ever reaching [`Self::finalize_fork_data`]/
    /// [`Self::fold_exit`], and this guard simply goes out of scope.
    /// Idempotent with the two consuming methods (`retired` is set the
    /// instant either runs), so this never double-retires an
    /// already-finished window.
    fn drop(&mut self) {
        if !self.retired {
            tracing::warn!(
                node = ?self.node,
                realm = ?self.realm,
                scope = ?self.scope,
                "child window dropped without an explicit exit (mechanism failure)"
            );
            let _ = self.agent.terminate_node(
                self.node,
                "child window dropped without an explicit exit (mechanism failure)",
            );
        }
    }
}

/// A cycle's loop-entry decision, minted once per cycle by
/// [`SelfHarnessDriver::take_loop_entry`] and consumed by whichever
/// compilation path runs this cycle — the fused
/// [`SelfHarnessDriver::compile_loop_entry`] or the unfused
/// [`SelfHarnessDriver::run_loop_fragment_inner`]. Its whole reason to
/// exist is [`SelfHarnessDriver::resume`]'s destructive `self.resume.take()`:
/// before this type, both compile sites called `take_loop_entry` directly,
/// each independently reading `self.resume`, and the fact that only one of
/// them runs per cycle was a RUNTIME CONVENTION a reader had to trust
/// rather than something the types enforced — exactly the shape a future
/// third call site (a preparatory/fallback compile) could violate. Non-Clone:
/// at most one plan is ever live, so a resume fold cannot be injected twice
/// or consumed by the wrong compile.
struct LoopEntryPlan {
    code: String,
    helpers: String,
}

impl LoopEntryPlan {
    /// Consumes the plan. `code` names the loop entry (`Loaded.loop
    /// __selfHarnessState` or `Loaded.resumeLoop …`); `helpers` is the
    /// resume-fold decode splice (empty on an ordinary cycle) a caller
    /// appends alongside `state_cross::state_in`/`operator_msg_in`, which
    /// stay the CALLER's business — they are cycle-wide, not part of the
    /// resume decision this plan makes.
    fn into_code_and_helpers(self) -> (String, String) {
        (self.code, self.helpers)
    }
}

impl SelfHarnessDriver {
    /// Construct a driver over an already-booted [`Harness`] (the nested
    /// orchestrator for `runLLMTurn`-answering Agent sessions) and an event
    /// [`Observer`]. The outer Harness-monad session itself is not
    /// bootstrapped until the first [`Self::run_loop`]/[`Self::run_one_loop_iteration`]
    /// call (it needs the loaded [`HarnessSource`] first).
    pub fn new(agent: Arc<Harness>, observer: Arc<dyn Observer>) -> Self {
        SelfHarnessDriver {
            outer: None,
            iteration_realm: AtomicU64::new(0),
            last_rotation_losses: None,
            pending_operator_input: None,
            loop_state_json: None,
            agent,
            lifecycle: SelfHarnessState::Idle,
            observer,
            last_compaction: None,
            cycle_compaction: None,
            compaction_threshold_percent: DEFAULT_COMPACTION_THRESHOLD_PERCENT,
            answerer_framing: None,
            answerer: None,
            loop_inference_calls: AtomicU32::new(0),
            answerer_nudge_rounds: TYPED_REQUEST_AGENT_NUDGE_ROUNDS,
            answerer_max_rounds: TYPED_REQUEST_AGENT_MAX_ROUNDS,
            loop_inference_call_cap: LOOP_INFERENCE_CALL_CAP,
            fork_budget_per_window: DEFAULT_FORK_BUDGET_PER_SESSION,
            max_fork_depth: DEFAULT_MAX_FORK_DEPTH,
            fork_subtree_cap: DEFAULT_FORK_SUBTREE_CAP,
            concurrency_cap: DEFAULT_CONCURRENCY_CAP,
            checkpoint_path: persistence::default_checkpoint_path(),
            checkpoint_generation: None,
            last_checkpoint: None,
            iteration: 0,
            gate: Arc::new(StdinGate),
            ask_id_counter: AtomicU64::new(0),
            handlers: Mutex::new(OuterHandlers::default()),
            subagent: Mutex::new(None),
            resume: None,
            node_labels: Mutex::new(HashMap::new()),
            fork_child_seq: Mutex::new(HashMap::new()),
        }
    }

    /// The driver's current lifecycle state.
    pub fn lifecycle(&self) -> &SelfHarnessState {
        &self.lifecycle
    }

    /// Refuse to proceed while [`SelfHarnessState::Poisoned`] — the guard every
    /// public entry point (`run_one_loop_iteration`/`run_loop`/`restore`) calls first.
    fn refuse_if_poisoned(&self) -> Result<(), DriverError> {
        match &self.lifecycle {
            SelfHarnessState::Poisoned { reason } => Err(DriverError::Poisoned(reason.clone())),
            _ => Ok(()),
        }
    }

    /// Discard every mutable resident component a cycle may have left behind:
    /// retire the per-loop answerer, clear its framing and this cycle's
    /// compaction, reset the inference-call counter, and retire the outer
    /// session — which may be parked mid-fragment on a hole. [`Self::bootstrap`]
    /// rebuilds it from the harness source the next time it is called, since
    /// it only no-ops while `outer` is `Some`.
    ///
    /// F6: retiring means removing the session from the registry
    /// ([`crate::harness::Harness::retire_adopted_session`]), not just
    /// dropping this struct's own `sid` handle — the session was `adopt_session`'d
    /// into the registry at bootstrap, and the registry is the only place
    /// its machine actually lives (heap, code arena, every still-parked
    /// frame). Dropping only the handle left it there forever: the NEXT
    /// `bootstrap` after a `Failed` cycle adopts a FRESH session under a NEW
    /// `sid`, so the old one was never reachable again — one whole leaked
    /// JIT machine per `Failed`→recovered cycle. (`run_loop` exits on the
    /// first error, so this matters mainly to an embedder/acceptance driver
    /// that keeps calling `run_one_loop_iteration` across a recovered `Failed`.)
    fn discard_resident_state(&mut self) {
        self.retire_typed_request_agent();
        self.answerer_framing = None;
        self.cycle_compaction = None;
        self.loop_inference_calls.store(0, Ordering::SeqCst);
        if let Some(outer) = self.outer.take() {
            self.agent.retire_adopted_session(outer.sid);
        }
    }

    /// The [`AnswerContract`] for a hole of type `ty`: pin `finalize` to it
    /// and import `modules` — the defining modules `asks.json` reported for
    /// `ty` ([`tidepool_runtime::AsksSidecar::modules_of`], resolved by
    /// extract at the call site from the real type environment) — so the
    /// type resolves to the SAME defining module the outer loop resolved,
    /// meaning the finalized value's constructor ids match at the crossing.
    /// This replaced a harness-import-scraping guess
    /// (`HarnessSource::answerer_imports`, since removed): the scrape only
    /// ever found types the harness AUTHOR imported into `loop`'s own
    /// module, so a type the MODEL declares in the session decl plane could
    /// never be named here even though it compiles everywhere else — the
    /// extract-side lookup has no such blind spot, because it runs over
    /// whichever module the type actually came from.
    ///
    /// `None` when the hole's type is unknown (no `asks.json` entry): there is
    /// nothing to pin `finalize` to, so the turn keeps the polymorphic verb.
    fn answer_contract(&self, ty: Option<&str>, modules: &[String]) -> Option<AnswerContract> {
        Some(AnswerContract {
            ty: ty?.to_string(),
            imports: modules.to_vec(),
        })
    }

    /// The author-facing explanation appended to a compile-error retry when the
    /// pinned answer type is what failed to resolve.
    ///
    /// Pinning `finalize` to the hole's type means the turn cannot compile
    /// unless that type is importable by the answerer — so a harness whose
    /// author types live in the same module as `loop` fails here, every round,
    /// until the round cap. That must not read as a mysterious not-in-scope
    /// loop: say what was imported and what the author has to change.
    /// `node`'s CURRENTLY SET [`AnswerContract`] is the source of truth for
    /// what was actually imported (`Harness::answer_contract` — set by the
    /// same caller that pinned this turn), not a second copy of the same
    /// list threaded down separately.
    fn types_in_scope_hint(&self, node: NodeId, ty: &str, error: &str) -> Option<String> {
        if error.contains("Not in scope") && error.contains(ty) {
            let contract = self.agent.answer_contract(node);
            let imported = match contract.as_ref().map(|c| c.imports.as_slice()) {
                None | Some([]) => "no author modules are importable by this stack".to_string(),
                Some(mods) => format!("this turn imports {}", mods.join(", ")),
            };
            return Some(format!(
                "\n\nNOTE: `{ty}` is not in scope and {imported}. The answering stack \
                 cannot import the module that defines `loop` (its `runLLMTurn` is not \
                 in this effect row), so the harness author must move `{ty}` into a \
                 separate module that `loop`'s module imports."
            ));
        }
        // `fork`/`forkAll` unresolved — same "not in scope" GHC shape as the
        // type-name case above, but naming a VALUE (a plain identifier, not
        // `ty`), so it needs its own check rather than folding into the
        // `contains(ty)` branch above.
        if Self::error_names_unimported_fork(error) {
            return Some(
                "\n\nNOTE: `fork`/`forkAll` come from `Tidepool.Fork` — add \
                 `import Tidepool.Fork` to this block's imports."
                    .to_string(),
            );
        }
        None
    }

    /// Whether `error` — a GHC "not in scope" diagnostic — names `fork`/
    /// `forkAll` as the missing identifier. Tokenized on non-alphanumerics so
    /// this matches GHC's exact-name diagnostic (`Variable not in scope:
    /// fork`, whatever quoting marks GHC wraps the name in) regardless of
    /// case, without also firing on `forkSited`/`forkAllSited` (the internal
    /// head-swap targets a model should never be naming directly).
    fn error_names_unimported_fork(error: &str) -> bool {
        let lower = error.to_ascii_lowercase();
        lower.contains("not in scope")
            && lower
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|tok| tok == "fork" || tok == "forkall")
    }

    /// Override the operator-input gate (default [`StdinGate`]). A web/GUI
    /// implementation of [`OperatorGate`] replaces the headless stdin
    /// behavior; a test can inject a scripted gate instead of driving real
    /// stdin.
    pub fn set_gate(&mut self, gate: Arc<dyn OperatorGate>) {
        self.gate = gate;
    }

    /// Wire the subagent seam: the handler a `Subagent` suspension from the
    /// AUTHORED loop dispatches into ([`Self::service_outer_subagent`]).
    /// Construct it with the target repo as its source repository and its
    /// registry/worktree/binding roots OUTSIDE any git work tree; back it
    /// with `MockBackend` in tests and `CodexAgentBackend` live.
    pub fn set_subagent_handler(&mut self, handler: tidepool_handlers::SubagentHandler) {
        *self.subagent.lock() = Some(handler);
    }

    /// Wire the Console seam: the handler a `say`/`Print` suspension from the
    /// AUTHORED loop dispatches into ([`Self::service_outer_effect`]).
    pub fn set_console_handler(&mut self, handler: tidepool_handlers::ConsoleHandler) {
        self.handlers.lock().console = Some(handler);
    }

    /// Wire the Worktree seam: the handler a `createWorktree`/
    /// `lookupWorktree`/`listWorktrees`/`worktreeBranch`/`worktreeHead`
    /// suspension from the AUTHORED loop dispatches into
    /// ([`Self::service_outer_effect`]). Must share its registry/worktree
    /// roots with [`Self::set_event_handler`]'s handler (and, when both are
    /// wired, [`Self::set_subagent_handler`]'s) so a `WorktreeId` minted by
    /// one resolves in the others.
    pub fn set_worktree_handler(&mut self, handler: tidepool_handlers::WorktreeHandler) {
        self.handlers.lock().worktree = Some(handler);
    }

    /// Wire the RepoEvent seam: the handler a `withHandler` subscribe/drain/
    /// unsubscribe suspension from the AUTHORED loop dispatches into
    /// ([`Self::service_outer_effect`]). See [`Self::set_worktree_handler`]'s
    /// doc on shared roots.
    pub fn set_event_handler(&mut self, handler: tidepool_handlers::RepoEventHandler) {
        self.handlers.lock().event = Some(handler);
    }

    /// Wire the Exec seam: the handler a `run`/`runIn`/`runArgv` suspension
    /// from the AUTHORED loop dispatches into ([`Self::service_outer_effect`]).
    pub fn set_exec_handler(&mut self, handler: tidepool_handlers::ExecHandler) {
        self.handlers.lock().exec = Some(handler);
    }

    /// Wire the Journal seam's WRITE half only: the handler a `record`
    /// suspension from the AUTHORED loop dispatches into
    /// ([`Self::service_outer_effect`]) — each call durably appends one step to
    /// the handler's run journal file.
    ///
    /// This wires an EMPTY fold: nothing is loaded, nothing is injected, and
    /// the next cycle compiles exactly the `loop` entry it always did. That is
    /// the right seam for a caller that only ever appends (an acceptance test
    /// exercising `record`, a run deliberately starting clean). A caller that
    /// wants a RESUMED run — folded entries injected through `resumeLoop`, and
    /// appends continuing past the prior process's `seq` — uses
    /// [`Self::open_run_journal`] instead, which wires both halves from one
    /// path so they cannot desync.
    pub fn set_journal_handler(&mut self, handler: tidepool_handlers::JournalHandler) {
        self.handlers.lock().journal = Some(handler);
        self.resume = None;
    }

    /// Open a RUN's journal: load and fold EVERY segment the run id owns, and
    /// build the appending handler over the segment THIS process was
    /// allocated (`acquired.segment`) — all from `acquired`, in one call, so a
    /// resumed run cannot end up folding one set of segments while appending
    /// to a path that disagrees with them.
    ///
    /// That non-desyncability is the whole reason this is one seam rather than
    /// separate calls. A caller that resolved the segment set once and the
    /// append target separately could give the two a different `log_dir`, and
    /// the failure would be silent: a run that folds an old location and
    /// appends to a new one looks like it is working right up until it redoes
    /// finished work.
    ///
    /// Three things happen together here:
    ///
    /// 1. [`crate::selfharness::resume::fold_run_journal`] enumerates every
    ///    segment `acquired.lease.run_id` owns in `log_dir`
    ///    ([`crate::selfharness::resume::list_segments`], numeric segment
    ///    order) and loads each with [`tidepool_handlers::load_journal`] — a
    ///    MISSING file is an empty journal, a torn FINAL line in a segment is
    ///    skipped with a warning, and a torn line anywhere earlier in a
    ///    segment fails loudly. That per-segment contract is unchanged; what
    ///    changes is that no segment but the crashed one can ever carry a
    ///    torn tail, because no other process ever appends into it.
    /// 2. The concatenated entries — segment order, then each segment's own
    ///    append order, the run's TRUE PHYSICAL WRITE ORDER — fold to a
    ///    [`crate::selfharness::resume::ResumeFold`] keyed on `(kind, key)`,
    ///    held until the first cycle consumes it.
    /// 3. The handler is built with
    ///    [`tidepool_handlers::JournalHandler::resuming`], targeting
    ///    `acquired.segment` (this process's OWN, freshly allocated segment —
    ///    never a segment a prior process wrote to) and seeded at
    ///    `acquired.segment_ordinal` — the ordinal THIS process's segment
    ///    claim landed on, never the fold's `next_seq`. Composing that
    ///    ordinal into every `seq` this handler writes
    ///    ([`tidepool_handlers::compose_journal_seq`]) is what makes `seq`
    ///    structurally unique even when two processes resume the SAME
    ///    extant lease at once and therefore fold the identical prior
    ///    state: seeding both from that identical fold's `next_seq` is
    ///    exactly how two concurrent resumes used to collide, and no
    ///    coordination beyond each process's own exclusively-claimed
    ///    segment ordinal is needed to prevent it.
    ///
    /// Returns how many `(kind, key)` pairs folded — `0` for a fresh run, which
    /// is also when the ordinary `loop` entry is compiled unchanged.
    ///
    /// The [`DriverError::ResumeEntryMissing`] refusal for a non-empty fold
    /// against a harness with no `resumeLoop` is raised at BOOTSTRAP (the first
    /// point a [`HarnessSource`] is in hand), not here — this seam never sees
    /// the harness.
    pub fn open_run_journal(
        &mut self,
        log_dir: &Path,
        acquired: &crate::selfharness::resume::AcquiredLease,
    ) -> Result<usize, DriverError> {
        let run_id = &acquired.lease.run_id;
        let fold = crate::selfharness::resume::fold_run_journal(log_dir, run_id)?;
        let folded = fold.len();
        let segment_count = crate::selfharness::resume::list_segments(log_dir, run_id)?.len();
        self.handlers.lock().journal = Some(tidepool_handlers::JournalHandler::resuming(
            acquired.segment.clone(),
            acquired.segment_ordinal,
        ));
        self.resume = Some(PendingResume {
            fold,
            log_dir: log_dir.to_path_buf(),
            segment_count,
        });
        Ok(folded)
    }

    /// Override the emergency-compaction threshold (default
    /// [`DEFAULT_COMPACTION_THRESHOLD_PERCENT`], ~80% of the context-window
    /// budget). Mainly for tests: a low percentage trips
    /// compaction deterministically off a single small scripted turn's usage
    /// instead of needing a long scripted reply sequence to organically cross
    /// 80% of [`EngineConfig::context_window_tokens`].
    pub fn set_compaction_threshold_percent(&mut self, percent: u64) {
        self.compaction_threshold_percent = percent;
    }

    /// Override the per-hole answerer round caps (default
    /// [`TYPED_REQUEST_AGENT_NUDGE_ROUNDS`]/[`TYPED_REQUEST_AGENT_MAX_ROUNDS`], 16/32).
    /// Mainly for tests: small caps (e.g. 3/6) trip
    /// the nudge + hard-fail with a few scripted turns instead of 16/32 real
    /// GHC compiles. `nudge` is clamped below `max`.
    pub fn set_answerer_round_caps(&mut self, nudge: u32, max: u32) {
        self.answerer_nudge_rounds = nudge.min(max);
        self.answerer_max_rounds = max;
    }

    /// Override the per-loop total inference-call cap (default
    /// [`LOOP_INFERENCE_CALL_CAP`], 1024). Mainly for tests: a small cap
    /// checks that a specific model call — e.g. the compaction summarize
    /// turn — is counted against it without scripting 1024 real turns.
    /// Override the per-window fork budget ([`Self::fork_budget_per_window`])
    /// — a test proves the refusal with a budget of 1 or 2 instead of
    /// scripting [`DEFAULT_FORK_BUDGET_PER_SESSION`] real child windows.
    pub fn set_fork_budget_per_window(&mut self, budget: u32) {
        self.fork_budget_per_window = budget;
    }

    /// Test knobs for the step-2 caps (see the constants' docs).
    pub fn set_max_fork_depth(&mut self, depth: u32) {
        self.max_fork_depth = depth;
    }

    pub fn set_fork_subtree_cap(&mut self, cap: u32) {
        self.fork_subtree_cap = cap;
    }

    pub fn set_loop_inference_call_cap(&mut self, cap: u32) {
        self.loop_inference_call_cap = cap;
    }

    /// Override the concurrency cap for concurrently-serviced fanout/fork
    /// `RunLLMTurn` windows (default [`DEFAULT_CONCURRENCY_CAP`], 8;
    /// [`Self::service_outer_fanout`]). Mainly for tests: a cap of exactly
    /// one forces full serialization deterministically, or a cap smaller
    /// than a fan's prompt count proves the excess waits behind it. Clamped
    /// to at least one — a cap of zero would service nothing.
    pub fn set_concurrency_cap(&mut self, cap: usize) {
        self.concurrency_cap = cap.max(1);
    }

    /// Override the checkpoint file path (default
    /// [`persistence::default_checkpoint_path`]). Mainly for tests: point it
    /// at a scratch dir so a test's checkpoint never touches the real cache
    /// dir, and so a "simulated restart" (a second, fresh driver pointed at
    /// the same path) can restore what the first one committed.
    pub fn set_checkpoint_path(&mut self, path: PathBuf) {
        self.checkpoint_path = path;
    }

    /// The current checkpoint file path.
    pub fn checkpoint_path(&self) -> &Path {
        &self.checkpoint_path
    }

    /// The latest compaction summary the driver holds (`self.last_compaction`)
    /// — what the next [`Self::render_framing`] call composes in. Reflects a
    /// reload from [`Self::checkpoint_path`] after [`Self::restore`] runs, or
    /// the most recent mid-loop compaction. `None` before any has fired.
    pub fn last_compaction(&self) -> Option<&str> {
        self.last_compaction.as_deref()
    }

    /// The number of loop cycles this driver has completed — the runtime's
    /// own loop-metadata counter (see [`Self::iteration`]'s field doc), NOT
    /// read from `state_json`. Reflects a reload from
    /// [`Self::checkpoint_path`] after [`Self::restore`] runs, or the count
    /// after the most recent [`Self::run_one_loop_iteration`].
    pub fn iteration(&self) -> u64 {
        self.iteration
    }

    /// Register the outer `PersistentSession` (via
    /// [`crate::harness::Session`]) and splice `source`'s whole module body
    /// ([`HarnessSource`]) as a plain `--include`d module (NOT the session
    /// decl plane — see [`HarnessSource`]'s module doc for why: a static
    /// on-disk harness needs one stable defining module BOTH this compile
    /// and a nested Agent's answerer turn resolve identically, so
    /// author-defined types crossing between them get the same DataConId),
    /// compiled against [`outer_decls`]/[`tidepool_mcp::runllmturn_decl`] —
    /// so `Harness = M` resolves to the literal `Eff '[RunLLMTurn]` row
    /// (02-runtime.md LOCKED). No-op if already bootstrapped. The outer
    /// session's machine comes up lazily, on its first real compile (the
    /// pre-loop `render`) — see [`crate::harness::ResidentSession::unbootstrapped`]
    /// — so this pays no GHC extract compile of its own.
    fn bootstrap(&mut self, source: &HarnessSource) -> Result<(), DriverError> {
        // BEFORE anything else, including the early return: a run whose journal
        // has entries against a harness with no `resumeLoop` is refused here,
        // so the refusal lands before a single cycle runs rather than after a
        // run has already redone finished work.
        if let Some(pending) = &self.resume {
            if !pending.fold.is_empty() && !source.declares_resume_entry {
                return Err(DriverError::ResumeEntryMissing {
                    harness: source.path.display().to_string(),
                    journal: format!(
                        "{} segment(s) under {}",
                        pending.segment_count,
                        pending.log_dir.display()
                    ),
                    run_id: pending.fold.run_id().to_string(),
                    entries: pending.fold.len(),
                });
            }
        }
        if self.outer.is_some() {
            return Ok(());
        }
        let agent_cfg = self.agent.cfg();
        let mut outer_cfg = EngineConfig::from_decls(
            outer_decls(),
            agent_cfg.prelude_dir.clone(),
            agent_cfg.project_lib.clone(),
        )
        .map_err(|e| DriverError::Session(format!("outer engine config: {e}")))?;
        outer_cfg.include.push(source.source_dir.clone());

        let session = Self::build_outer_session(&outer_cfg, Self::open_outer_plane(&outer_cfg));

        // The one-session collapse: the outer session lives in the tree's
        // registry (uniform checkout discipline, panic-safety Drop), the
        // driver holds only its id. Answerer nodes attach to it as realms.
        let sid = self.agent.adopt_session(session);
        self.outer = Some(OuterSession {
            sid,
            cfg: outer_cfg,
            module_name: source.module_name.clone(),
        });
        Ok(())
    }

    /// Construct a fresh outer-session machine handle from `cfg` — shared by
    /// [`Self::bootstrap`] and machine ROTATION ([`Self::machine_maintenance`]):
    /// one construction, so a rotated machine cannot differ from a booted one.
    /// The shared session's decl-plane root — STABLE across rotations
    /// within a process (the plane transfers), wiped at bootstrap (restart
    /// persistence of the plane is future work: the decl log has no disk
    /// reload yet, so a fresh process starts a fresh library — the legible
    /// restart-loss line covers it).
    fn outer_plane_root() -> PathBuf {
        tidepool_runtime::paths::cache_dir().join("selfharness/outer-plane")
    }

    /// Open the shared session's decl plane (one-session LIVING STRUCTURE):
    /// model-authored declarations accumulate here as SOURCE, in scope for
    /// every later answerer turn — across loops, and across machine
    /// rotations (the plane transfers; it is source-side state). Validated
    /// against [`EngineConfig::validation_include`] — the include set MINUS
    /// the per-window SHIM dir, PLUS the stable `Tidepool.Effects.Core` dir
    /// (stable-effects-core). An effectful declaration written
    /// `Member <Eff> effs => ... -> Eff effs T` validates at define time AND
    /// persists across turns/windows — Core's tycons are the same ones every
    /// later turn's compile sees, so a bound call site unifies cleanly. A
    /// declaration that instead spells the per-window `M` alias persists
    /// identically: `M` still never resolves on this plane (the shim isn't on
    /// its include path), but the plane strips the M-mentioning signature
    /// before compiling and lets GHC infer the same `Member`-polymorphic
    /// shape (`tidepool_runtime::session::render`'s `generalize_m_signatures`
    /// — M carries forward cleanly, so this is no longer a taxonomy the model
    /// needs to reason about). Only a declaration pinning a genuinely
    /// CONCRETE row still surfaces the row boundary, and only as an ordinary
    /// unsolved-`Member` error at whatever later use can't satisfy it — never
    /// a define-time refusal. The OUTER render/loop compiles never see this
    /// plane (their include never carries it): the authored harness cannot
    /// silently depend on model-authored names (pillar D) — unaffected by
    /// this change.
    fn open_outer_plane(cfg: &EngineConfig) -> Option<tidepool_runtime::session::SessionLib> {
        let root = Self::outer_plane_root();
        let _ = std::fs::remove_dir_all(&root);
        // The PURE-OR-STABLE-EFFECTFUL decl env, not `standalone_default`: the
        // plane validates under the same ambient pure names a turn has
        // (`Text`, `object`, the Prelude) PLUS the stable Core effect surface,
        // minus the per-window shim modules its include excludes. The minimal
        // env failed `data X = X Text` — companion dogfood 2026-08-14.
        tidepool_runtime::session::SessionLib::open(
            tidepool_repr::SessionId(0),
            &root,
            tidepool_mcp::pure_decl_module_env(),
        )
        .map(|lib| lib.with_validation_include(cfg.validation_include()))
        .ok()
    }

    fn build_outer_session(
        cfg: &EngineConfig,
        lib: Option<tidepool_runtime::session::SessionLib>,
    ) -> crate::harness::Session {
        let handler_cfg = tidepool_handlers::HandlerConfig {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            kv_path: tidepool_runtime::paths::cache_dir().join("selfharness-kv.json"),
            llm_model: std::env::var("TIDEPOOL_LLM_MODEL")
                .unwrap_or_else(|_| "gpt-4o-mini".to_string()),
        };
        // Never actually dispatched to: `cfg.suspend_tag == 0` means every
        // declared effect suspends before reaching a handler.
        let stack: crate::harness::BoxedStack =
            Box::new(tidepool_handlers::build_base_stack(&handler_cfg));
        crate::harness::Session::unbootstrapped(
            stack,
            cfg.suspend_tag,
            cfg.effect_names.clone(),
            tidepool_mcp::CapturedOutput::new(),
            cfg.include.clone(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            lib,
        )
    }

    /// LOOP-BOUNDARY MACHINE MAINTENANCE (one-session plan, Phase 4 —
    /// bounded lifetime, not immortality): emit the machine's
    /// instrumentation ([`Event::MachineStats`] — the rotation-cadence
    /// evidence base), and at the fragment CEILING rotate: a fresh machine
    /// adopted under the SAME session id at a quiescent boundary. Durable
    /// state flows through the checkpoint exactly as every loop always has;
    /// decl-plane source (when present) is machine-independent; living
    /// session VALUES are lost — ENUMERATED into [`Event::MachineRotated`]
    /// and the next render's legible-loss note, never silently. A
    /// non-quiescent machine at the ceiling refuses the loop with a legible
    /// error instead of growing silently (the enforced bound the parking
    /// contract's §2(c) amendment names).
    fn machine_maintenance(&mut self) -> Result<(), DriverError> {
        let sid = self.outer_sid()?;
        let (stats, hole_count, bindings) = self
            .agent
            .with_session(sid, |s| {
                (s.heap_stats(), s.parked_holes().len(), s.binding_names())
            })
            .map_err(|e| DriverError::Session(e.to_string()))?;
        let Some(stats) = stats else {
            // Machine not booted yet (first cycle) — nothing to measure.
            return Ok(());
        };
        self.emit(Event::MachineStats {
            fragments: stats.fragments,
            live_bytes: stats.live_bytes as u64,
            gc_count: stats.gc_count,
        });
        let ceiling = std::env::var("TIDEPOOL_MACHINE_FRAGMENT_CEILING")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_FRAGMENT_CEILING);
        if stats.fragments < ceiling {
            return Ok(());
        }
        if hole_count > 0 {
            return Err(DriverError::Session(format!(
                "machine at fragment ceiling ({} >= {ceiling}) but not quiescent \
                 ({hole_count} parked hole(s)) — cannot rotate mid-suspension; raise \
                 TIDEPOOL_MACHINE_FRAGMENT_CEILING or bounce the harness",
                stats.fragments
            )));
        }
        // The decl plane is SOURCE-side state and SURVIVES rotation: take it
        // off the old machine and install it into the fresh one (living
        // structure defined by name persists; only heap VALUES die, and
        // those are the enumerated losses below).
        let lib = self
            .agent
            .with_session(sid, |s| s.take_lib())
            .map_err(|e| DriverError::Session(e.to_string()))?;
        let cfg = &self.outer.as_ref().ok_or_else(not_bootstrapped)?.cfg;
        let fresh = Self::build_outer_session(cfg, lib);
        self.agent
            .replace_session(sid, fresh)
            .map_err(|e| DriverError::Session(e.to_string()))?;
        self.emit(Event::MachineRotated {
            fragments: stats.fragments,
            bindings_lost: bindings.clone(),
        });
        self.last_rotation_losses = Some(bindings);
        Ok(())
    }

    /// The shared session's registry id, or the not-bootstrapped error.
    fn outer_sid(&self) -> Result<tidepool_repr::SessionId, DriverError> {
        self.outer
            .as_ref()
            .map(|o| o.sid)
            .ok_or_else(not_bootstrapped)
    }

    /// Compile `code` (with `helpers`) against the outer session, importing
    /// the harness module QUALIFIED as [`state_cross::LOADED_QUALIFIER`]
    /// rather than unqualified — every later fragment turn's shared compile
    /// step (`Loaded.loop ...`, `Loaded.render ...`, `Loaded.initialState`).
    /// Qualifying dodges a turn's default preamble always bringing
    /// `Tidepool.Prelude` into scope unqualified too, which exports names
    /// an authored harness routinely also defines (e.g. `render`) — see
    /// `state_cross`'s module doc for the "ambiguous occurrence" this
    /// avoids.
    ///
    /// `label` (`"render"`/`"loop"`) identifies this compile in the emitted
    /// [`Event::OuterCompile`] — the OUTER session has no per-node durable
    /// log of its own (`crate::log::Event::TurnStart` only ever covers a tree
    /// node's turns), so this event is the whole record of what the outer
    /// session's fragments actually were, verbatim.
    fn compile_outer(
        &mut self,
        code: &str,
        helpers: &str,
        label: &str,
    ) -> Result<CompiledTurn, DriverError> {
        let outer = self.outer.as_ref().ok_or_else(not_bootstrapped)?;
        let imports = format!(
            "qualified {} as {}",
            outer.module_name,
            state_cross::LOADED_QUALIFIER
        );
        let stack = outer
            .cfg
            .turn_target(None)
            .map_err(|e| DriverError::Session(format!("outer engine target: {e}")))?
            .stack;
        let extract_bin = outer.cfg.extract_bin.clone();
        let include = outer.cfg.include.clone();
        let src = outer_template(&stack, code, &imports, helpers);
        self.emit(Event::OuterCompile {
            label: label.to_string(),
            source: src.clone(),
        });
        engine::compile_turn(
            &extract_bin,
            &src,
            "result",
            &include,
            timing::NO_NODE,
            timing::NO_ROUND,
        )
        .map_err(|e| DriverError::Session(format!("outer compile failed: {e}")))
    }

    /// The `--targets` name of the fused module's extra entry — the loop
    /// body, compiled alongside `result` (the render entry) by
    /// [`Self::compile_loop_entry`] in ONE `tidepool-extract` spawn. Named in
    /// the `__selfHarness*` family like every other runtime-generated splice
    /// in this driver.
    const LOOP_ENTRY_TARGET: &'static str = "__selfHarnessLoopEntry";

    /// Which loop entry THIS cycle compiles, and the extra helper text it
    /// needs: `(code, extra_helpers)`.
    ///
    /// - A fresh boot, or any cycle after the first, or an EMPTY fold →
    ///   `Loaded.loop __selfHarnessState` with no extra helpers: byte for byte
    ///   the entry every harness has always compiled, which is what keeps the
    ///   twelve `loop`-only harnesses and their tests untouched.
    /// - A non-empty boot fold → `Loaded.resumeLoop __selfHarnessResume
    ///   __selfHarnessState`, with [`state_cross::resume_in`]'s decode splice
    ///   in the helpers.
    ///
    /// `take()`s the fold: the injection is ONE-SHOT at boot (see
    /// [`Self::resume`]'s doc), minted into a [`LoopEntryPlan`] a caller
    /// then consumes exactly once. [`Self::run_one_loop_iteration`] mints ONE plan
    /// per cycle and passes it down to [`Self::compile_loop_entry`] (the
    /// fused, production path); the unfused [`Self::run_loop_fragment_inner`]
    /// — a direct fragment API `run_one_loop_iteration` never itself calls, used by a
    /// test driving the fragment in isolation — mints its OWN plan instead.
    /// Either way `self.resume` is readable ONLY through this method, so a
    /// second call in the same cycle cannot get the resume fold a second
    /// time: it already saw `self.resume.take()` return `None` from the
    /// first call and mints the fold-less plan instead — a physically
    /// enforced one-shot rather than "only one caller happens to run per
    /// cycle" left to convention.
    ///
    /// A non-empty fold against a harness with no `resumeLoop` never reaches
    /// here: `bootstrap` refused it.
    fn take_loop_entry(&mut self) -> LoopEntryPlan {
        let q = state_cross::LOADED_QUALIFIER;
        match self.resume.take() {
            Some(pending) if !pending.fold.is_empty() => LoopEntryPlan {
                code: format!("{q}.resumeLoop __selfHarnessResume __selfHarnessState"),
                helpers: state_cross::resume_in(&pending.fold),
            },
            _ => LoopEntryPlan {
                code: format!("{q}.loop __selfHarnessState"),
                helpers: String::new(),
            },
        }
    }

    /// Compile the PRE-loop `render` and this cycle's `loop` fragment as TWO
    /// entries of ONE module, in a SINGLE `tidepool-extract` spawn
    /// ([`compile_turns`]) — the pre-model boot-path fusion this
    /// driver exists to land (`plans/post-restart/extract-wave/spawn-latency/
    /// 04-turn-latency-plan.md` §2). Both entries splice
    /// `state_cross::state_in(prior_state)` with the SAME `prior_state`, so
    /// their helper text is byte-identical by construction — one splice, not
    /// two — and [`tidepool_mcp::TurnTemplate::extra_entries`] renders the
    /// loop entry through the exact code path `result` (the render entry)
    /// uses, so the two are identical by construction rather than a
    /// hand-copied second shape.
    ///
    /// The merged table this spawn returns is a FEATURE, not an artifact: both
    /// targets share ONE `meta.cbor` (`compile_turns`'s whole point),
    /// so the render entry's [`CompiledTurn::table`] already carries the loop
    /// entry's constructors — including the `RunLLMTurn` ConTags the machine
    /// needs once `loop` starts suspending on holes.
    ///
    /// Returns `(render_turn, loop_turn)`. Does NOT run either — that stays
    /// [`Self::render_framing`]/[`Self::run_loop_fragment_inner`]'s job, so a
    /// caller can compile once and run each entry through its own existing
    /// path.
    ///
    /// `plan` is this cycle's [`LoopEntryPlan`] — minted ONCE by the caller
    /// ([`Self::run_one_loop_iteration`]) via [`Self::take_loop_entry`] and consumed
    /// HERE, never minted by this method itself: see `LoopEntryPlan`'s doc
    /// for why that split is the point.
    fn compile_loop_entry(
        &mut self,
        prior_state: Option<&Json>,
        plan: LoopEntryPlan,
    ) -> Result<(CompiledTurn, CompiledTurn), DriverError> {
        // Register this cycle's (stateJson, operatorMsgJson) under the
        // stable Val.G0 binding BEFORE compiling — the outer module's
        // `--inject-val` reference below resolves at RUN time against
        // whatever this call last registered on the OUTER session.
        self.refresh_harness_ctx(prior_state)?;

        let outer = self.outer.as_ref().ok_or_else(not_bootstrapped)?;
        let imports = format!(
            "qualified {} as {}\n{}",
            outer.module_name,
            state_cross::LOADED_QUALIFIER,
            state_cross::harness_ctx_module().module_name(),
        );
        let stack = outer
            .cfg
            .turn_target(None)
            .map_err(|e| DriverError::Session(format!("outer engine target: {e}")))?
            .stack;
        let extract_bin = outer.cfg.extract_bin.clone();
        let include = outer.cfg.include.clone();

        let (loop_code, resume_helpers) = plan.into_code_and_helpers();
        // FIXED text — turn-invariant by construction, see
        // `state_cross::state_in_via_ctx`'s doc. `resume_helpers` stays a
        // literal splice (unchanged, out of this pass's scope): it is
        // non-empty on at most the first cycle after a boot fold, never
        // re-paid every turn the way state/operator-msg were.
        let helpers = format!(
            "{}{}{}",
            state_cross::state_in_via_ctx(),
            state_cross::operator_msg_in_via_ctx(),
            resume_helpers,
        );
        let render_code = format!(
            "pure ({q}.render __selfHarnessState)",
            q = state_cross::LOADED_QUALIFIER
        );
        let src = engine::template_turn_for_fused(
            &outer_decls(),
            &stack,
            &render_code,
            &imports,
            &helpers,
            &[(Self::LOOP_ENTRY_TARGET, &loop_code)],
        );
        self.emit(Event::OuterCompile {
            label: "render+loop".to_string(),
            source: src.clone(),
        });
        let mut turns = engine::compile_turns_with_stable_inject(
            &extract_bin,
            &src,
            &["result", Self::LOOP_ENTRY_TARGET],
            &include,
            tidepool_runtime::StableValInject {
                module: state_cross::harness_ctx_module(),
                session_root: &Self::harness_ctx_session_root(),
            },
            timing::NO_NODE,
            timing::NO_ROUND,
        )
        .map_err(|e| DriverError::Session(format!("fused outer compile failed: {e:?}")))?;
        let render_turn = turns.remove("result").ok_or_else(|| {
            DriverError::Session("fused outer compile: missing render entry".into())
        })?;
        let loop_turn = turns.remove(Self::LOOP_ENTRY_TARGET).ok_or_else(|| {
            DriverError::Session("fused outer compile: missing loop entry".into())
        })?;
        Ok((render_turn, loop_turn))
    }

    /// The `--session-root` the harness-ctx bind writes/reads its `.hi`
    /// iface under — reuses [`Self::outer_plane_root`] rather than a
    /// separate directory: the OUTER session's `PersistentSession` is ONE
    /// `BindingTable`/value plane (the one-session collapse), so once
    /// `Val.G0` is registered there, ANY later compile on the same session
    /// that consults `live_val_modules`/`current_val_modules` — in
    /// particular an answerer turn's compile, which injects every live
    /// session value — reports it and expects to find its iface under
    /// THIS SAME root, whichever caller set it up. Two sessions couldn't
    /// each keep their own; there is exactly one root per session, already
    /// named. Safe to write into: `open_outer_plane` wipes this dir only at
    /// `bootstrap` (once per session — a machine ROTATION transfers the
    /// existing `SessionLib` via [`Self::build_outer_session`] rather than
    /// re-wiping), and the harness-ctx iface's CONTENT is a pure function of
    /// [`state_cross::harness_ctx_module`] (a fixed name/type), so
    /// overwriting it in place every cycle alongside the decl plane's own
    /// `Lib.G<g>.hs` files is safe and keeps the memo's content fingerprint
    /// stable turn to turn.
    fn harness_ctx_session_root() -> PathBuf {
        Self::outer_plane_root()
    }

    /// (Re-)bind [`state_cross::HARNESS_CTX_BINDING`] at
    /// [`state_cross::harness_ctx_module`] on the OUTER session to this
    /// cycle's `(stateJson, operatorMsgJson)` — the value half of
    /// `plans/turn-latency-state-injection.md`'s injection (the type/iface
    /// half is [`Self::compile_loop_entry`]'s `--inject-val`).
    ///
    /// Compiles a tiny standalone module
    /// ([`state_cross::harness_ctx_source`]) through
    /// [`tidepool_runtime::session::turn::compile_session_turn`]'s
    /// `--session-bind` path — its own small, deliberately non-cacheable
    /// spawn (fresh literal content every cycle; see that function's doc) —
    /// then runs it, tenures the result, and registers it against the OUTER
    /// session via [`tidepool_runtime::session::resident::ResidentSession::run_bind`]:
    /// the SAME `Tidepool.Session.Val.G<g>` value-plane mechanism the
    /// interactive session already uses for its rotating binds, just at the
    /// one reserved, non-rotating generation
    /// ([`state_cross::harness_ctx_module`]'s doc explains why gen 0 can
    /// never collide with a real one). `run_bind` both materializes AND
    /// registers the binding in one call, so nothing further is needed here
    /// for a later `--inject-val` reference (or this SAME session's own
    /// `render`/`loop` run, which resolves it automatically via
    /// `ResidentSession::run`'s existing `seed_external_env_for`) to see it.
    fn refresh_harness_ctx(&mut self, prior_state: Option<&Json>) -> Result<(), DriverError> {
        let state_json = prior_state.map_or_else(|| "null".to_string(), Json::to_string);
        let operator_json = serde_json::to_string(&self.pending_operator_input)
            .unwrap_or_else(|_| "null".to_string());
        let src = state_cross::harness_ctx_source(&state_json, &operator_json);

        let session_root = Self::harness_ctx_session_root();
        std::fs::create_dir_all(&session_root)
            .map_err(|e| DriverError::Session(format!("harness-ctx session root: {e}")))?;

        let binding_name = state_cross::HARNESS_CTX_BINDING.to_string();
        let turn = tidepool_runtime::session::turn::compile_session_turn(
            &src,
            &[],
            &session_root,
            &[],
            Some(tidepool_runtime::session::turn::SessionBind {
                names: std::slice::from_ref(&binding_name),
                gen: 0,
                probe_only: false,
            }),
        )
        .map_err(|e| DriverError::Session(format!("harness-ctx bind compile failed: {e:?}")))?;
        let binder = turn.binders.first().ok_or_else(|| {
            DriverError::Session("harness-ctx bind: extract returned no binders".into())
        })?;

        let sid = self.outer_sid()?;
        let outcome = self
            .agent
            .with_session(sid, |s| {
                s.run_bind(
                    "harness_ctx",
                    &turn.expr,
                    &turn.table,
                    binder,
                    tidepool_repr::Generation(0),
                )
            })
            .map_err(|e| DriverError::Session(e.to_string()))?
            .map_err(|e| DriverError::Session(format!("harness-ctx bind run failed: {e}")))?;
        match outcome {
            ResidentOutcome::Completed { .. } => Ok(()),
            ResidentOutcome::Suspended { .. } => Err(DriverError::Session(
                "harness-ctx bind suspended unexpectedly — must be a pure value".into(),
            )),
        }
    }

    /// Run ONE `render` → `loop` → (service each `runLLMTurn` hole) →
    /// `render` cycle: bootstrap the outer session if needed, render the
    /// pre-loop prompt ([`Self::render_framing`] — the author's `render`
    /// output composed with the prior compaction summary and the
    /// loop-iteration count), run `loop state` as a suspendable fragment
    /// (servicing every `runLLMTurn` hole via
    /// [`Self::service_typed_request_suspension`]), serialize the returned `State`
    /// ([`state_cross::state_out`]), advance `self.iteration`, and render
    /// the post-loop prompt. `prior_state` is `None` only for the very
    /// first cycle. The compaction summary fed to [`Self::render_framing`]
    /// is NOT a parameter — it is `self.last_compaction`, the latest
    /// emergency-compaction `Text` if one has fired, carried forward
    /// automatically across repeated calls (by [`Self::run_loop`], or by a
    /// caller driving cycles by hand — see `acceptance_selfharness.rs`),
    /// since the *runtime*, not the caller, owns the compaction lifecycle.
    /// This cycle's OWN compaction (if
    /// [`Self::maybe_compact_answerer`] fires one MID-LOOP) updates
    /// `self.last_compaction` before `prompt_after` is rendered, so
    /// `prompt_after` already reflects it — proving the summary reaches the
    /// very next render.
    pub async fn run_one_loop_iteration(
        &mut self,
        source: &HarnessSource,
        prior_state: Option<&Json>,
    ) -> Result<LoopIterationOutcome, DriverError> {
        self.refuse_if_poisoned()?;

        // A prior cycle's error guard (below) already discarded `self.outer`,
        // so this bootstrap call is where recovery from a `Failed` state
        // rebuilds it. If recovery itself cannot bootstrap, the driver has no
        // path back to a usable outer session — escalate past `Failed`
        // (recoverable) to `Poisoned` (not) rather than sit in a stale
        // `Failed` that will never clear. A bootstrap failure that is NOT a
        // recovery attempt (the very first cycle a driver ever runs) is an
        // ordinary cycle error: discard whatever partial state accumulated
        // and publish `Failed`, same as any other cycle error, so the next
        // call retries bootstrap rather than leaving `lifecycle()` reporting
        // the cosmetic `Idle` a driver starts in.
        let recovering_from_failure = matches!(self.lifecycle, SelfHarnessState::Failed { .. });
        if let Err(e) = self.bootstrap(source) {
            self.discard_resident_state();
            self.lifecycle = if recovering_from_failure {
                SelfHarnessState::Poisoned {
                    reason: e.to_string(),
                }
            } else {
                SelfHarnessState::Failed {
                    reason: e.to_string(),
                }
            };
            return Err(e);
        }
        // Loop-boundary machine maintenance: instrumentation + the enforced
        // fragment ceiling (rotation at quiescence) — one-session plan, Phase 4.
        self.machine_maintenance()?;
        self.emit(Event::LoopBoundary);

        // Mint THIS cycle's loop-entry plan ONCE, here — the one call to
        // `take_loop_entry` a production cycle ever makes — and pass it
        // down to `compile_loop_entry` rather than letting that method
        // mint its own (`LoopEntryPlan`'s doc).
        let plan = self.take_loop_entry();

        // Compile the pre-loop `render` and this cycle's `loop` fragment
        // TOGETHER, in ONE spawn (`Self::compile_loop_entry`), then run the
        // render entry directly against `prior_state` — `None` (the very
        // first cycle) splices `Loaded.initialState` in the shared helpers
        // (`state_cross::state_in(None)`), so no redundant `pure initialState`
        // compile + round-trip through JSON is needed.
        //
        // THIS compile — not `bootstrap` — is where "can we build a usable
        // outer session at all" is actually answered, so its failure (compile
        // OR the render entry's run) takes the same Failed-vs-Poisoned
        // classification as a bootstrap failure. Being one fused spawn, a
        // loop-entry compile failure surfaces here too. A bare `?` here would
        // return early PAST the lifecycle update below, leaving a failed
        // driver reporting the cosmetic `Idle` (or a failed recovery
        // reporting `Failed` forever instead of escalating) — do not
        // simplify this to one.
        let prior_compaction = self.last_compaction.clone();
        let (prompt_before, loop_turn) = match self.compile_loop_entry(prior_state, plan) {
            Ok((render_turn, loop_turn)) => {
                match self.render_framing_with(&render_turn, prior_compaction.as_deref()) {
                    Ok(prompt) => (prompt, loop_turn),
                    Err(e) => {
                        self.discard_resident_state();
                        self.lifecycle = if recovering_from_failure {
                            SelfHarnessState::Poisoned {
                                reason: e.to_string(),
                            }
                        } else {
                            SelfHarnessState::Failed {
                                reason: e.to_string(),
                            }
                        };
                        return Err(e);
                    }
                }
            }
            Err(e) => {
                self.discard_resident_state();
                self.lifecycle = if recovering_from_failure {
                    SelfHarnessState::Poisoned {
                        reason: e.to_string(),
                    }
                } else {
                    SelfHarnessState::Failed {
                        reason: e.to_string(),
                    }
                };
                return Err(e);
            }
        };

        // The pre-loop render IS the answerer session's system message.
        // Compose it with the narrow answerer instruction and stash it for
        // `run_loop_fragment` to seed the per-loop answerer node.
        self.answerer_framing = Some(format!(
            "{prompt_before}\n\n{}",
            typed_request_agent_framing_suffix(
                &self.agent.cfg().decls,
                self.fork_budget_per_window,
                self.fork_subtree_cap
            )
        ));

        self.lifecycle = SelfHarnessState::RunningLoop;
        // The driver must not strand the lifecycle in `RunningLoop`/`Compacting`
        // on any exit from the loop body: a runaway-cap hard-fail, a failed
        // resume, or a compaction error all leave a mutable resident session
        // (the outer session, the per-loop answerer) that outlives this call.
        // Run the fallible body, then publish `Idle` on success or `Failed`
        // (after discarding that resident state) on error — never `Idle` on
        // a path that didn't actually finish.
        let result: Result<LoopIterationOutcome, DriverError> = async {
            let (value, table) = self.run_loop_fragment(prior_state, Some(loop_turn)).await?;
            let state_json = state_cross::state_out(&value, &table);

            // This cycle's `loop` completed — advance the runtime's OWN
            // iteration counter (never part of authored `State`) before the
            // post-loop render, so `prompt_after` (and the next cycle's
            // `prompt_before`) report the count of loops completed so far.
            self.iteration += 1;

            // Any MID-LOOP compaction that fired during this loop has already
            // set `self.cycle_compaction` (and `self.last_compaction`) IN PLACE —
            // the loop CONTINUED under the summary rather than aborting. `None` if
            // the context window never crossed threshold this loop.
            let compaction = self.cycle_compaction.take();
            // `self.last_compaction` carries the LATEST compaction summary forward
            // to the next render regardless of which cycle produced it: this
            // cycle's if one fired, else the prior cycle's (unchanged). Render
            // `prompt_after` against it so the summary reaches the very next render
            // (02-runtime.md; the compaction summary is composed by
            // `render_framing`, not threaded through the author's `render`).
            let next_compaction = self.last_compaction.clone();
            let prompt_after =
                self.render_framing(Some(&state_json), next_compaction.as_deref())?;

            // One writer, one boundary: a cycle that reaches this point
            // completed successfully, so its state and the compaction summary
            // in force right now commit together as the next generation.
            self.commit_checkpoint(source, &state_json)?;

            Ok(LoopIterationOutcome {
                prompt_before,
                state_json,
                prompt_after,
                compaction,
            })
        }
        .await;
        match &result {
            Ok(_) => self.lifecycle = SelfHarnessState::Idle,
            Err(err) => {
                self.discard_resident_state();
                self.lifecycle = SelfHarnessState::Failed {
                    reason: err.to_string(),
                };
            }
        }
        result
    }

    /// Bootstrap the outer session over `source`, restore the last
    /// committed checkpoint from [`Self::checkpoint_path`] if one is there
    /// yet (restart-reload — falls back to `initialState`, exactly the
    /// in-process very-first-cycle case, when nothing has been committed
    /// yet), then run [`Self::run_one_loop_iteration`] FOREVER, threading each
    /// cycle's returned `State` into the next one (each cycle commits its
    /// own checkpoint on success — see [`Self::commit_checkpoint`] — so
    /// this loop does no persistence of its own). Production entry point —
    /// see the module doc for why this (and everything it calls) must run
    /// on a thread with an active multi-thread tokio runtime.
    ///
    /// Between-loops human gate: before each new cycle, unless `auto` is set
    /// or this is the very first cycle of a fresh run (no checkpoint restored
    /// at all — straight into the loop, which asks the authored seed question
    /// via `askUser`), block on [`Self::between_loops_gate`] — an ordinary
    /// operator form ("Turn N complete — start turn N+1?" plus an optional
    /// steering field), presented through the same `present_form` machinery
    /// every `askUser` ask uses. `auto` (the binary's `--yes`/`--auto` flag)
    /// skips the gate for CI/replay. The acceptance path drives
    /// [`Self::run_one_loop_iteration`] directly and has NO gate.
    ///
    /// **Restart rule (uniform, no marker):** ANY boot that restores a
    /// checkpoint presents the between-turns gate before running the next
    /// turn — regardless of whether the prior process crashed mid-turn (the
    /// turn simply reruns per the existing at-least-once semantics, and the
    /// operator is asked again before it starts) or while genuinely parked on
    /// the gate itself (the operator is asked again, no different from any
    /// other restart). This replaces an earlier design that persisted a
    /// dedicated `awaiting_continue` checkpoint marker to distinguish the two
    /// cases — the marker is gone; every restart with prior history simply
    /// re-asks.
    ///
    /// Defense in depth against a state-decode failure taking the whole
    /// process down: whatever [`Self::restore`]'s fingerprint check misses (a
    /// hash collision, a hand-edited checkpoint, a same-source edit that
    /// changes the `State` type without changing the file's fingerprint), a
    /// cycle that fails with [`DriverError::StateDecode`] while `state_json`
    /// was `Some` (a restored or prior-cycle state, not the very first cycle)
    /// is retried EXACTLY ONCE from fresh `initialState` rather than
    /// propagated — logged loudly first. If the retry ALSO fails, that is an
    /// ordinary cycle error and takes the existing F3 (`Failed`/`Poisoned`)
    /// path, same as any other error; this is a single retry, not a new
    /// ladder rung. A `StateDecode` when `state_json` is already `None` means
    /// the harness source's own `initialState`/`FromJSON State` disagree —
    /// a real bug in the harness, not a stale checkpoint — and propagates as
    /// it does for every other cycle error.
    pub async fn run_loop(
        &mut self,
        source: &HarnessSource,
        auto: bool,
    ) -> Result<(), DriverError> {
        self.refuse_if_poisoned()?;
        let mut state_json: Option<Json> = self.restore(source).await?;
        // Uniform restart rule: any restored checkpoint means this is not the
        // very first cycle ever, so the between-turns gate must present
        // before the next turn runs — whether the prior process crashed
        // mid-turn (the turn reruns, per existing at-least-once semantics,
        // and the operator is asked again first) or while genuinely parked on
        // the gate (asked again, no different from any other restart). Only
        // a first-ever run (no checkpoint at all, `last_checkpoint` is
        // `None`) skips straight into the loop, which asks the seed question
        // via the authored `askUser`.
        let mut first = self.last_checkpoint.is_none();
        loop {
            if !first && !auto {
                self.between_loops_gate().await?;
            }
            first = false;
            let outcome = match self
                .run_one_loop_iteration(source, state_json.as_ref())
                .await
            {
                Ok(outcome) => outcome,
                Err(DriverError::StateDecode(detail)) if state_json.is_some() => {
                    tracing::warn!(
                        detail = %detail,
                        "cycle failed to decode its restored State — retrying once from \
                         fresh initialState instead of taking the process down"
                    );
                    state_json = None;
                    // The carried state's loop history goes with it (same
                    // reasoning as restore's compaction drop).
                    self.iteration = 0;
                    self.discard_resident_state();
                    self.run_one_loop_iteration(source, state_json.as_ref())
                        .await?
                }
                Err(e) => return Err(e),
            };
            state_json = Some(outcome.state_json);
        }
    }

    /// Reload the checkpoint at [`Self::checkpoint_path`], if one is there
    /// yet, returning its `State` JSON (or `None` for a first-ever run — no
    /// checkpoint has been committed). Restores `self.last_compaction`,
    /// `self.checkpoint_generation`, and `self.iteration` from the same
    /// record, so the first render after a restart feeds the same
    /// compaction summary the prior process distilled, the next commit
    /// continues the generation sequence rather than restarting it at 1,
    /// and the loop-metadata count resumes at the right number instead of
    /// resetting to `0`.
    ///
    /// `source`'s fingerprint identifies the harness file THIS process just
    /// loaded. A restored checkpoint whose fingerprint disagrees is DISCARDED
    /// rather than restored: the checkpoint's `State` was produced by a
    /// DIFFERENT harness source and is not safe to decode against the
    /// current one (a mismatched `State` shape crashes the process on boot —
    /// the whole reason this check exists). [`Event::HarnessSourceChanged`]
    /// is still emitted, carrying both fingerprints, as the durable record of
    /// what happened; the generation counter still adopts
    /// `checkpoint.generation` so it stays monotonic across the restart, but
    /// `self.last_compaction` is left `None` (a compaction summary describes
    /// the discarded harness's loop, not this one) and the run starts fresh
    /// from `initialState`, exactly the first-ever-run path. A harness file
    /// that self-edits and restarts therefore loses its accumulated `State`
    /// even when the `State` TYPE didn't change — a real cost, taken
    /// deliberately: a lost `State` costs a run, a decoded-then-poisoned one
    /// costs the process.
    ///
    /// Called by [`Self::run_loop`] at start; exposed so a restart-durability
    /// test can drive the same reload path without entering the
    /// forever-loop.
    pub async fn restore(&mut self, source: &HarnessSource) -> Result<Option<Json>, DriverError> {
        self.refuse_if_poisoned()?;
        let Some(checkpoint) = persistence::load_checkpoint(&self.checkpoint_path)? else {
            return Ok(None);
        };
        // Kept whole so `Self::run_loop` can tell "a checkpoint exists" from
        // "first-ever run" — the uniform restart rule (see that method's
        // doc): any prior checkpoint means the between-turns gate presents
        // before the next turn, no marker needed.
        self.last_checkpoint = Some(checkpoint.clone());
        self.checkpoint_generation = Some(checkpoint.generation());
        self.iteration = checkpoint.iteration().get();
        // Neither is scoped to the harness source: the operator's pending
        // utterance and the ask-id high-water mark are driver-runtime facts,
        // not `State`, so both carry forward even through the
        // fingerprint-mismatch branch below (which only discards `State`).
        self.pending_operator_input = checkpoint.pending_operator_input().map(str::to_string);
        self.ask_id_counter
            .store(checkpoint.ask_id_high_water(), Ordering::SeqCst);
        if checkpoint.harness_source != source.fingerprint {
            // CARRY the state forward anyway (revised 2026-08-15, with the
            // operator). History matters here: restore once returned the
            // stale state unconditionally and a shape-incompatible decode
            // CRASHED the process on boot (the live-dogfood defect the
            // discard branch was added for). The discard fixed the crash by
            // making EVERY harness edit lossy — a prompt tweak wiped
            // threads/scratch. What makes carry-forward safe NOW is
            // [`Self::run_loop`]'s `DriverError::StateDecode` retry (added
            // after the discard): a genuinely incompatible state fails the
            // first cycle's decode LEGIBLY and the loop retries once from
            // `initialState` — the crash cannot recur, and a
            // shape-compatible edit keeps its accumulated state.
            // `last_compaction` still drops (it narrates the old source's
            // loop); iteration carries with the state and is zeroed by the
            // retry arm if the state falls back.
            tracing::info!(
                restored_fingerprint = %checkpoint.harness_source,
                current_fingerprint = %source.fingerprint,
                "harness source changed since the checkpoint — carrying the persisted \
                 state forward (a shape-incompatible state falls back to initialState \
                 via the StateDecode retry)"
            );
            self.emit(Event::HarnessSourceChanged {
                restored_fingerprint: checkpoint.harness_source,
                current_fingerprint: source.fingerprint.clone(),
            });
            self.last_compaction = None;
            return Ok(Some(checkpoint.state));
        }
        self.last_compaction = checkpoint.compaction;
        Ok(Some(checkpoint.state))
    }

    /// Commit the checkpoint for a cycle that just completed successfully:
    /// `state` (that cycle's own returned `State`), `self.last_compaction`
    /// (the compaction summary in force at this same moment — a mid-loop
    /// compaction already updated it in place, so a cycle that compacted and
    /// one that didn't commit through the same path), and `self.iteration`
    /// (already advanced by [`Self::run_one_loop_iteration`] before this call) go
    /// into one [`persistence::Checkpoint`], written atomically under the
    /// next generation. Called once, at the end of [`Self::run_one_loop_iteration`]'s
    /// success path — the ONLY place a checkpoint is written, so a state, a
    /// summary, and an iteration count read back together are always from
    /// the same generation.
    fn commit_checkpoint(
        &mut self,
        source: &HarnessSource,
        state: &Json,
    ) -> Result<(), DriverError> {
        let checkpoint = persistence::Checkpoint::committed(
            self.checkpoint_generation,
            state.clone(),
            self.last_compaction.clone(),
            source.fingerprint.clone(),
            persistence::LoopIteration::new(self.iteration),
        )
        .with_ask_id_high_water(self.ask_id_counter.load(Ordering::SeqCst));
        persistence::save_checkpoint(&self.checkpoint_path, &checkpoint)?;
        self.checkpoint_generation = Some(checkpoint.generation());
        self.last_checkpoint = Some(checkpoint);
        Ok(())
    }

    /// The between-loops human checkpoint: an ORDINARY operator form —
    /// [`between_loops_gate_shape`] — presented through
    /// [`Self::present_askuser_form`], the same funnel every `askUser` ask
    /// uses (precedent: `Harness::escalate_to_operator`'s `AllocateMore`/
    /// `Abort` form, driver-authored the same way). No dedicated gate
    /// mechanism, no checkpoint write of its own: the uniform restart rule
    /// ([`Self::run_loop`]'s doc) covers the crash-while-parked case without
    /// one — a kill here just means the next boot re-presents this same ask
    /// before running the next turn, same as a kill mid-turn means the turn
    /// reruns.
    ///
    /// An empty (or whitespace-only) `steer` field is a plain continue; any
    /// other text becomes [`Self::pending_operator_input`] — the operator's
    /// one channel for initiating — threaded into the next cognition
    /// window's framing exactly as before.
    async fn between_loops_gate(&mut self) -> Result<(), DriverError> {
        let shape = between_loops_gate_shape(self.iteration);
        let mut reprompts: u32 = 0;
        let submission = self
            .present_askuser_form(&mut reprompts, FormSource::OuterLoop, &shape)
            .await?;
        let operator_text = submission
            .get("steer")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(text) = &operator_text {
            self.emit(Event::OperatorMessage { text: text.clone() });
        }
        self.pending_operator_input = operator_text;
        Ok(())
    }

    /// Drive `Loaded.loop __selfHarnessState` (spliced via
    /// [`state_cross::state_in`]) as a suspendable fragment on the outer
    /// session, servicing every `runLLMTurn` hole it suspends on via
    /// [`Self::service_typed_request_suspension`] until it completes. Returns the
    /// completed `State` value and the DataConTable its OWN compile produced
    /// (the table every hole along this same continuation classifies
    /// against — `resume` never recompiles).
    ///
    /// `precompiled`, when `Some`, is this cycle's loop entry from
    /// [`Self::compile_loop_entry`] — used AS-IS instead of compiling one
    /// here, which is how [`Self::run_one_loop_iteration`] pays only ONE fused spawn
    /// for both `render` and `loop`. `None` compiles it here via
    /// [`Self::compile_outer`], exactly as before fusion — a direct caller
    /// (a test driving this fragment in isolation) keeps working unfused.
    ///
    /// Emergency compaction does NOT happen here at loop end — it fires
    /// MID-LOOP via [`Self::maybe_compact_answerer`] (checked between the
    /// answerer's holes/rounds against its real accumulated context), setting
    /// `self.cycle_compaction`/`self.last_compaction` in place while the loop
    /// continues under the summary.
    async fn run_loop_fragment(
        &mut self,
        prior_state: Option<&Json>,
        precompiled: Option<CompiledTurn>,
    ) -> Result<(Value, DataConTable), DriverError> {
        self.loop_inference_calls.store(0, Ordering::SeqCst);
        self.cycle_compaction = None;
        self.loop_state_json = prior_state.cloned();

        // Create the ONE render-seeded answerer session for this whole
        // loop, up front — every `runLLMTurn` hole pushes onto it, so hole #2
        // sees hole #1's exchange (the accumulating context window). Retired
        // in `retire_typed_request_agent` once the loop completes (or errors out).
        let answerer =
            self.agent
                .create_root_framed("loop answerer", "", self.answerer_framing.clone())?;
        // ONE SESSION: the answerer node runs as a REALM on the shared outer
        // machine (its turns park beside the loop's own frame; its values —
        // closures included — are born in the loop's heap). Realm minted per
        // loop; retirement is that realm's scope exit via terminate_node.
        let sid = self.outer_sid()?;
        self.agent.force_attached(answerer, Actor::Operator, sid)?;
        let realm = self.mint_realm();
        self.agent.set_node_realm(answerer, realm);
        self.answerer = Some(AgentSessionMode::ReusableLoop {
            node: answerer,
            realm,
        });

        let result = self.run_loop_fragment_inner(prior_state, precompiled).await;
        self.retire_typed_request_agent();
        result
    }

    /// Retire the current loop's answerer node (terminalize it and drop its
    /// session), so the next loop starts from a fresh render-seeded one.
    /// Idempotent — a no-op if no answerer is live.
    fn retire_typed_request_agent(&mut self) {
        if let Some(lease) = self.answerer.take() {
            let node = lease.node();
            let _ = self.agent.terminate_node(node, "loop answerer retired");
            self.fork_child_seq.lock().remove(&node);
        }
    }

    /// Mint a fresh, globally-unique (within this driver) realm id — `&self`
    /// so concurrent fanout/fork children ([`Self::service_outer_fanout`])
    /// can each mint their OWN realm alongside the single reused
    /// [`Self::answerer`]'s, without contending for `&mut self`.
    fn mint_realm(&self) -> tidepool_codegen::jit_machine::RealmId {
        tidepool_codegen::jit_machine::RealmId(
            self.iteration_realm.fetch_add(1, Ordering::SeqCst) + 1,
        )
    }

    /// The body of [`Self::run_loop_fragment`] — run `loop`, service each
    /// `runLLMTurn` hole against the pre-created per-loop answerer
    /// (`self.answerer`), and return the completed `State` + table. Split out
    /// so [`Self::run_loop_fragment`] can retire the answerer whether this
    /// succeeds or errors.
    ///
    /// Mid-loop, in-place compaction: AFTER each hole is serviced — a
    /// natural boundary between the answerer's holes — [`Self::maybe_compact_answerer`]
    /// checks the answerer's real accumulated context against the threshold and,
    /// if past it, summarizes + replaces the answerer's context IN PLACE so the
    /// loop's REMAINING holes continue under a smaller window (no abort).
    ///
    /// `precompiled`, when `Some`, is used as-is instead of calling
    /// [`Self::compile_outer`] — see [`Self::run_loop_fragment`]'s doc.
    async fn run_loop_fragment_inner(
        &mut self,
        prior_state: Option<&Json>,
        precompiled: Option<CompiledTurn>,
    ) -> Result<(Value, DataConTable), DriverError> {
        let compiled = match precompiled {
            Some(compiled) => compiled,
            None => {
                // The unfused path mints its OWN plan — a direct fragment
                // API a test drives in isolation, never called from
                // `run_one_loop_iteration` (which mints one plan and passes it to
                // `compile_loop_entry` instead). See `LoopEntryPlan`'s doc.
                let (code, resume_helpers) = self.take_loop_entry().into_code_and_helpers();
                let helpers = format!(
                    "{}{}{}",
                    state_cross::state_in(prior_state),
                    state_cross::operator_msg_in(self.pending_operator_input.as_deref()),
                    resume_helpers,
                );
                self.compile_outer(&code, &helpers, "loop")?
            }
        };

        // The scheduler's FIFO ready queue (PRD 20 S1-L4) — EVERY suspension
        // this loop drives (the primary `loop` chain's own, and any green
        // thread's) goes through it uniformly: pop one already-produced
        // outcome, classify it, service it (pushing back whatever it
        // produces next), repeat. A `Completed` can only ever come from the
        // PRIMARY chain's own top-level fragment — a green thread's body is
        // always wrapped (`asyncSpawn`) to end by SUSPENDING on
        // `AsyncDoneWith`, never by completing — so seeing one here IS this
        // cycle's `loop` finishing, regardless of any other thread still
        // parked (an unawaited thread is a legitimate orphan, same
        // starvation contract `Tidepool.Async`'s module doc already states).
        let mut ready: VecDeque<GreenReady> = VecDeque::new();
        {
            let sid = self.outer_sid()?;
            let first = self
                .agent
                .with_session(sid, |s| s.run("loop", &compiled.expr, &compiled.table))
                .map_err(|e| DriverError::Session(e.to_string()))?
                .map_err(|e| map_run_error("loop run failed", e.to_string()))?;
            ready.push_back(GreenReady {
                chain: GreenChain::Primary,
                outcome: first,
            });
        }

        let mut threads: HashMap<i64, GreenThread> = HashMap::new();
        let mut waiters: HashMap<i64, Vec<(GreenChain, String)>> = HashMap::new();
        let mut next_tid: i64 = 1;
        let mut next_thread_realm: u64 = 1;

        // EVERY branch below hands back a `ServicedSuspension` instead of directly
        // pushing to `ready`/breaking the loop — see that type's doc for why:
        // a servicing arm that computed a next outcome and merely assigned it
        // to a dead local, rather than returning it, is the incident this
        // subsumes into the type. `SuspensionRouting::Green` is the one exception,
        // documented at `ServicedSuspension` and at `Self::service_green_hole`.
        //
        // F6: wrapped in a bare (non-`move`) `async` block so every `?`
        // inside the loop body (`classify_hole`, each `service_*().await?`,
        // every resume's `map_err(...)?`) returns from THIS block instead of
        // from the whole function — `?` always targets the nearest enclosing
        // fn/closure/async-block, and an `async {}` block counts. Without
        // this, an early `?` skipped the structured-concurrency sweep below
        // entirely, leaking every still-open thread realm this fragment
        // spawned. No `move`: `threads`/`waiters`/`ready`/`next_tid`/
        // `next_thread_realm` (and `self`) stay borrowed for the block's
        // span and are still owned by this function afterward, which is what
        // the sweep below needs.
        let outcome_result: Result<(Value, DataConTable), DriverError> = async {
            loop {
            let Some(GreenReady { chain, outcome }) = ready.pop_front() else {
                break Err(DriverError::Session(
                    "green scheduler starved: no ready work and the outer loop never completed \
                     (a parked thread with no waiter and no completion path)"
                        .into(),
                ));
            };
            let serviced: ServicedSuspension = match outcome {
                ResidentOutcome::Completed { result, .. } => ServicedSuspension::Completed {
                    result: result.into_value(),
                    table: compiled.table.clone(),
                },
                ResidentOutcome::Suspended { hole, request, .. } => {
                    let classified =
                        engine::classify_hole(&request, &compiled.table, &compiled.asks)?;
                    match &classified.routing {
                        SuspensionRouting::RunLLMTurn { site, ty } => {
                            let answer = self
                                .service_typed_request_suspension(
                                    site.get(),
                                    ty.as_deref(),
                                    compiled.asks.modules_of(site.get()),
                                    &classified.prompt,
                                    &compiled.table,
                                )
                                .await?;
                            // Between holes — if the answerer's accumulated
                            // context has crossed threshold, compact + replace its
                            // context IN PLACE now, so the NEXT hole drives under
                            // the smaller window.
                            //
                            // This runs only AFTER `service_typed_request_suspension` has
                            // already finalized THIS hole's answer
                            // (`take_finalized_value_keep_open` consumed the finalize
                            // continuation and returned the session to idle —
                            // harness.rs `take_finalized_value_keep_open`). So the
                            // summarize turn `maybe_compact_answerer` drives sees the
                            // last answer already IN the transcript and cannot drop
                            // it: a future refactor that moves this call BEFORE the
                            // answer is taken would compact a mid-finalize session —
                            // do not.
                            self.maybe_compact_answerer().await?;
                            let sid = self.outer_sid()?;
                            let next = self
                                .agent
                                .with_session(sid, |s| match answer {
                                    FinalAnswer::Value(v) => s.resume(hole, v),
                                    // Pillar B: the closure payload is
                                    // DELIVERED by handle — same heap, no
                                    // bridge, no sentinel.
                                    FinalAnswer::Handle(h) => s.resume_handle(hole.cont_id(), h),
                                })
                                .map_err(|e| DriverError::Session(e.to_string()))?
                                .map_err(|e| {
                                    DriverError::Session(format!("loop resume failed: {e}"))
                                })?;
                            ServicedSuspension::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        // The AUTHORED loop itself evaluated `askUser`/`note`
                        // (`Tidepool.Form`, auto-imported because `AskUser` is in
                        // `outer_decls`) — a form or narration raised DIRECTLY by
                        // the loop, distinct from an answerer's own
                        // (`service_askuser_hole`). Service it via the same
                        // operator gate and resume the OUTER session; the helper
                        // loops over `askUser`'s Haskell-side decode-retry (a bad
                        // submission re-suspends on a fresh `AskUserWith`) and any
                        // interleaved `note`, returning the first outcome that
                        // ISN'T another operator form/note — a `runLLMTurn`
                        // suspension the main loop then services, or a completion.
                        SuspensionRouting::AskUser { .. } | SuspensionRouting::Note { .. } => {
                            let next = self
                                .service_outer_askuser_hole(
                                    hole.clone(),
                                    classified.routing.clone(),
                                    &compiled,
                                )
                                .await?;
                            ServicedSuspension::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        // The AUTHORED loop called a Subagent verb
                        // (`spawnAgent`/`spawnAgentRaw`) — dispatch the
                        // ORIGINAL request into the driver-owned handler
                        // (suspension-serviced; the outer handled prefix
                        // stays empty) and resume with its typed response.
                        SuspensionRouting::Subagent => {
                            let value = self.service_outer_subagent(
                                &request,
                                &compiled.table,
                                FormSource::OuterLoop,
                            )?;
                            let sid = self.outer_sid()?;
                            let next = self
                                .agent
                                .with_session(sid, |s| s.resume(hole, value))
                                .map_err(|e| DriverError::Session(e.to_string()))?
                                .map_err(|e| {
                                    DriverError::Session(format!("subagent resume failed: {e}"))
                                })?;
                            ServicedSuspension::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        // Console/Worktree/RepoEvent/Exec (S1-L1) / Journal
                        // (run-journal lane) — same suspension-servicing
                        // shape as Subagent above, generalized over
                        // `OuterEffectKind`.
                        SuspensionRouting::OuterEffect(kind) => {
                            let kind = *kind;
                            // `RepoEventAwait` (PRD 20 S1-L4 wave 2) is the
                            // ONE outer-row suspension whose handler-side
                            // implementation can BLOCK indefinitely
                            // (`repo_event_await`'s own reconcile/check/sleep
                            // loop). Routing it through the ordinary
                            // dispatch-then-resume path below would stall
                            // THIS WHOLE ready-queue loop — every other
                            // chain's already-ready work, including a
                            // sibling green thread's — until it happens to
                            // match. Service it non-blockingly instead: poll
                            // with the SAME handler's plain (non-sleeping)
                            // drain; a match resumes the hole immediately,
                            // exactly as if `RepoEventAwait` itself had
                            // returned it; an EMPTY batch leaves the hole
                            // genuinely parked — `ServicedSuspension::LeaveParked`,
                            // reinserted at the BACK of `ready` so every other
                            // already-ready chain runs first, revisited by
                            // this same arm on a later iteration. The
                            // caller-observable contract (block until a
                            // match) is unchanged — only who does the waiting
                            // is. `repo_event_await`'s own implementation,
                            // and every other RepoEvent verb, are untouched:
                            // this is a DRIVER SERVICING CHOICE, made only
                            // for this one constructor.
                            if kind == engine::OuterEffectKind::RepoEvent
                                && engine::con_name(&request, &compiled.table)
                                    == Some("RepoEventAwait")
                            {
                                match self.poll_repo_event_await(&request, &compiled.table)? {
                                    None => {
                                        // Nothing else ready to interleave
                                        // with right now — a real sibling
                                        // wakes this up on the very next
                                        // iteration regardless (it's already
                                        // ahead in the queue), so this only
                                        // ever fires while genuinely waiting
                                        // on external progress (a deadline
                                        // elapsing, another chain not yet
                                        // scheduled). Bounds the poll to a
                                        // cooperative cadence instead of a
                                        // tight CPU spin; `tokio::time::sleep`
                                        // yields this task rather than
                                        // blocking the runtime.
                                        if ready.is_empty() {
                                            tokio::time::sleep(std::time::Duration::from_millis(
                                                5,
                                            ))
                                            .await;
                                        }
                                        ServicedSuspension::LeaveParked(GreenReady {
                                            chain,
                                            outcome: ResidentOutcome::Suspended {
                                                output: Vec::new(),
                                                hole,
                                                request,
                                            },
                                        })
                                    }
                                    Some(value) => {
                                        let sid = self.outer_sid()?;
                                        let next = self
                                            .agent
                                            .with_session(sid, |s| s.resume(hole, value))
                                            .map_err(|e| DriverError::Session(e.to_string()))?
                                            .map_err(|e| {
                                                DriverError::Session(format!(
                                                    "RepoEventAwait resume failed: {e}"
                                                ))
                                            })?;
                                        ServicedSuspension::Resumed(GreenReady {
                                            chain,
                                            outcome: next,
                                        })
                                    }
                                }
                            } else {
                                let value =
                                    self.service_outer_effect(kind, &request, &compiled.table)?;
                                let sid = self.outer_sid()?;
                                let next = self
                                    .agent
                                    .with_session(sid, |s| s.resume(hole, value))
                                    .map_err(|e| DriverError::Session(e.to_string()))?
                                    .map_err(|e| {
                                        DriverError::Session(format!(
                                            "outer effect resume failed: {e}"
                                        ))
                                    })?;
                                ServicedSuspension::Resumed(GreenReady {
                                    chain,
                                    outcome: next,
                                })
                            }
                        }
                        // `Tidepool.Async`'s substrate (PRD 20 S1-L4) — raised
                        // either by the loop itself or by a green thread's own
                        // body. Unlike every other arm here, servicing may push
                        // ZERO, ONE, or TWO ready items (a park with no terminal
                        // candidate pushes none; a spawn pushes both the resumed
                        // spawner and the freshly started thread) and mutates the
                        // thread table / waiter map — it does not fit
                        // `ServicedSuspension` (see that type's doc), so it owns
                        // `ready` directly and this arm hands nothing back.
                        SuspensionRouting::Green => {
                            // Raw delivery never reports node-blocked. The
                            // AUTHORED plane keeps misuse a hard error —
                            // authored code fails loud, it is not coached.
                            if let GreenHoleServiced::Misuse(msg) = self
                                .service_green_hole(
                                    None,
                                    chain,
                                    hole.cont_id(),
                                    &request,
                                    &compiled.table,
                                    &mut threads,
                                    &mut waiters,
                                    &mut next_tid,
                                    &mut next_thread_realm,
                                    &mut ready,
                                    GreenDelivery::Raw,
                                )
                                .await?
                            {
                                return Err(DriverError::Session(msg));
                            }
                            continue;
                        }
                        // `runLLMTurnFork @T`/`runLLMTurnFanout @T` raised
                        // DIRECTLY by the AUTHORED loop (`RunLLMTurn`'s own
                        // fork/fanout payload — reachable wherever
                        // `RunLLMTurn` is in the row, so the outer session
                        // needs no separate `Fork` decl): S1-L4 — service
                        // every prompt CONCURRENTLY, each in its own
                        // freshly-minted answerer realm, then resume this
                        // ONE hole once with the assembled answer.
                        // `source` is unread here: the outer row is
                        // `outer_decls()`, which has no `Fork` effect, so the
                        // only verb that can raise this routing on the outer
                        // session is `runLLMTurnFork`/`runLLMTurnFanout` —
                        // `ForkSource::RunLLMTurn` by construction.
                        SuspensionRouting::Fork {
                            site,
                            ty,
                            fan,
                            prompts,
                            source: _,
                        } => {
                            let value = self
                                .service_outer_fanout(
                                    site.get(),
                                    ty.as_deref(),
                                    compiled.asks.modules_of(site.get()),
                                    *fan,
                                    &classified.prompt,
                                    prompts,
                                    &compiled.table,
                                )
                                .await?;
                            let sid = self.outer_sid()?;
                            let next = self
                                .agent
                                .with_session(sid, |s| s.resume(hole, value))
                                .map_err(|e| DriverError::Session(e.to_string()))?
                                .map_err(|e| {
                                    DriverError::Session(format!("fanout resume failed: {e}"))
                                })?;
                            ServicedSuspension::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        other => {
                            break Err(DriverError::Session(format!(
                                "outer loop suspended on an unserviceable hole ({other:?}) — \
                                 the Harness monad exposes runLLMTurn, askUser, note, \
                                 spawnAgent, say, \
                                 createWorktree/lookupWorktree/listWorktrees/worktreeBranch/\
                                 worktreeHead, withHandler (repository events), run/runIn/\
                                 runArgv, record, and Tidepool.Async's async/wait/waitEither/cancel \
                                 only"
                            )))
                        }
                    }
                }
            };
            match serviced {
                ServicedSuspension::Completed { result, table } => break Ok((result, table)),
                ServicedSuspension::Resumed(gr) | ServicedSuspension::LeaveParked(gr) => {
                    ready.push_back(gr);
                }
            }
        }
        }
        .await;

        // Structured-concurrency scope exit: every thread this loop spawned
        // is scoped to this ONE `loop` fragment run — close every realm that
        // is still open so its frames/handles don't outlive the cycle that
        // created them. That is Running threads (never joined/cancelled by
        // the authored code) AND Settled ones: a settle parks the thread's
        // `AsyncDoneWith` frame FOREVER by design (the arm never resumes
        // it), so a settled realm left unclosed is a permanently-parked hole
        // on the shared session — enough of them and the machine is never
        // quiescent again, which blocks rotation until the fragment ceiling
        // kills the run. Only a Cancelled thread's realm is already closed
        // (the cancel arm does it eagerly).
        if let Ok(sid) = self.outer_sid() {
            for entry in threads.values() {
                if !matches!(entry.state, GreenThreadState::Cancelled) {
                    let _ = self.agent.with_session(sid, |s| s.close_realm(entry.realm));
                }
            }
        }
        outcome_result
    }

    /// [`Harness::with_session_retrying`] when `host` names a real answerer
    /// node (F4: the answerer-plane green scheduler, whose host node may have
    /// opted into contention retry via
    /// [`Harness::set_retry_checkout_on_contention`]), else plain
    /// [`Harness::with_session`] — the AUTHORED outer loop's own green
    /// servicing (`run_loop_fragment_inner`) runs against the node-LESS
    /// outer session and has no node to look a retry flag up on.
    async fn with_session_maybe_retrying<T>(
        &self,
        host: Option<NodeId>,
        sid: tidepool_repr::SessionId,
        f: impl FnOnce(&mut Session) -> T,
    ) -> Result<T, HarnessError> {
        match host {
            Some(node) => self.agent.with_session_retrying(node, sid, f).await,
            None => self.agent.with_session(sid, f),
        }
    }

    /// Deliver a serviced green suspension's own resume per
    /// [`GreenDelivery`] — see that type's doc for the plane split.
    async fn deliver_green_resume(
        &self,
        host: Option<NodeId>,
        sid: tidepool_repr::SessionId,
        delivery: &GreenDelivery<'_>,
        chain: GreenChain,
        hole: &str,
        answer: GreenAnswer,
        ready: &mut VecDeque<GreenReady>,
        what: &str,
    ) -> Result<(), DriverError> {
        match delivery {
            GreenDelivery::Raw => {
                let next = self
                    .with_session_maybe_retrying(host, sid, |s| match answer {
                        GreenAnswer::Value(v) => s.resume(ResidentHole::plain(hole), v),
                        GreenAnswer::BorrowedRoot(h) => s.resume_handle_borrowed(hole, h),
                    })
                    .await
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(|e| DriverError::Session(format!("{what} resume failed: {e}")))?;
                ready.push_back(GreenReady {
                    chain,
                    outcome: next,
                });
                Ok(())
            }
            GreenDelivery::Node { node, hole } => match answer {
                GreenAnswer::Value(v) => Ok(retry_on_turn_in_flight_async(|| {
                    self.agent.resume_with_value(*node, hole, v.clone())
                })
                .await?),
                GreenAnswer::BorrowedRoot(h) => Ok(retry_on_turn_in_flight_async(|| {
                    self.agent.resume_with_borrowed_root(*node, hole, h)
                })
                .await?),
            },
        }
    }

    /// Service one `Tidepool.Async` suspension (PRD 20 S1-L4): decode which
    /// of the six `Async*With` verbs `request` is by CONSTRUCTOR NAME (never
    /// in [`engine::classify_hole`] — the payload may carry a live closure,
    /// see [`SuspensionRouting::Green`]'s doc) and act, mutating the scheduler's
    /// thread table / waiter map / ready queue in place.
    ///
    /// Deliberately returns `Result<(), DriverError>`, not a [`ServicedSuspension`]
    /// — checked and rejected before the rest of the dispatcher adopted that
    /// sum. Its six arms push zero (`AsyncJoinAnyWith` with no terminal
    /// candidate, `AsyncDoneWith` on a cancelled/already-settled thread),
    /// one, two (`AsyncSpawnWith`: the resumed spawner and the freshly
    /// started thread), or an arbitrary N (`AsyncDoneWith`/`AsyncCancelWith`
    /// waking every parked joiner) items onto `ready`, and several never
    /// resume the triggering hole at all (`AsyncDoneWith`'s own hole stays
    /// parked forever, its frame reclaimed only when the thread's realm
    /// eventually closes). `ServicedSuspension::{Resumed,LeaveParked}` both assume
    /// "exactly one hole, exactly one outcome, handed back once" — this
    /// method's job is precisely to not have that shape, so forcing it into
    /// the sum would mean returning `Vec<ServicedSuspension>` (or a payload-free
    /// `Handled` marker), neither of which catches anything a caller
    /// forgetting to `?` this `Result` doesn't already catch today. Mirrors
    /// [`Self::service_outer_subagent`]'s shape (driver-owned, suspension-
    /// serviced, no handler) but is not a single dispatch-then-resume: a
    /// spawn starts a NEW top-level run and a park-until-terminal join may
    /// register a waiter instead of answering immediately.
    #[allow(clippy::too_many_arguments)]
    async fn service_green_hole(
        &self,
        // The HOST answerer node — the node whose `retry_checkout_on_contention`
        // opt-in (F4) governs every raw `with_session` this call makes against
        // the shared session, regardless of whether `delivery` targets this
        // same node's own hole (`GreenDelivery::Node`) or a raw thread chain
        // (`GreenDelivery::Raw`): a thread belongs to this host's own window,
        // so it contends on exactly the same checkout races its host does.
        // `None` for the AUTHORED outer loop's own node-less green servicing
        // (`run_loop_fragment_inner`), which has no node to opt in with.
        host: Option<NodeId>,
        chain: GreenChain,
        hole: &str,
        request: &Value,
        table: &DataConTable,
        threads: &mut HashMap<i64, GreenThread>,
        waiters: &mut HashMap<i64, Vec<(GreenChain, String)>>,
        next_tid: &mut i64,
        next_realm: &mut u64,
        ready: &mut VecDeque<GreenReady>,
        delivery: GreenDelivery<'_>,
    ) -> Result<GreenHoleServiced, DriverError> {
        let sid = self.outer_sid()?;
        match engine::con_name(request, table) {
            // Field 1 is the thread body — ALWAYS a closure by construction
            // (`asyncSpawn` wraps every body in a lambda so the
            // closure-sentinel scan fires even for `async (pure 5)`; see
            // `tidepool-mcp/src/effect_defs.rs`'s `green_effect_def!` doc).
            Some("AsyncSpawnWith") => {
                let tid = *next_tid;
                *next_tid += 1;
                // Tagged with a high bit so a thread realm can never collide
                // with `OUTER_REALM` (0), a per-loop answerer realm
                // (`iteration_realm`, small increasing ints), or
                // `ResidentSession::run_child`'s throwaway realms (bit 63).
                let realm = tidepool_codegen::jit_machine::RealmId((1u64 << 61) | *next_realm);
                *next_realm += 1;
                threads.insert(
                    tid,
                    GreenThread {
                        realm,
                        state: GreenThreadState::Running,
                    },
                );
                // Mint the spawner's body custody AND start the thread in ONE
                // checkout (F5): the mint (`finalized_handle`) and the
                // consume (`run_forked`) used to be two SEPARATE
                // `with_session` calls, with the minted `RootCustody` moved
                // into the second call's closure. If that second checkout
                // ever refused (a concurrent sibling holding the machine —
                // the F4 contention class — or the session slot gone),
                // `with_session` returns `Err` BEFORE ever calling the
                // closure, so the closure — and the `RootCustody` it
                // captured — drops unconsumed, and `RootCustody::Drop`
                // panics by design (a leak detector), unwinding the whole
                // driver task instead of surfacing an ordinary
                // `DriverError`. Folding both steps into ONE closure of ONE
                // `with_session` call removes the window entirely: the
                // custody is minted only AFTER the checkout has already
                // succeeded, so there is no fallible step between mint and
                // consume for an early return to land on.
                let thread_start = self
                    .with_session_maybe_retrying(
                        host,
                        sid,
                        |s| -> Result<ResidentOutcome, String> {
                            let body = s.finalized_handle(hole).ok_or_else(|| {
                                "AsyncSpawnWith: spawner frame carries no untaken body closure"
                                    .to_string()
                            })?;
                            s.run_forked("async_thread", body, realm, Some(table))
                                .map_err(|e| format!("run_forked failed: {e}"))
                        },
                    )
                    .await
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(DriverError::Session)?;
                // Spawner-continues-first (the ready queue's own choice, per
                // this lane's scaffold doc) — resume the spawner immediately
                // with the fresh id, then start the thread; either push lands
                // on `ready` so both eventually run regardless.
                //
                // Boxed via `i64: ToCore` (an `I#` Con looked up in THIS
                // compile's own table) — NOT `engine::json_answer_to_value`
                // (which bridges to `Tidepool.Aeson.Value`, the wrong TYPE
                // for a plain `Int` `send` delivers natively — that generic
                // wire path is for an `askUser` submission's `FromJSON`
                // decode) and NOT a bare `Value::Lit` (unboxed; only
                // tolerated by the JIT's OWN synthesized `App` in
                // `apply_finalized`/`run_forked`, not by arbitrary compiled
                // Haskell that pattern-matches `case x of I# n#`).
                let tid_value = tid
                    .to_value(table)
                    .map_err(|e| DriverError::Session(format!("AsyncSpawnWith tid box: {e}")))?;
                // Spawner-continues-first: the spawner's resume lands (Raw:
                // pushed to `ready` ahead of the thread; Node: the node's own
                // pending record refreshes) before the fresh thread's first
                // outcome enters the queue.
                self.deliver_green_resume(
                    host,
                    sid,
                    &delivery,
                    chain,
                    hole,
                    GreenAnswer::Value(tid_value),
                    ready,
                    "AsyncSpawnWith spawner",
                )
                .await?;
                ready.push_back(GreenReady {
                    chain: GreenChain::Thread(tid),
                    outcome: thread_start,
                });
                Ok(GreenHoleServiced::Proceed(false))
            }
            // A thread's last act. Its own leading `Int` field is always the
            // dummy `0` `asyncSpawn` bakes in — the settling thread's real id
            // is `chain`, not that field (see `GreenChain`'s doc). The
            // AsyncDoneWith hole itself is deliberately NEVER resumed: the
            // payload is taken by reference/handle here, exactly like
            // `finalize`, and the frame stays parked until its realm
            // eventually closes (`cancel`, or this loop's end-of-scope
            // sweep) — nothing is lost by never driving it to a Rust-level
            // `Completed`.
            Some("AsyncDoneWith") => {
                if matches!(delivery, GreenDelivery::Node { .. }) {
                    return Err(DriverError::Session(
                        "AsyncDoneWith delivered on the node chain (scheduler bug: settles \
                         are thread-only)"
                            .into(),
                    ));
                }
                let GreenChain::Thread(tid) = chain else {
                    return Err(DriverError::Session(
                        "AsyncDoneWith suspended on a non-thread chain (scheduler bug: every \
                         thread body is reached only via run_forked)"
                            .into(),
                    ));
                };
                // Decide whether this settle will actually be RECORDED before
                // minting anything. A custody token must not be created on a
                // path that might not consume it: cancellation, if it already
                // landed, wins and a late result is dropped — and dropping an
                // unconsumed `RootCustody` panics, turning a deliberate,
                // benign no-op into a crash. (Before linearization the drop
                // really was benign, which is why the guard below reads as if
                // it still is.)
                let records_result = threads
                    .get(&tid)
                    .is_some_and(|t| matches!(t.state, GreenThreadState::Running));
                if !records_result {
                    self.wake_green_waiters(host, tid, table, sid, waiters, ready)
                        .await?;
                    return Ok(GreenHoleServiced::Proceed(false));
                }
                let answer = if green_field_is_closure(request, 1) {
                    // Owned by the SESSION's realm, deliberately (not the
                    // thread's own) — a result must outlive the thread realm
                    // that produced it, since cancelling or retiring this
                    // thread must not invalidate a waiter's already-delivered
                    // handle.
                    let handle = self
                        .with_session_maybe_retrying(host, sid, |s| {
                            s.finalized_handle_owned_by(hole, OUTER_REALM)
                        })
                        .await
                        .map_err(|e| DriverError::Session(e.to_string()))?
                        .ok_or_else(|| {
                            DriverError::Session(
                                "AsyncDoneWith: thread frame carries no untaken result closure"
                                    .into(),
                            )
                        })?;
                    FinalAnswer::Handle(handle)
                } else {
                    let Value::Con(_, fields) = request else {
                        return Err(DriverError::Session(
                            "AsyncDoneWith: malformed request (not a Con)".into(),
                        ));
                    };
                    let value = fields.get(1).cloned().ok_or_else(|| {
                        DriverError::Session("AsyncDoneWith: missing result field".into())
                    })?;
                    FinalAnswer::Value(value)
                };
                // Unconditional by construction: `records_result` above already
                // established this entry exists and is `Running`, and nothing
                // between here and now can have changed it (cooperative
                // scheduling, single-threaded). So the custody minted above is
                // always consumed on this path.
                if let Some(entry) = threads.get_mut(&tid) {
                    entry.state = GreenThreadState::Settled(match answer {
                        FinalAnswer::Value(v) => GreenResult::Value(v),
                        // The ONE custody transfer: out of the thread's
                        // finalize frame and into the table, which owns it for
                        // the rest of the session realm's life.
                        FinalAnswer::Handle(custody) => GreenResult::Root(custody.into_handle()),
                    });
                    // Wake any `WatchAsync tid` subscriber exactly once — the
                    // `Tidepool.Event.waitEvent`/`Tidepool.Event` completion
                    // watch (PRD 20 S1-L4 wave 2). `records_result` above
                    // already established this is a genuine Running→Settled
                    // transition, so this always fires exactly once per
                    // settle. No-op if `RepoEvent` was never wired —
                    // `WatchAsync` is unusable without it anyway.
                    if let Some(h) = self.handlers.lock().event.as_mut() {
                        h.registry_mut().publish_async_done(tid);
                    }
                }
                self.wake_green_waiters(host, tid, table, sid, waiters, ready)
                    .await?;
                Ok(GreenHoleServiced::Proceed(false))
            }
            Some("AsyncJoinAnyWith") => {
                let ids = green_int_list_field(request, 0, table);
                let winner = ids.iter().copied().find(|&tid| {
                    threads
                        .get(&tid)
                        .is_some_and(|t| !matches!(t.state, GreenThreadState::Running))
                });
                match winner {
                    Some(winner) => {
                        let winner_value = winner.to_value(table).map_err(|e| {
                            DriverError::Session(format!("AsyncJoinAnyWith winner box: {e}"))
                        })?;
                        self.deliver_green_resume(
                            host,
                            sid,
                            &delivery,
                            chain,
                            hole,
                            GreenAnswer::Value(winner_value),
                            ready,
                            "AsyncJoinAnyWith",
                        )
                        .await?;
                    }
                    None => {
                        // None terminal yet. Raw chains park as waiters on
                        // EVERY listed thread — whichever settles/cancels
                        // first wakes them; nothing goes on `ready`. The
                        // NODE chain never registers (the raw waiter wake
                        // must never touch it) — it reports BLOCKED and its
                        // still-pending join is re-serviced (a pure winner
                        // scan) each scheduler iteration.
                        match delivery {
                            GreenDelivery::Raw => {
                                for tid in ids {
                                    waiters
                                        .entry(tid)
                                        .or_default()
                                        .push((chain, hole.to_string()));
                                }
                            }
                            GreenDelivery::Node { .. } => {
                                return Ok(GreenHoleServiced::Proceed(true))
                            }
                        }
                    }
                }
                Ok(GreenHoleServiced::Proceed(false))
            }
            Some("AsyncStatusWith") => {
                let tid = green_int_field(request, 0, table);
                let code: i64 = match threads.get(&tid).map(|t| &t.state) {
                    Some(GreenThreadState::Settled(_)) => 1,
                    Some(GreenThreadState::Cancelled) => 2,
                    _ => 0,
                };
                let code_value = code
                    .to_value(table)
                    .map_err(|e| DriverError::Session(format!("AsyncStatusWith code box: {e}")))?;
                self.deliver_green_resume(
                    host,
                    sid,
                    &delivery,
                    chain,
                    hole,
                    GreenAnswer::Value(code_value),
                    ready,
                    "AsyncStatusWith",
                )
                .await?;
                Ok(GreenHoleServiced::Proceed(false))
            }
            Some("AsyncResultWith") => {
                let tid = green_int_field(request, 0, table);
                let answer = match threads.get(&tid).map(|t| &t.state) {
                    Some(GreenThreadState::Settled(GreenResult::Value(v))) => {
                        GreenResult::Value(v.clone())
                    }
                    // A BORROW of the session-owned root, not a copy of a
                    // custody: the table stays the owner and the next waiter
                    // reads the same root.
                    Some(GreenThreadState::Settled(GreenResult::Root(h))) => GreenResult::Root(*h),
                    _ => {
                        return Ok(GreenHoleServiced::Misuse(format!(
                            "`asyncResult` was called on thread {tid}, which has not \
                             settled (or whose handle is from an earlier round — handles \
                             do not survive a round boundary). Gate with `asyncStatus`, \
                             or use `wait`, and spawn + wait in the SAME block"
                        )))
                    }
                };
                match self
                    .deliver_green_resume(
                        host,
                        sid,
                        &delivery,
                        chain,
                        hole,
                        match answer {
                            GreenResult::Value(v) => GreenAnswer::Value(v),
                            GreenResult::Root(h) => GreenAnswer::BorrowedRoot(h),
                        },
                        ready,
                        "AsyncResultWith",
                    )
                    .await
                {
                    Ok(()) => Ok(GreenHoleServiced::Proceed(false)),
                    // F9: a bind-shaped block (`h <- async (…closure-valued…);
                    // wait h`) parks its OWN turn on a Binding hole, which
                    // cannot honor a borrowed-root resume — model-attributable
                    // (the block bound a closure result), not a mechanism
                    // failure, so it gets the same loud-refusal treatment as
                    // every other async misuse instead of ending the run.
                    Err(DriverError::Agent(HarnessError::BorrowedRootOnBindingHole(_))) => {
                        Ok(GreenHoleServiced::Misuse(format!(
                            "thread {tid}'s result is a closure/function value, and this \
                             block tried to BIND it with `<-` (e.g. `h <- async (…); r <- \
                             wait h`). A closure-valued async result can only be used \
                             directly (call it, or pass it onward) in the SAME expression \
                             — it cannot be bound to a name. Restructure the block to \
                             consume `wait h`'s result without binding it"
                        )))
                    }
                    Err(e) => Err(e),
                }
            }
            Some("AsyncCancelWith") => {
                let tid = green_int_field(request, 0, table);
                if let Some(entry) = threads.get_mut(&tid) {
                    if matches!(entry.state, GreenThreadState::Running) {
                        let realm = entry.realm;
                        entry.state = GreenThreadState::Cancelled;
                        self.with_session_maybe_retrying(host, sid, |s| {
                            s.close_realm(realm);
                        })
                        .await
                        .map_err(|e| DriverError::Session(e.to_string()))?;
                        // A cancel is a terminal-state transition exactly like
                        // a settle — `waitEvent` must fire for either, so it
                        // shares the same publish (see the `AsyncDoneWith` arm
                        // above).
                        if let Some(h) = self.handlers.lock().event.as_mut() {
                            h.registry_mut().publish_async_done(tid);
                        }
                        self.wake_green_waiters(host, tid, table, sid, waiters, ready)
                            .await?;
                    }
                    // Idempotent: a terminal thread's cancel is a no-op.
                }
                let unit = ()
                    .to_value(table)
                    .map_err(|e| DriverError::Session(format!("AsyncCancelWith () bridge: {e}")))?;
                self.deliver_green_resume(
                    host,
                    sid,
                    &delivery,
                    chain,
                    hole,
                    GreenAnswer::Value(unit),
                    ready,
                    "AsyncCancelWith",
                )
                .await?;
                Ok(GreenHoleServiced::Proceed(false))
            }
            other => Err(DriverError::Session(format!(
                "outer loop suspended on an unrecognized Green constructor ({other:?})"
            ))),
        }
    }

    /// Wake every waiter parked (via `AsyncJoinAnyWith`) on `tid` — resume
    /// each with `tid`'s own id (the winner) and push the result onto
    /// `ready`. Shared by `AsyncDoneWith` (a settle) and `AsyncCancelWith` (a
    /// cancellation) servicing.
    async fn wake_green_waiters(
        &self,
        host: Option<NodeId>,
        tid: i64,
        table: &DataConTable,
        sid: tidepool_repr::SessionId,
        waiters: &mut HashMap<i64, Vec<(GreenChain, String)>>,
        ready: &mut VecDeque<GreenReady>,
    ) -> Result<(), DriverError> {
        let Some(parked) = waiters.remove(&tid) else {
            return Ok(());
        };
        let tid_value = tid
            .to_value(table)
            .map_err(|e| DriverError::Session(format!("green wake tid box: {e}")))?;
        for (wchain, whole) in parked {
            let next = self
                .with_session_maybe_retrying(host, sid, |s| {
                    s.resume(ResidentHole::plain(whole.clone()), tid_value.clone())
                })
                .await
                .map_err(|e| DriverError::Session(e.to_string()))?
                .map_err(|e| DriverError::Session(format!("green wake resume failed: {e}")))?;
            ready.push_back(GreenReady {
                chain: wchain,
                outcome: next,
            });
        }
        Ok(())
    }

    /// Service one `runLLMTurn @A` suspension (`site`/`ty` from
    /// [`crate::engine::SuspensionRouting::RunLLMTurn`], `prompt` the hole's
    /// human-facing text) against the CURRENT loop's SINGLE render-seeded
    /// answerer session (`self.answerer`): push the hole card as a User
    /// turn onto that persistent node — so hole #2 sees hole #1's exchange
    /// (the accumulating context window) — then drive it as a bounded
    /// multi-turn interaction to `finalize` (the effect that terminates the
    /// Agent turn loop rather than resuming it). The finalized value feeds
    /// straight into the OUTER session's `resume` to answer `loop`'s parked
    /// continuation.
    ///
    /// Bounded: each non-finalize model round counts against a per-hole
    /// budget — at [`TYPED_REQUEST_AGENT_NUDGE_ROUNDS`] the answerer is nudged to
    /// finalize, at [`TYPED_REQUEST_AGENT_MAX_ROUNDS`] the hole hard-fails — and
    /// against the per-loop [`LOOP_INFERENCE_CALL_CAP`] total.
    pub async fn service_typed_request_suspension(
        &mut self,
        site: u32,
        ty: Option<&str>,
        modules: &[String],
        prompt: &str,
        table: &DataConTable,
    ) -> Result<FinalAnswer, DriverError> {
        self.lifecycle = SelfHarnessState::SuspendedOnHole;
        self.emit(Event::RunLLMTurnHole {
            site,
            ty: ty.map(String::from),
            prompt: prompt.to_string(),
        });

        let lease = self.answerer.ok_or_else(|| {
            DriverError::Session(
                "service_typed_request_suspension called with no per-loop answerer (run_loop_fragment \
                 must create it first)"
                    .into(),
            )
        })?;
        // This hole finishes by taking the finalized answer and keeping the
        // node open for the NEXT hole — only a `ReusableLoop` lease may do
        // that (see `AgentSessionMode::require_reusable`'s doc).
        let node = lease.require_reusable()?;

        // Declare THIS hole's answer contract on the (reused) answerer node
        // before it takes a turn: the type pins `finalize`, and the harness's
        // types module puts that type in scope. Set per hole, because
        // consecutive holes in one loop can want different types.
        self.agent
            .set_answer_contract(node, self.answer_contract(ty, modules));

        // Push the hole card onto the EXISTING answerer node, accumulating
        // context rather than spawning a fresh one. The SCOPED answerer card
        // (`[AskUser, Finalize]`) names `finalize @T`, NOT the generic
        // `resume expr` (which does not compile against this stack).
        let child_prompt = engine::finalize_typed_request_prompt(
            "The loop",
            prompt,
            ty,
            modules,
            Some(table),
            &self.agent.finalize_typed_request_prompt_effect_row(),
        );
        self.agent.push_user_turn(node, &child_prompt)?;
        self.emit(Event::TurnStart { node });

        // An in-context window has NO branch position and no siblings — its
        // failure IS this turn's failure, which is why `runLLMTurn @T` keeps
        // a bare answer (PRD 21 decision 6's asymmetry, stated at the verb
        // declaration). So a typed exit from the shared round loop collapses
        // back into a hard failure HERE, unchanged from before the exit
        // plumbing existed.
        let fork_subtree = std::sync::atomic::AtomicU32::new(0);
        let outcome = match self
            .drive_agent_session_to_finalize(
                node,
                ty,
                site,
                0,
                &fork_subtree,
                AgentSessionExitPolicy::Interactive,
            )
            .await?
        {
            Ok(o) => o,
            Err(exit) => {
                return Err(DriverError::Session(format!(
                    "runLLMTurn answerer node {node:?}: {exit}"
                )))
            }
        };
        self.emit(Event::TurnEnd { node });

        let is_finalize = matches!(
            &outcome,
            TurnOutcome::Suspended { classified, .. }
                if matches!(classified.routing, SuspensionRouting::Finalize { .. })
        );
        if !is_finalize {
            return Err(DriverError::Session(format!(
                "runLLMTurn answerer node {node:?} did not suspend on finalize (got {})",
                turn_outcome_tag(&outcome)
            )));
        }

        // Take the finalized answer AND keep the node live (consume the
        // finalize hole, Suspended→Running) so the NEXT hole can push onto the
        // same accumulating session — not `take_finalized_value`, which cancels.
        // A CLOSURE payload is taken as a HANDLE (pillar B: it never bridges,
        // it is delivered verbatim into the loop's parked continuation on the
        // shared heap); data keeps the bridged-value path.
        let answer = if self.agent.finalize_is_closure(node) {
            let handle =
                retry_on_turn_in_flight(|| self.agent.take_finalized_handle_keep_open(node))
                    .await?;
            self.emit(Event::Finalize {
                node,
                value: "\"<closure>\"".to_string(),
            });
            FinalAnswer::Handle(handle)
        } else {
            let (value, rendered) = self.agent.take_finalized_value_keep_open(node)?;
            self.emit(Event::Finalize {
                node,
                value: rendered,
            });
            FinalAnswer::Value(value)
        };
        // The answerer node is REUSED across the loop's holes, so
        // `node_usage` returns the node's CUMULATIVE context size. The
        // MID-LOOP compaction check (`maybe_compact_answerer`) reads it BETWEEN
        // holes, once per hole, right after this returns — never summed per-hole
        // (that would double-count the reused node's running total).
        self.lifecycle = SelfHarnessState::RunningLoop;
        Ok(answer)
    }

    /// Service a `runLLMTurnFork @T`/`runLLMTurnFanout @T` suspension raised
    /// DIRECTLY by the AUTHORED outer loop (PRD 20 S1-L4, "concurrent
    /// cognition windows") — `fan: Some(_)` for a fanout (`prompts` one per
    /// child, answered as `[T]`), `fan: None` for a single fork (answered as
    /// bare `T`, `single_prompt` the one task text). Unlike this driver's
    /// other fork-servicing path ([`Self::drain_answerer_fork`], which drives
    /// each child sequentially through the recursive pump), every child here
    /// gets its own freshly-minted answerer realm on the SHARED outer
    /// machine and is driven CONCURRENTLY, up to [`Self::concurrency_cap`]
    /// at once ([`Self::drive_fanout_child`]/[`buffer_unordered`]): only
    /// machine occupancy serializes a child's actual compile+run, everything
    /// else (assembling its prompt, awaiting the provider) overlaps freely.
    /// Completion order is never observable — results are re-sorted back to
    /// DECLARATION order before assembly.
    ///
    /// # The child-attributable / mechanism line (PRD 21 locked decision 6)
    ///
    /// This is the ONE place the two are separated, and the separation is the
    /// whole point of the verbs' `Either` shape:
    ///
    /// - A failure ATTRIBUTABLE TO ONE CHILD'S WINDOW — its rounds ran out, it
    ///   ended on something that is not an answer, its own provider call
    ///   failed — comes back from [`Self::drive_fanout_child`] as
    ///   `Ok(Err(exit))` and is folded as `Left exit` AT THAT CHILD'S BRANCH
    ///   POSITION. Its siblings' answers are unaffected: the whole reason
    ///   decision 6 exists is that an exception here erases results that were
    ///   already produced.
    /// - A failure of the MECHANISM — the fan cardinality check below, the
    ///   `Either`/list assembly against the table, session bookkeeping, the
    ///   per-loop inference-call runaway cap — still hard-fails the turn via
    ///   `?`. Laundering a broken mechanism into "the model failed" would put
    ///   a false receipt in front of the operator, which is precisely what
    ///   this codebase refuses.
    #[allow(clippy::too_many_arguments)]
    async fn service_outer_fanout(
        &mut self,
        site: u32,
        ty: Option<&str>,
        modules: &[String],
        fan: Option<FanBadge>,
        single_prompt: &str,
        prompts: &[String],
        table: &DataConTable,
    ) -> Result<Value, DriverError> {
        self.lifecycle = SelfHarnessState::SuspendedOnHole;

        let is_fanout = fan.is_some();
        // A fanout site's recorded type is the LIST type (`[T]`); a plain
        // fork's is already the element type.
        let element_ty = if is_fanout {
            ty.and_then(engine::strip_list_type)
        } else {
            ty
        };
        // Single-vs-fanout normalization + cardinality integrity, via the
        // ONE home (`engine::fork_briefs`).
        let prompts: Vec<&str> = engine::fork_briefs(&fan, prompts, single_prompt)
            .map_err(|e| DriverError::Session(e.to_string()))?;

        for prompt in &prompts {
            self.emit(Event::RunLLMTurnHole {
                site,
                ty: element_ty.map(String::from),
                prompt: (*prompt).to_string(),
            });
        }

        let sid = self.outer_sid()?;
        let cap = self.concurrency_cap;
        // A shared borrow of `self` — every concurrent child needs only
        // `&self`-reachable state (the `Arc`-shared `agent`/`gate`, the
        // atomic counters, the plain round-cap config); none of them
        // outlives this `.await`, so no `Arc<Self>`/`tokio::spawn` is
        // needed (see `drive_fanout_child`'s doc for why `tokio::spawn`
        // itself doesn't fit here). `drive_concurrent` owns the
        // ordering/concurrency shell (buffer_unordered + re-sort to
        // DECLARATION order, since completion order is nondeterministic and
        // must never be observable in the resumed answer).
        let this = &*self;
        #[allow(clippy::type_complexity)]
        let results: Vec<(usize, Result<Result<Value, InvocationExit>, DriverError>)> =
            drive_concurrent(cap, prompts.len(), |idx| {
                let prompt = prompts[idx];
                async move {
                    this.drive_fanout_child(sid, site, idx, prompt, element_ty, modules, table)
                        .await
                }
            })
            .await;

        self.lifecycle = SelfHarnessState::RunningLoop;

        // Per-child assembly. The `?` on the OUTER `Result` is the mechanism
        // line: only a mechanism failure reaches it. The INNER `Result` is the
        // child's own outcome and becomes `Right`/`Left` at its position —
        // `engine::build_child_answer_value` follows `build_list_value`'s
        // loud-failure discipline (a `Left`/`Right`/`Exit*` constructor absent
        // from the turn's table is itself a mechanism failure, never a
        // defaulted value).
        let mut answers = Vec::with_capacity(results.len());
        for (idx, r) in results {
            let outcome = r?;
            if let Err(exit) = &outcome {
                tracing::warn!(
                    child = idx,
                    exit = %exit,
                    "fanout child exited without an answer — folding it as data at its \
                     branch position; siblings are unaffected"
                );
            }
            answers.push(
                engine::build_child_answer_value(outcome, table)
                    .map_err(|e| DriverError::Session(e.to_string()))?,
            );
        }

        if is_fanout {
            engine::build_list_value(answers, table)
                .map_err(|e| DriverError::Session(e.to_string()))
        } else {
            answers
                .into_iter()
                .next()
                .ok_or_else(|| DriverError::Session("outer fork produced no answer".into()))
        }
    }

    /// Drive ONE fanout/fork child to `finalize`, from scratch: mint a fresh
    /// answerer node ATTACHED to the shared outer session as its OWN realm
    /// (the "freshly-minted answerer realm" per window S1-L4 asks for —
    /// distinct from [`Self::answerer`], the single node the REUSED
    /// single-hole path drives), drive it through the ONE round loop
    /// ([`Self::drive_agent_session_to_finalize`], under
    /// [`AgentSessionExitPolicy::FinalizeOnly`] — a concurrent child supports
    /// `finalize` only; explore/define rounds and compile-error correction
    /// work exactly like the interactive path, but a nested
    /// `askUser`/`note`/`fork` suspension folds straight to
    /// `InvocationExit::NotFinalized` instead of being serviced — v1 scope,
    /// no operator-gate serialization or fork bookkeeping across siblings
    /// racing the same machine), and retire the node either way (realm
    /// scope-exit, never session removal — same discipline
    /// [`Self::retire_typed_request_agent`] uses for the reused answerer).
    ///
    /// `&self`, not `&mut self`: [`Self::service_outer_fanout`] runs up to
    /// [`Self::concurrency_cap`] of these concurrently via
    /// `futures_util::stream::buffer_unordered`, all borrowing the SAME
    /// `&SelfHarnessDriver` for the duration of one `.await` — genuine
    /// `tokio::spawn` tasks would need `'static` ownership of driver state
    /// this borrow-based shape avoids entirely. Every `Harness` call this
    /// makes is `&self` too (`agent: Arc<Harness>`); the two pieces of
    /// driver state a round loop mutates (`loop_inference_calls`,
    /// `iteration_realm`) are atomics for exactly this reason.
    ///
    /// The nesting of the return type is the contract: the OUTER `Result` is
    /// the MECHANISM (a hard failure of this driver, which fails the turn),
    /// the INNER one is THIS CHILD'S WINDOW (`Err(exit)` folds as `Left` at
    /// its branch position). See [`Self::service_outer_fanout`]'s doc for the
    /// line between them. The node is retired either way — a child that exits
    /// without an answer still releases its realm and scope.
    #[allow(clippy::too_many_arguments)]
    async fn drive_fanout_child(
        &self,
        sid: tidepool_repr::SessionId,
        site: u32,
        idx: usize,
        prompt: &str,
        element_ty: Option<&str>,
        modules: &[String],
        table: &DataConTable,
    ) -> Result<Result<Value, InvocationExit>, DriverError> {
        let node = self.agent.create_root_framed(
            &format!("fanout answerer {idx}"),
            "",
            self.answerer_framing.clone(),
        )?;
        self.agent.force_attached(node, Actor::Operator, sid)?;
        self.agent.set_node_realm(node, self.mint_realm());
        // Sibling fanout children share this ONE session's machine — a
        // checkout race against another child's turn is expected, benign
        // contention (not a real conflict), so this node's checkouts WAIT
        // instead of failing fast. See `Harness::checkout_run_retrying`'s
        // doc for why `drive_turn` itself is never retried as a whole.
        self.agent.set_retry_checkout_on_contention(node, true);

        let result = self
            .finalize_fanout_child(node, site, idx, prompt, element_ty, modules, table)
            .await;
        let _ = self.agent.terminate_node(node, "fanout child retired");
        result
    }

    /// Seed `node` with this fanout/fork child's hole card, drive it through
    /// the shared pump ([`Self::drive_agent_session_to_finalize`], `FinalizeOnly`
    /// policy), and extract the finalized value.
    /// Split out of [`Self::drive_fanout_child`] only so that function's
    /// `terminate_node` always runs, on every return path here.
    ///
    /// # What is a typed exit here and what is not
    ///
    /// `Ok(Err(exit))` — THIS WINDOW ended without an answer, and nothing
    /// about the driver is broken: round exhaustion
    /// ([`InvocationExit::RoundsExhausted`]), a suspension on a
    /// non-`finalize` hole ([`InvocationExit::NotFinalized`], raised inside
    /// the shared pump under `FinalizeOnly`), or the window's own provider
    /// call failing ([`InvocationExit::RuntimeFailure`]).
    ///
    /// `Err(..)` — the MECHANISM is broken, and calling that "the model
    /// failed" would be a false receipt: the per-loop inference-call cap (a
    /// runaway HARNESS, not a runaway window — and it is shared, so the next
    /// child would trip it too); a finalized CLOSURE (the window DID answer,
    /// and this driver cannot carry the answer it gave — v1 scope, the gap is
    /// ours); or session/registry faults, and any `Harness` error that is not
    /// the window's own compile (handled in-loop) or provider call.
    #[allow(clippy::too_many_arguments)]
    async fn finalize_fanout_child(
        &self,
        node: NodeId,
        site: u32,
        idx: usize,
        prompt: &str,
        element_ty: Option<&str>,
        modules: &[String],
        table: &DataConTable,
    ) -> Result<Result<Value, InvocationExit>, DriverError> {
        self.agent
            .set_answer_contract(node, self.answer_contract(element_ty, modules));
        let child_prompt = engine::finalize_typed_request_prompt(
            "The loop",
            prompt,
            element_ty,
            modules,
            Some(table),
            &self.agent.finalize_typed_request_prompt_effect_row(),
        );
        self.agent.push_user_turn(node, &child_prompt)?;
        self.emit(Event::TurnStart { node });

        let outcome = self
            .drive_agent_session_to_finalize(
                node,
                element_ty,
                site,
                0,
                &std::sync::atomic::AtomicU32::new(0),
                AgentSessionExitPolicy::FinalizeOnly { idx },
            )
            .await?;
        self.emit(Event::TurnEnd { node });

        // `FinalizeOnly` only ever returns `Ok(_)` with a `Finalize`
        // suspension (any other suspension is folded to `Err(NotFinalized)`
        // inside the shared pump), so this is the finalize extraction only.
        if let Some(exit) = outcome.err() {
            return Ok(Err(exit));
        }

        if self.agent.finalize_is_closure(node) {
            // NOT a typed exit: the window DID answer, and it is this driver
            // that cannot carry a closure across the fanout join (v1 scope).
            // Reporting our own gap as the child's failure would be a false
            // receipt — see `service_outer_fanout`'s doc.
            return Err(DriverError::Session(format!(
                "fanout child {idx} finalized a closure — a concurrent fanout/fork \
                 answer must be plain data in this driver (v1 scope)"
            )));
        }
        let (value, rendered) =
            retry_on_turn_in_flight(|| self.agent.take_finalized_value_keep_open(node)).await?;
        self.emit(Event::Finalize {
            node,
            value: rendered,
        });
        Ok(Ok(value))
    }

    /// Drive `node` (an answerer, already seeded with this hole's card)
    /// turn-by-turn until it suspends on `finalize`, applying the runaway
    /// caps: count each non-finalize model round; at [`TYPED_REQUEST_AGENT_NUDGE_ROUNDS`]
    /// push a one-time "finalize now" nudge; at [`TYPED_REQUEST_AGENT_MAX_ROUNDS`]
    /// hard-fail the hole; and abort the whole loop if the per-loop
    /// [`LOOP_INFERENCE_CALL_CAP`] is hit. A `Completed` (non-finalize) or
    /// `NoBlock` turn is treated as a wasted round — re-prompted toward
    /// `finalize` — rather than accepted, since the answerer's contract is to
    /// resolve the hole via `finalize`, not return a plain value.
    ///
    /// THE ONE PUMP (sol cross-family review finding 4): this is the ONLY
    /// model-session round loop in this driver — the reused single-hole
    /// answerer, a sequential branch/branch-fanout child, a recursive fork
    /// child, and a concurrent `runLLMTurnFork`/`runLLMTurnFanout` child
    /// ([`Self::drive_fanout_child`]) all drive through here. What genuinely
    /// differs between them is not the round loop — it is which suspensions
    /// get SERVICED once the node parks, captured by [`AgentSessionExitPolicy`]:
    /// [`AgentSessionExitPolicy::Interactive`] runs the full dispatcher
    /// (`askUser`/`note`/`fork`/green threads); [`AgentSessionExitPolicy::FinalizeOnly`]
    /// is what a concurrent fanout/fork child gets (v1 scope: no operator
    /// gate serialization or fork bookkeeping across siblings racing the same
    /// machine) — any non-finalize suspension folds straight to
    /// `InvocationExit::NotFinalized` DATA at that child's own position
    /// instead of being serviced.
    ///
    /// Each round `.await`s [`Harness::drive_turn`] directly — the resident
    /// JIT run it performs is CPU-blocking and sits inside this `async fn`
    /// unchanged; it already blocked a tokio worker before this method was
    /// `async` (called straight from async test bodies and `#[tokio::main]`
    /// with no bridge), so nothing about that changes here. It is not
    /// `spawn_blocking`'d: the resident session is not `Send`-shaped for
    /// that, and doing so is a separate piece of work.
    /// The return NESTING is the child-attributable/mechanism line: `Ok(Err(exit))`
    /// means THIS WINDOW ended without an answer (round exhaustion, its own
    /// provider call failing, or — under `FinalizeOnly` — a non-finalize
    /// suspension), `Err(..)` means the mechanism is broken (the per-loop
    /// inference-call cap, session faults). Whether an exit is DATA or fatal
    /// is the CALLER's to decide, because it depends on whether the window
    /// sits at a branch position: [`Self::drive_fanout_child`] folds it as
    /// `Left` at that branch, while [`Self::service_typed_request_suspension`] —
    /// answering IN CONTEXT on the outer turn's own continuation, with no
    /// siblings and no position — still hard-fails, exactly as before;
    /// [`Self::drive_fork_child_agent_session`] — a recursive fork/async-fork
    /// child, also with no branch position — turns it into a plain-language
    /// corrective instead of either of those (operator decision, 2026-08-24):
    /// the child's own node still retires and reports `node_failed`, but the
    /// caller aborts only the block that was consuming this child, not the
    /// parent's whole turn.
    async fn drive_agent_session_to_finalize(
        &self,
        node: NodeId,
        ty: Option<&str>,
        site: u32,
        fork_depth: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
        policy: AgentSessionExitPolicy,
    ) -> Result<Result<TurnOutcome, InvocationExit>, DriverError> {
        let ty_label = ty.unwrap_or("A");
        let max_rounds = self.answerer_max_rounds;
        let nudge_rounds = self.answerer_nudge_rounds;
        // The glide: at `max_rounds`, ONE explicit ultimatum ("your next
        // reply must be the minimal honest finalize") and two grace rounds
        // before the hard fail — a window that wedges on an expressible
        // answer (the live 2026-08-14 incident burned 32 rounds on a
        // spelling it was never told) gets a direct instruction first, and
        // only a window that cannot even comply takes the loop down.
        let hard_rounds = max_rounds.saturating_add(2);
        let mut rounds: u32 = 0;
        let mut nudged = false;
        let mut ultimatum = false;
        // Session-scoped (all rounds): total fork children, direct + green.
        // DEPTH CONTAINMENT (fork-subsumes-split step 1½): a CHILD session's
        // fork budget is ZERO — the pump path removed the old fork-free
        // child row's structural depth-one bound, and un-stripping is unsafe
        // until step 2's subtree depth/total-node budgets are checked
        // atomically at spawn (seam map §6). A zero cap rides the existing
        // loud-refusal machinery; `fork_budget_refusal` teaches the boundary.
        let mut fork_budget = ForkBudget {
            cap: if fork_depth >= self.max_fork_depth {
                0
            } else {
                self.fork_budget_per_window
            },
            spent: 0,
        };
        // The subject named in this round's diagnostics — the reused
        // single-hole answerer under `Interactive`, or `fanout child {idx}`
        // under `FinalizeOnly` (finding 4: these messages used to live in
        // two separately-worded copies of this same loop).
        let subject = match policy {
            AgentSessionExitPolicy::Interactive => "runLLMTurn answerer".to_string(),
            AgentSessionExitPolicy::FinalizeOnly { idx } => format!("fanout child {idx}"),
        };
        'round: loop {
            let cap = self.loop_inference_call_cap;
            if self.loop_inference_calls.load(Ordering::SeqCst) >= cap {
                let ctx = match policy {
                    AgentSessionExitPolicy::Interactive => String::new(),
                    AgentSessionExitPolicy::FinalizeOnly { idx } => {
                        format!(" while servicing concurrent fanout child {idx}")
                    }
                };
                return Err(DriverError::Session(format!(
                    "per-loop inference-call cap ({cap}) reached{ctx} — \
                     hard-stopping the loop (a runaway harness)"
                )));
            }
            if rounds >= hard_rounds {
                // ROUND EXHAUSTION — this window's own budget, spent. Data
                // for a caller that has a branch position to fold it at;
                // `service_typed_request_suspension` still turns it into a hard failure.
                return Ok(Err(InvocationExit::RoundsExhausted(format!(
                    "{subject} exceeded {hard_rounds} rounds (cap {max_rounds} \
                     + ultimatum grace) without finalizing"
                ))));
            }
            if rounds >= max_rounds && !ultimatum {
                self.agent.push_user_turn(
                    node,
                    &format!(
                        "ROUND CAP REACHED. Your NEXT reply must be a single ```haskell \
                         block that ONLY finalizes — the minimal honest answer of type \
                         `{}` (a no-change/empty answer is acceptable and preferred over \
                         anything elaborate). Nothing else will be accepted.",
                        display_ty(ty_label)
                    ),
                )?;
                ultimatum = true;
            }
            if rounds == nudge_rounds && !nudged {
                self.agent.push_user_turn(
                    node,
                    &format!(
                        "Reminder: you have used {rounds} of {max_rounds} model rounds on \
                         this request. Budget the remainder — finalize as soon as another \
                         round would not improve the answer, and no later than round \
                         {max_rounds}: evaluate `finalize @{} value`.",
                        display_ty(ty_label)
                    ),
                )?;
                nudged = true;
            }

            self.loop_inference_calls.fetch_add(1, Ordering::SeqCst);
            rounds += 1;
            let outcome = self.agent.drive_turn(node).await;
            // A retry loop that burns rounds must be visible while it is
            // happening, not reconstructable afterwards — one `AnswererRound` per round that reached a
            // compile attempt, `error: None` on success regardless of what the
            // block went on to do, PLUS a `NoBlock` reply (no compile
            // attempt at all, but still a round the operator watched pass).
            // `round_progress` mirrors the same three-way split onto the
            // operator gate so it is visible live, not only in the durable
            // log.
            match &outcome {
                Ok(TurnOutcome::Suspended { .. } | TurnOutcome::Completed { .. }) => {
                    self.emit(Event::AnswererRound {
                        node,
                        site,
                        round: rounds,
                        error: None,
                    });
                    let gate = self.resolve_gate(&FormSource::Answerer { node });
                    gate.round_progress(rounds, None);
                    // Show the operator what the answerer actually ran —
                    // once per COMPILED round (a failed compile has no
                    // executed source to show; `post_turn_source` is a
                    // default-no-op on headless gates). Routed per-node like
                    // asks/notes: a labeled branch child's turns belong on
                    // its own section, not the default one.
                    if let Some(src) = self.agent.last_turn_source(node) {
                        gate.post_turn_source(&src);
                    }
                }
                Err(HarnessError::Compile(msg)) => {
                    self.emit(Event::AnswererRound {
                        node,
                        site,
                        round: rounds,
                        error: Some(msg.clone()),
                    });
                    self.resolve_gate(&FormSource::Answerer { node })
                        .round_progress(rounds, Some(msg.as_str()));
                }
                Ok(TurnOutcome::NoBlock { .. }) => {
                    // Previously silent: no `Event` and no gate call at all —
                    // the operator saw nothing pass while the answerer burned
                    // an empty-reply round. `NoBlock` never reaches a
                    // compile, so it is not a round for the corrective-retry
                    // fold's purposes, but it IS a round the operator should
                    // see go by.
                    self.emit(Event::AnswererRound {
                        node,
                        site,
                        round: rounds,
                        error: Some(NO_HASKELL_BLOCK_ROUND_ERROR.to_string()),
                    });
                    self.resolve_gate(&FormSource::Answerer { node })
                        .round_progress(rounds, Some(NO_HASKELL_BLOCK_ROUND_ERROR));
                }
                Err(_) => {}
            }
            match outcome {
                Ok(out @ TurnOutcome::Suspended { .. }) => {
                    // The round's SERVICING DISPATCHER. A Finalize suspension
                    // is the answer. Everything else is serviced and the
                    // fresh suspension re-dispatched, so the four families
                    // COMPOSE in any order within one block: operator forms
                    // (`service_askuser_hole`), fork delegation
                    // (`drain_answerer_fork` — REUSED, not reimplemented),
                    // green threads (`service_green_round` — the
                    // answerer-plane scheduler behind `async (fork @T …)`),
                    // and the mechanical resumes (`note`/`getStateJson`/
                    // `delegate`, via `drain_note_holes`). Any OTHER
                    // suspension is a hard error: the scoped answerer stack
                    // (`[AskUser, Fork, ReadState, Green, Finalize]`) can
                    // reach nothing else, and this driver has no operator
                    // for it.
                    //
                    // `green` is ROUND-scoped (one compile = one table for
                    // every chain); at the round's end it is SWEPT — a
                    // thread still running when the block finalizes or
                    // completes is dropped, and the corrective prompt names
                    // the count when anything was.
                    let TurnOutcome::Suspended { hole, classified } = out else {
                        unreachable!("matched TurnOutcome::Suspended above");
                    };
                    // `FinalizeOnly` (a concurrent fanout/fork child, v1
                    // scope) never reaches the dispatcher below: `finalize`
                    // is the answer, and everything else — including a
                    // mechanical `note`/`getStateJson` — folds straight to
                    // `NotFinalized` DATA at this child's own branch
                    // position, since there is no per-child operator-gate
                    // serialization or fork bookkeeping to service it with.
                    if let AgentSessionExitPolicy::FinalizeOnly { idx } = policy {
                        return match &classified.routing {
                            SuspensionRouting::Finalize { .. } => {
                                Ok(Ok(TurnOutcome::Suspended { hole, classified }))
                            }
                            other => Ok(Err(InvocationExit::NotFinalized(format!(
                                "fanout child {idx} suspended on a non-finalize hole \
                                 ({other:?}) — a concurrent fanout/fork child cannot \
                                 present an operator form, note, or nested fork in this \
                                 driver (v1 scope)"
                            )))),
                        };
                    }
                    let mut green: Option<ModelRoundGreenThreadScheduler> = None;
                    let mut current = Some((hole, classified));
                    let finalized: Option<TurnOutcome> = loop {
                        // Mechanical holes first (note/getStateJson/delegate)
                        // — the block may read `note "..." >> choose [...]`,
                        // so the current hole is routinely `Note`, not the
                        // thing that follows it.
                        let Some((hole, classified)) = current.take() else {
                            break None;
                        };
                        let Some((hole, classified)) = self
                            .sweep_green_on_err(
                                node,
                                &mut green,
                                self.drain_note_holes(node, hole, classified).await,
                            )
                            .await?
                        else {
                            break None;
                        };
                        match &classified.routing {
                            SuspensionRouting::Finalize { .. } => {
                                break Some(TurnOutcome::Suspended { hole, classified });
                            }
                            SuspensionRouting::AskUser { shape } => {
                                match self
                                    .sweep_green_on_err(
                                        node,
                                        &mut green,
                                        self.service_askuser_hole(node, shape).await,
                                    )
                                    .await?
                                {
                                    Some(TurnOutcome::Suspended {
                                        hole: h,
                                        classified: c,
                                    }) => current = Some((h, c)),
                                    Some(_) | None => break None,
                                }
                            }
                            SuspensionRouting::Fork { .. } => {
                                match self
                                    .sweep_green_on_err(
                                        node,
                                        &mut green,
                                        self.drain_answerer_fork(
                                            node,
                                            ty_label,
                                            &mut fork_budget,
                                            fork_depth,
                                            fork_subtree,
                                        )
                                        .await,
                                    )
                                    .await?
                                {
                                    Some(TurnOutcome::Suspended {
                                        hole: h,
                                        classified: c,
                                    }) => current = Some((h, c)),
                                    // Completed without finalizing (or the
                                    // budget refused a fork) —
                                    // `drain_answerer_fork` already returned
                                    // the node to Running and pushed its
                                    // corrective.
                                    Some(_) | None => {
                                        if let Some(g) = green.as_mut() {
                                            let dropped = self.sweep_green_round(node, g).await;
                                            if dropped > 0 {
                                                self.agent.push_user_turn(
                                                    node,
                                                    &dropped_threads_warning(dropped),
                                                )?;
                                            }
                                        }
                                        continue 'round;
                                    }
                                }
                            }
                            SuspensionRouting::Green => {
                                // The `g` borrow must end before
                                // `sweep_green_on_err` can reborrow `green`
                                // mutably to sweep it on an `Err`.
                                let green_result = {
                                    let g = green
                                        .get_or_insert_with(ModelRoundGreenThreadScheduler::new);
                                    self.service_green_round(
                                        node,
                                        g,
                                        &mut fork_budget,
                                        fork_depth,
                                        fork_subtree,
                                        ty_label,
                                    )
                                    .await
                                };
                                match self
                                    .sweep_green_on_err(node, &mut green, green_result)
                                    .await?
                                {
                                    GreenRoundExit::NodeParked => {
                                        current = self
                                            .agent
                                            .pending_suspension_full(node)
                                            .map(|(h, c, _)| (h.0, c));
                                    }
                                    GreenRoundExit::NodeDone => break None,
                                    // A thread's fork was refused: abort the
                                    // block (the node is parked on its own
                                    // green join — refuse that hole), sweep
                                    // the round's threads, and push the
                                    // budget corrective. The WINDOW survives.
                                    GreenRoundExit::ForkBudgetRefused { msg } => {
                                        let dropped = match green.as_mut() {
                                            Some(g) => self.sweep_green_round(node, g).await,
                                            None => 0,
                                        };
                                        retry_on_turn_in_flight(|| {
                                            self.agent.refuse_pending_suspension(node, msg.clone())
                                        })
                                        .await?;
                                        let warn = if dropped > 0 {
                                            format!("\n\n{}", dropped_threads_warning(dropped))
                                        } else {
                                            String::new()
                                        };
                                        self.agent.push_user_turn(node, &format!("{msg}{warn}"))?;
                                        continue 'round;
                                    }
                                    // Async misuse: same loud-refusal shape
                                    // as the budget — the block dies, the
                                    // SESSION survives with a corrective.
                                    // One model slip must not end the run.
                                    GreenRoundExit::AsyncMisuse { msg } => {
                                        let dropped = match green.as_mut() {
                                            Some(g) => self.sweep_green_round(node, g).await,
                                            None => 0,
                                        };
                                        let corrective = format!(
                                            "Async misuse — the block was aborted; your \
                                             session continues and earlier rounds' \
                                             definitions/bindings persist. Problem: {msg}."
                                        );
                                        retry_on_turn_in_flight(|| {
                                            self.agent
                                                .refuse_pending_suspension(node, corrective.clone())
                                        })
                                        .await?;
                                        let warn = if dropped > 0 {
                                            format!("\n\n{}", dropped_threads_warning(dropped))
                                        } else {
                                            String::new()
                                        };
                                        self.agent
                                            .push_user_turn(node, &format!("{corrective}{warn}"))?;
                                        continue 'round;
                                    }
                                    // A thread's fork child ended in
                                    // `InvocationExit` rather than
                                    // finalizing: same loud-refusal shape —
                                    // the block dies (this operator
                                    // decision — see `GreenRoundExit::ForkChildFailed`'s
                                    // doc), the SESSION survives with a
                                    // corrective naming the child by its
                                    // path. The child's own node already
                                    // retired and reported `node_failed`
                                    // inside `drive_fork_child_agent_session`.
                                    GreenRoundExit::ForkChildFailed { msg } => {
                                        let dropped = match green.as_mut() {
                                            Some(g) => self.sweep_green_round(node, g).await,
                                            None => 0,
                                        };
                                        retry_on_turn_in_flight(|| {
                                            self.agent.refuse_pending_suspension(node, msg.clone())
                                        })
                                        .await?;
                                        let warn = if dropped > 0 {
                                            format!("\n\n{}", dropped_threads_warning(dropped))
                                        } else {
                                            String::new()
                                        };
                                        self.agent.push_user_turn(node, &format!("{msg}{warn}"))?;
                                        continue 'round;
                                    }
                                }
                            }
                            other => {
                                // A suspension this driver cannot service.
                                // Hard error rather than silently hanging.
                                if let Some(g) = green.as_mut() {
                                    self.sweep_green_round(node, g).await;
                                }
                                return Err(DriverError::Session(format!(
                                    "runLLMTurn answerer suspended on a hole this driver \
                                     has no operator for ({other:?})"
                                )));
                            }
                        }
                    };
                    let dropped = match green.as_mut() {
                        Some(g) => self.sweep_green_round(node, g).await,
                        None => 0,
                    };
                    if let Some(answer) = finalized {
                        // Finalize won; a still-running thread losing the
                        // race to it is the documented spawn-and-wait-in-one-
                        // block contract, swept silently above.
                        return Ok(Ok(answer));
                    }
                    // The chain resolved (the answerer's block completed)
                    // WITHOUT finalize — same corrective retry as a plain
                    // Completed turn below, and it must say the same thing:
                    // this is the EXPECTED batch-per-round idiom the suffix
                    // teaches (fork a wave, wait, end the round), not a
                    // failure to scold (companion dogfood, 2026-08-13's
                    // Completed-arm fix, mirrored here — see that arm's
                    // comment). Servicing resumes are NOT model rounds:
                    // `rounds` stays untouched, only this outer loop repeats.
                    self.agent.reopen_node(node)?;
                    let ty_disp = display_ty(ty_label);
                    let warn = if dropped > 0 {
                        format!("\n\n{}", dropped_threads_warning(dropped))
                    } else {
                        String::new()
                    };
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "Round complete — your session continues, and that round's \
                             definitions and bindings (including everything you waited \
                             on) persist. The request still awaits its answer: explore \
                             further, fork another batch, or evaluate \
                             `finalize @{ty_disp} value` when ready (that ends the \
                             session).{warn}"
                        ),
                    )?;
                    continue;
                }
                // A plain value: the block ran to completion WITHOUT
                // `finalize`, so the node is now `Done`. Reopen it
                // (`Done`→`Running`) before the corrective re-prompt, so the
                // same accumulating node keeps driving toward `finalize` (a
                // wasted round, already counted).
                Ok(TurnOutcome::Completed { rendered }) => {
                    // A completed non-finalize round is a VALID explore/define
                    // round, not a failure — the window is multi-round by
                    // design, and scolding here taught the model that only
                    // `finalize` is admitted (companion dogfood, 2026-08-13:
                    // it reported exactly that, accurately). Acknowledge, SHOW
                    // the block's value (GHCi parity — the wave-per-round
                    // idiom needs the model to SEE what it bound, see
                    // `rendered_result_snippet`), and keep the request
                    // standing.
                    self.agent.reopen_node(node)?;
                    let ty_disp = display_ty(ty_label);
                    let shown = rendered_result_snippet(&rendered);
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "Round complete — your session continues, and that round's \
                             definitions/bindings persist. The block evaluated to:\n\
                             {shown}\n\
                             The request still awaits its \
                             answer: when ready, evaluate `finalize @{ty_disp} value` \
                             (that ends the session)."
                        ),
                    )?;
                }
                // An empty turn (no haskell block): the node is still `Running`
                // (no block ran), so no reopen — just re-prompt.
                Ok(TurnOutcome::NoBlock { .. }) => {
                    let ty_disp = display_ty(ty_label);
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "Your reply had no ```haskell block, so nothing ran. Reply \
                             with ```haskell blocks — an explore/define round is fine, \
                             or `finalize @{ty_disp} value` when ready."
                        ),
                    )?;
                }
                // A compile error: feed it back so the answerer can correct,
                // same as the corrective-retry loop in `run_to_hole_or_done`.
                // A wrong-typed `finalize` now lands HERE rather than crossing
                // in-heap and case-trapping — that is what pinning `finalize`
                // to the hole's type buys.
                Err(HarnessError::Compile(msg)) => {
                    let hint = self
                        .types_in_scope_hint(node, ty_label, &msg)
                        .unwrap_or_default();
                    // Parenthesize a compound answer type in the prompt — an
                    // unparenthesized `finalize @State -> State` is itself
                    // ill-typed advice. And do NOT teach single-shot: the
                    // window stays multi-round; a fixed block may be another
                    // define/explore round, with `finalize` whenever ready
                    // (the companion learned "declarations are forbidden"
                    // from the old wording — dogfood, 2026-08-13). For a
                    // multi-block reply, `msg` already leads with the sequence
                    // context (which blocks ran/persist, where to resume —
                    // `engine::sequence_failure_context`).
                    let ty_disp = display_ty(ty_label);
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "A block did not compile — your session continues; everything \
                             that already ran persists. Reply with corrected ```haskell \
                             blocks. Another define/explore round is fine (top-level \
                             declarations are welcome and persist); when you are ready to \
                             answer, evaluate \
                             `finalize @{ty_disp} value`.\n\nGHC error:\n{msg}{hint}"
                        ),
                    )?;
                }
                // A provider fault is THIS SESSION's own runtime failure —
                // decision 6's "runtime failure" class, shared by every
                // policy: a branch-position caller (fork child, labeled
                // branch, `FinalizeOnly` fanout child) folds it as `Left` at
                // that position instead of one transient 5xx erasing every
                // sibling's finished answer. The in-context caller
                // (`service_typed_request_suspension`) still collapses it to a hard
                // failure, unchanged. Every OTHER `HarnessError` is
                // driver/session machinery and hard-fails the turn.
                Err(HarnessError::Engine(EngineError::Provider(pe))) => {
                    return Ok(Err(InvocationExit::RuntimeFailure(format!(
                        "{subject} provider call failed: {pe}"
                    ))));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Service a contiguous run of `askUser` suspensions on `node`, starting
    /// from the just-classified `shape`: block on the operator gate for a
    /// submission ([`OperatorGate::present_form`]),
    /// resume the answerer with it via [`Harness::answer_dialog`] (the same
    /// audited resume path a mechanical dialog answer uses — `answer_dialog`
    /// accepts `AskUser` alongside `Dialog`/`Ask`), and repeat while the
    /// resume keeps landing on ANOTHER `AskUser` suspension — `askUser`
    /// re-prompts by RECURSION on a decode failure (no `Either`; the retry is
    /// entirely Haskell-side), so a bad submission genuinely re-suspends on a
    /// fresh `AskUserWith`, not an error this driver sees.
    ///
    /// Bounded by [`ASKUSER_MAX_REPROMPTS`] CONSECUTIVE re-presentations,
    /// independent of the model-round caps (see that constant's doc): a form
    /// resume never calls the model, so it must not touch `rounds`/
    /// `loop_inference_calls` — but left totally uncapped, a non-interactive
    /// gate at EOF (the default [`StdinGate`], closed stdin) composes with
    /// `askUser`'s unbounded re-prompt recursion into a hot loop no existing
    /// cap catches.
    ///
    /// Returns `Ok(Some(outcome))` when the chain resolves to any
    /// non-form suspension (`Finalize`, a fork, a green `wait`, …) — built
    /// from [`Harness::pending_suspension_full`] read right after the resume,
    /// since `answer_dialog` itself returns no outcome — which the caller's
    /// dispatcher routes. Returns `Ok(None)` when a
    /// resume completes the node with NO pending hole (the answerer's block
    /// finished without ever calling `finalize`); the caller falls through to
    /// its existing completed-without-finalize corrective retry. `Err` on a
    /// resume failure or the reprompt cap being hit.
    async fn service_askuser_hole(
        &self,
        node: NodeId,
        shape: &FormShape,
    ) -> Result<Option<TurnOutcome>, DriverError> {
        let mut shape = shape.clone();
        let mut reprompts: u32 = 0;
        loop {
            let submission = self
                .present_askuser_form(&mut reprompts, FormSource::Answerer { node }, &shape)
                .await?;
            retry_on_turn_in_flight_async(|| self.agent.answer_dialog(node, submission.clone()))
                .await?;

            let Some((hole, classified, _table)) = self.agent.pending_suspension_full(node) else {
                // The resume completed the node with no further suspension.
                return Ok(None);
            };
            // A submission (or a `note` resume below) may land on a `note`
            // hole next — e.g. `askUser @T >>= \t -> note (explain t) >>
            // finalize @T t` — drain it purely via resumes before checking
            // Finalize/AskUser.
            let Some((hole, classified)) = self.drain_note_holes(node, hole.0, classified).await?
            else {
                return Ok(None);
            };
            if let SuspensionRouting::AskUser { shape: next_shape } = classified.routing {
                shape = next_shape;
                continue;
            }
            // Finalize, or any OTHER routing (a fork after the form, a `wait`
            // on an earlier-spawned thread): hand the outcome back — the
            // dispatcher in `drive_agent_session_to_finalize` routes it. Before
            // the answerer-plane green scheduler this arm hard-errored on
            // everything but Finalize/AskUser.
            return Ok(Some(TurnOutcome::Suspended { hole, classified }));
        }
    }

    /// Service a run of `askUser` suspensions the AUTHORED OUTER loop itself
    /// raised (distinct from [`Self::service_askuser_hole`], which handles a
    /// nested ANSWERER's form). Present `shape` via the operator gate
    /// ([`OperatorGate::present_form`]),
    /// convert the flat submission into the `Value` `askUserRaw :: Value -> M
    /// Value` returns ([`engine::json_answer_to_value`] against the outer
    /// compile's `table`), and resume the OUTER session — repeating while the
    /// resume lands on ANOTHER `AskUser` suspension, since `askUser` re-prompts
    /// by RECURSION on a decode failure (no `Either`; the retry is entirely
    /// Haskell-side, so a bad submission genuinely re-suspends on a fresh
    /// `AskUserWith`, not an error this driver sees).
    ///
    /// Returns the FIRST [`ResidentOutcome`] that is NOT another operator form
    /// — a `runLLMTurn` suspension (which [`Self::run_loop_fragment_inner`]'s
    /// main loop then services) or a completion — so the outer loop can
    /// interleave author-driven forms and model-answered holes freely.
    ///
    /// Bounded by [`ASKUSER_MAX_REPROMPTS`] CONSECUTIVE re-presentations, for
    /// the same reason [`Self::service_askuser_hole`] is: the default headless
    /// [`StdinGate`] returns an EMPTY submission on EOF rather than erroring, so
    /// a non-interactive gate composes with `askUser`'s unbounded Haskell-side
    /// re-prompt into a hot loop no model-round cap catches (a form resume is
    /// not a model round). The between-loops human gate bounds loop ITERATIONS,
    /// not re-prompts WITHIN one loop's `askUser` — this counter does.
    /// `routing` is the FIRST hole's already-classified routing — either
    /// `SuspensionRouting::AskUser` (a typed form) or `SuspensionRouting::Note`
    /// (display-only narration, e.g. `note "..." >> askUser @T ...` at the
    /// outer level): each iteration dispatches on whichever of the two the
    /// CURRENT hole is, so a chain freely interleaving `note` and `askUser`
    /// (in either order) drives to completion without a model round.
    /// Service ONE `Subagent` suspension raised by the AUTHORED outer loop:
    /// decode the ORIGINAL suspended request through the generated
    /// `SubagentReq: FromCore` (against the loop compile's own table — the
    /// args are bridged ADTs, never JSON-probed), dispatch it into the
    /// driver-owned [`tidepool_handlers::SubagentHandler`], and return the
    /// `Response::Complete` value the caller resumes the hole with — the
    /// IDENTICAL generated conversion path a dispatched effect takes, minus
    /// the dispatch (the outer row's handled prefix must stay empty on the
    /// shared machine; see [`outer_decls`]).
    ///
    /// Runs the actual dispatch under `tokio::task::block_in_place`
    /// (`CodexAgentBackend` owns its own runtime and `block_on`s it — the
    /// same discipline every `OperatorGate` call uses), so a lane-1 coupled
    /// spawn blocking this loop turn for the agent's whole cycle (~30–120s
    /// live, by design — `plans/companion-memory.md`) frees the tokio worker
    /// rather than parking it. The lock held for that call is
    /// [`Self::subagent`] alone, NOT [`Self::handlers`] — see that field's
    /// doc for why: a long subagent cycle must never starve an unrelated
    /// `Console`/`Worktree`/`RepoEvent`/`Exec`/`Journal` suspension serviced
    /// concurrently on another turn.
    ///
    /// `source` identifies who raised this delegation — the AUTHORED outer
    /// loop itself ([`FormSource::OuterLoop`]) or a labeled node's own
    /// `delegate` ([`FormSource::Answerer`]) — and is used ONLY to resolve
    /// which operator gate's timeline the delegation-lifecycle events
    /// ([`DelegationPhase`], via [`OperatorGate::delegation_progress`]) land
    /// on, via [`Self::resolve_gate`]; it never affects dispatch itself.
    /// Emits [`DelegationPhase::Started`] before the dispatch (so a spawn
    /// that never returns is still visible), then EXACTLY ONE of
    /// [`DelegationPhase::Settled`]/[`DelegationPhase::Failed`] — including
    /// the "no subagent handler configured" refusal, which used to be
    /// completely silent (no `Event`, no gate call, no `tracing` line).
    fn service_outer_subagent(
        &self,
        request: &Value,
        table: &DataConTable,
        source: FormSource,
    ) -> Result<Value, DriverError> {
        let gate = self.resolve_gate(&source);
        gate.delegation_progress(&DelegationPhase::Started {
            brief: rendered_result_snippet(&request.to_string()),
        });
        let started = std::time::Instant::now();
        let mut guard = self.subagent.lock();
        let handler = match guard.as_mut() {
            Some(handler) => handler,
            None => {
                let reason =
                    "the authored loop called a Subagent verb (spawnAgent/spawnAgentRaw) but no \
                     subagent handler is configured — wire one with \
                     SelfHarnessDriver::set_subagent_handler (the tidepool-selfharness binary \
                     does this when TIDEPOOL_MEMORY_REPO is set)"
                        .to_string();
                gate.delegation_progress(&DelegationPhase::Failed {
                    reason: reason.clone(),
                    duration: started.elapsed(),
                });
                return Err(DriverError::Session(reason));
            }
        };
        let dispatched =
            tokio::task::block_in_place(|| Self::dispatch_outer_effect(handler, request, table));
        let elapsed = started.elapsed();
        match dispatched {
            Ok(value) => {
                tracing::info!(
                    elapsed_ms = elapsed.as_millis() as u64,
                    "outer subagent suspension serviced"
                );
                gate.delegation_progress(&DelegationPhase::Settled {
                    outcome: rendered_result_snippet(&value.to_string()),
                    duration: elapsed,
                });
                Ok(value)
            }
            Err(e) => {
                let reason = format!("subagent dispatch: {e}");
                gate.delegation_progress(&DelegationPhase::Failed {
                    reason: reason.clone(),
                    duration: elapsed,
                });
                Err(DriverError::Session(reason))
            }
        }
    }

    /// Service a Console/Worktree/RepoEvent/Exec/Journal suspension raised by
    /// the AUTHORED outer loop (`kind` classified by [`engine::classify_hole`]):
    /// dispatch the ORIGINAL request into the matching driver-owned handler
    /// via [`Self::dispatch_outer_effect`] — the same decode-dispatch-convert
    /// shape [`Self::service_outer_subagent`] uses, generalized over which
    /// handler is reached. `Console`'s `say` additionally posts its text to
    /// the operator feed the way the `note` servicing arm does
    /// ([`Self::announce_note`]) before resuming with `()`.
    fn service_outer_effect(
        &mut self,
        kind: engine::OuterEffectKind,
        request: &Value,
        table: &DataConTable,
    ) -> Result<Value, DriverError> {
        if kind == engine::OuterEffectKind::Console {
            if let Ok(tidepool_handlers::ConsoleReq::Print(text)) =
                <tidepool_handlers::ConsoleReq as tidepool_bridge::FromCore>::from_value(
                    request, table,
                )
            {
                self.announce_note(FormSource::OuterLoop, &text);
            }
        }
        let mut handlers = self.handlers.lock();
        match kind {
            engine::OuterEffectKind::Console => {
                let handler = handlers.console.as_mut().ok_or_else(|| {
                    Self::unwired_outer_effect_error("Console", "say", "set_console_handler")
                })?;
                Self::dispatch_outer_effect(handler, request, table)
            }
            engine::OuterEffectKind::Worktree => {
                let handler = handlers.worktree.as_mut().ok_or_else(|| {
                    Self::unwired_outer_effect_error(
                        "Worktree",
                        "createWorktree/lookupWorktree/listWorktrees/worktreeBranch/worktreeHead",
                        "set_worktree_handler",
                    )
                })?;
                Self::dispatch_outer_effect(handler, request, table)
            }
            engine::OuterEffectKind::RepoEvent => {
                let handler = handlers.event.as_mut().ok_or_else(|| {
                    Self::unwired_outer_effect_error(
                        "RepoEvent",
                        "withHandler (repository events)",
                        "set_event_handler",
                    )
                })?;
                Self::dispatch_outer_effect(handler, request, table)
            }
            engine::OuterEffectKind::Exec => {
                let handler = handlers.exec.as_mut().ok_or_else(|| {
                    Self::unwired_outer_effect_error(
                        "Exec",
                        "run/runIn/runArgv",
                        "set_exec_handler",
                    )
                })?;
                Self::dispatch_outer_effect(handler, request, table)
            }
            engine::OuterEffectKind::Journal => {
                let handler = handlers.journal.as_mut().ok_or_else(|| {
                    Self::unwired_outer_effect_error("Journal", "record", "set_journal_handler")
                })?;
                Self::dispatch_outer_effect(handler, request, table)
            }
        }
        .map_err(|e| DriverError::Session(format!("{kind:?} dispatch: {e}")))
    }

    /// Non-blocking companion to the `RepoEventAwait` interception inside the
    /// `SuspensionRouting::OuterEffect` servicing arm (PRD 20 S1-L4 wave 2): decode
    /// the original suspended request's `subscription`, poll the event
    /// handler's plain (non-sleeping) drain, and report `None` on an empty
    /// batch — still parked, nothing to resume with — or `Some(value)`
    /// already encoded exactly as `RepoEventAwait`'s own dispatch would
    /// encode it (`Either EventError [RepositoryEvent]`), ready to resume the
    /// hole with directly. Never calls `repo_event_await` — that verb's own
    /// blocking loop is exactly what this exists to avoid running inline in
    /// the scheduler.
    fn poll_repo_event_await(
        &mut self,
        request: &Value,
        table: &DataConTable,
    ) -> Result<Option<Value>, DriverError> {
        use tidepool_bridge::FromCore;
        let mut handlers = self.handlers.lock();
        let handler = handlers.event.as_mut().ok_or_else(|| {
            Self::unwired_outer_effect_error(
                "RepoEvent",
                "withHandler (repository events)",
                "set_event_handler",
            )
        })?;
        let req = tidepool_handlers::RepoEventReq::from_value(request, table)
            .map_err(|e| DriverError::Session(format!("RepoEventAwait decode: {e}")))?;
        let tidepool_handlers::RepoEventReq::RepoEventAwait(subscription, _timeout_ms) = req else {
            return Err(DriverError::Session(
                "poll_repo_event_await: decoded request was not RepoEventAwait (scheduler bug)"
                    .into(),
            ));
        };
        // `_timeout_ms` is deliberately unread: `nextEvent`'s own calling
        // convention (`awaitFirst`) always passes -1 (no deadline) — a
        // bounded wait is expressed by merging an `after ms` deadline into
        // the SAME subscription instead, which arrives as an ordinary `Tick`
        // through this same drain. No caller in the authored stdlib surface
        // passes a non-negative timeout to this verb.
        let result = handler.repo_event_drain(subscription);
        if let Ok(batch) = &result {
            if batch.is_empty() {
                return Ok(None);
            }
        }
        let value = result
            .to_value(table)
            .map_err(|e| DriverError::Session(format!("RepoEventAwait encode: {e}")))?;
        Ok(Some(value))
    }

    /// The legible "no handler wired" error every [`Self::service_outer_effect`]
    /// branch raises for its own effect — names the verb family and the
    /// setter that fixes it, never a hang.
    fn unwired_outer_effect_error(effect: &str, verbs: &str, setter: &str) -> DriverError {
        DriverError::Session(format!(
            "the authored loop called a {effect} verb ({verbs}) but no {effect} handler is \
             configured — wire one with SelfHarnessDriver::{setter}"
        ))
    }

    /// The shared decode-dispatch-convert shape every outer-row effect
    /// suspension goes through: decode the ORIGINAL suspended request `Value`
    /// via the handler's generated `<Eff>Req: FromCore` (against the loop
    /// compile's own table — never JSON-probed), dispatch it into `handler`
    /// under `tokio::task::block_in_place` (the same discipline every
    /// `OperatorGate` call and [`Self::service_outer_subagent`] use), and
    /// convert the [`tidepool_effect::Response`] back into a resumable
    /// `Value` — a `Complete` value as-is, a `List` folded into a cons chain
    /// from its carried `cons_id`/`nil_id` (mirrors the in-machine dispatch
    /// path's own fold, `tidepool_effect::machine`; a suspending outer row
    /// never reaches that path itself, so this is the suspend-side
    /// equivalent). No outer-row verb returns a list today, but a future one
    /// (`respond_list`) resumes correctly without another servicing site.
    fn dispatch_outer_effect<H>(
        handler: &mut H,
        request: &Value,
        table: &DataConTable,
    ) -> Result<Value, tidepool_effect::EffectError>
    where
        H: tidepool_effect::EffectHandler<tidepool_mcp::CapturedOutput>,
    {
        use tidepool_bridge::FromCore;
        use tidepool_effect::dispatch::EffectContext;
        let req = H::Request::from_value(request, table)?;
        let captured = tidepool_mcp::CapturedOutput::new();
        let resp = tokio::task::block_in_place(|| {
            let cx = EffectContext::with_user(table, &captured);
            handler.handle(req, &cx)
        })?;
        Ok(match resp {
            tidepool_effect::Response::Complete(v) => v,
            tidepool_effect::Response::List {
                items,
                cons_id,
                nil_id,
            } => {
                let mut acc = Value::Con(nil_id, vec![]);
                for item in items.into_iter().rev() {
                    acc = Value::Con(cons_id, vec![item, acc]);
                }
                acc
            }
        })
    }

    async fn service_outer_askuser_hole(
        &mut self,
        hole: ResidentHole,
        routing: SuspensionRouting,
        compiled: &CompiledTurn,
    ) -> Result<ResidentOutcome, DriverError> {
        let mut hole = hole;
        let mut routing = routing;
        let mut reprompts: u32 = 0;
        loop {
            let outcome = match routing {
                SuspensionRouting::AskUser { shape } => {
                    let submission = self
                        .present_askuser_form(&mut reprompts, FormSource::OuterLoop, &shape)
                        .await?;
                    let answer = engine::json_answer_to_value(&submission, &compiled.table)
                        .map_err(|e| {
                            DriverError::Session(format!("outer askUser submission decode: {e}"))
                        })?;
                    let sid = self.outer_sid()?;
                    self.agent
                        .with_session(sid, |s| s.resume(hole, answer))
                        .map_err(|e| DriverError::Session(e.to_string()))?
                        .map_err(|e| {
                            DriverError::Session(format!("outer askUser resume failed: {e}"))
                        })?
                }
                SuspensionRouting::Note { text } => {
                    self.announce_note(FormSource::OuterLoop, &text);
                    use tidepool_bridge::ToCore;
                    let answer = ().to_value(&compiled.table).map_err(|e| {
                        DriverError::Session(format!("bridge unit note-answer to Value: {e}"))
                    })?;
                    let sid = self.outer_sid()?;
                    self.agent
                        .with_session(sid, |s| s.resume(hole, answer))
                        .map_err(|e| DriverError::Session(e.to_string()))?
                        .map_err(|e| {
                            DriverError::Session(format!("outer note resume failed: {e}"))
                        })?
                }
                other => {
                    return Err(DriverError::Session(format!(
                        "service_outer_askuser_hole: expected an AskUser or Note routing, \
                         got {other:?}"
                    )))
                }
            };

            match &outcome {
                ResidentOutcome::Suspended {
                    hole: next_hole,
                    request,
                    ..
                } => {
                    let classified =
                        engine::classify_hole(request, &compiled.table, &compiled.asks)?;
                    if matches!(
                        classified.routing,
                        SuspensionRouting::AskUser { .. } | SuspensionRouting::Note { .. }
                    ) {
                        // askUser's Haskell-side decode-retry re-suspended on a
                        // fresh form, or the chain's next `note`/`askUser` step
                        // — re-drive it (does NOT count as progress).
                        hole = next_hole.clone();
                        routing = classified.routing;
                        continue;
                    }
                    // A runLLMTurn suspension (or anything else) — hand it back
                    // to the main loop, which classifies and services it.
                    return Ok(outcome);
                }
                ResidentOutcome::Completed { .. } => return Ok(outcome),
            }
        }
    }

    /// Resolve which operator gate a form/note tied to `source` should reach:
    /// a labeled branch child (`source` is [`FormSource::Answerer`] AND the
    /// node carries an entry in [`Self::node_labels`]) routes to
    /// [`crate::selfharness::operator::OperatorGate::node_gate`]; every other
    /// case — an unlabeled answerer node, or [`FormSource::OuterLoop`] (the
    /// outer session's own seed question / between-loops asks, which are
    /// never node-scoped) — falls back to the default gate, byte-identical to
    /// before per-node routing existed.
    fn resolve_gate(&self, source: &FormSource) -> Arc<dyn OperatorGate> {
        if let FormSource::Answerer { node } = source {
            if let Some(label) = self.node_labels.lock().get(node).cloned() {
                if let Some(gate) = self.gate.node_gate(&label) {
                    return gate;
                }
            }
        }
        Arc::clone(&self.gate)
    }

    /// Post `text` to the operator gate and emit [`Event::NotePosted`] — the
    /// shared, non-blocking half of servicing a `note` hole. `source`
    /// distinguishes a nested answerer's own note from one the AUTHORED
    /// OUTER loop raised directly, same as [`FormSource`] does for a form.
    /// Unlike [`Self::present_askuser_form`], there is nothing to wait for:
    /// the caller resumes immediately after this returns.
    fn announce_note(&self, source: FormSource, text: &str) {
        let gate = self.resolve_gate(&source);
        self.emit(Event::NotePosted {
            source,
            text: text.to_string(),
        });
        let posted = text.to_string();
        tokio::task::block_in_place(move || gate.post_note(&posted));
    }

    /// Post `text` to the operator gate and resume `node`'s `note` hole
    /// immediately with `()` via [`Harness::answer_note`] — no operator
    /// interaction, no model round. Unlike `askUser`'s reprompt cap, this has
    /// no bound of its own: a `note` resume always makes progress (the next
    /// pending hole, or none at all), so nothing here can spin.
    async fn service_note_hole(&self, node: NodeId, text: &str) -> Result<(), DriverError> {
        self.announce_note(FormSource::Answerer { node }, text);
        self.agent.answer_note(node).await?;
        Ok(())
    }

    /// A `getStateJson` hole's response: the current loop iteration's entry
    /// state, or `Null` when there is none (the general Agent path, which
    /// carries no cycle state at all). The ONE construction both
    /// [`Self::drain_note_holes`]'s node-level resume and
    /// [`Self::service_thread_ready`]'s raw-thread resume read from (sol
    /// cross-family review finding 9c) — delivery differs (a node-level
    /// `answer_dialog` vs. a raw in-machine `resume`), the value doesn't.
    fn loop_state_snapshot(&self) -> Json {
        self.loop_state_json.clone().unwrap_or(Json::Null)
    }

    /// Drain a leading run of `note` holes on `node`, starting from
    /// `classified` (which may or may not already be `SuspensionRouting::Note` —
    /// a no-op passthrough when it isn't): post each via
    /// [`Self::service_note_hole`] and resume immediately with `()`,
    /// repeating while the resume keeps landing on ANOTHER note. Returns the
    /// first NON-note pending hole once the chain stops — the caller (already
    /// prepared to dispatch on `Finalize`/`AskUser`/`Fork`) proceeds from
    /// there — or `None` if the chain completed the node with NO further
    /// suspension (the caller's existing corrective-retry path, same as a
    /// plain `Completed` round outcome).
    async fn drain_note_holes(
        &self,
        node: NodeId,
        mut hole: String,
        mut classified: ClassifiedSuspension,
    ) -> Result<Option<(String, ClassifiedSuspension)>, DriverError> {
        loop {
            match classified.routing.clone() {
                SuspensionRouting::Note { text } => {
                    self.service_note_hole(node, &text).await?;
                }
                SuspensionRouting::ReadState => {
                    // Immediate resume with the cycle's entry state — no
                    // operator, no model round (note's service shape).
                    let state = self.loop_state_snapshot();
                    retry_on_turn_in_flight_async(|| self.agent.answer_dialog(node, state.clone()))
                        .await?;
                }
                // PRD 21 C5: a branch-node window's own `delegate` call
                // lowers to a real `Subagent` send (`Tidepool.Agent.Delegate.
                // runDelegate`) — same suspension, same driver-owned
                // handler, as the AUTHORED outer loop's `spawnAgent`
                // (`Self::service_outer_subagent`); this is the SAME
                // dispatch, just resumed against THIS node's own session
                // (`Harness::resume_with_value`) rather than the outer one.
                // No operator, no model round — the saga itself is the
                // "wait" (worktree + backend cycle), not a suspension this
                // driver presents to anyone.
                SuspensionRouting::Subagent => {
                    let (pending_suspension, _classified, table, request) = self
                        .agent
                        .pending_suspension_with_request(node)
                        .ok_or_else(|| {
                            DriverError::Session(format!(
                                "node {node:?} has no pending Subagent hole to service"
                            ))
                        })?;
                    let value = self.service_outer_subagent(
                        &request,
                        &table,
                        FormSource::Answerer { node },
                    )?;
                    retry_on_turn_in_flight_async(|| {
                        self.agent
                            .resume_with_value(node, &pending_suspension, value.clone())
                    })
                    .await?;
                }
                _ => break,
            }
            match self.agent.pending_suspension_full(node) {
                Some((next_hole, next_classified, _table)) => {
                    hole = next_hole.0;
                    classified = next_classified;
                }
                None => return Ok(None),
            }
        }
        Ok(Some((hole, classified)))
    }

    /// Present `shape` via the operator gate and return the operator's raw
    /// submission — the servicing step shared by [`Self::service_askuser_hole`]
    /// (a nested answerer's own form) and [`Self::service_outer_askuser_hole`]
    /// (the authored OUTER loop's own form): check + increment the shared
    /// reprompt cap, emit [`Event::FormPresented`], block on the operator gate,
    /// then emit [`Event::FormSubmitted`]. `source` is the only observable
    /// difference between the two callers — a genuine tag distinguishing which
    /// side raised the form in the transcript, not a hidden behavior fork. It
    /// is also what [`Self::resolve_gate`] reads to route a labeled branch
    /// child's form to its own per-node gate.
    ///
    /// This is the ONE site every form presentation funnels through
    /// (regardless of `source`), so it also mints this presentation's
    /// [`AskId`] — one global monotonic counter rather than one per
    /// `source`, since `source` already disambiguates in the log and a
    /// re-prompt (another call here for the same logical ask) gets a FRESH
    /// id like any other presentation.
    async fn present_askuser_form(
        &self,
        reprompts: &mut u32,
        source: FormSource,
        shape: &FormShape,
    ) -> Result<Json, DriverError> {
        if *reprompts >= ASKUSER_MAX_REPROMPTS {
            return Err(DriverError::Session(format!(
                "operator form re-presented {reprompts} times without a decodable \
                 submission (a non-interactive gate at EOF, or a form whose \
                 submission never decodes)"
            )));
        }
        *reprompts += 1;

        let ask_id = AskId(self.ask_id_counter.fetch_add(1, Ordering::SeqCst) + 1);
        self.emit(Event::FormPresented {
            source: source.clone(),
            shape: shape.clone(),
            ask_id,
        });
        // `OperatorGate::present_form` is SYNC-BLOCKING by frozen contract
        // (`selfharness/operator.rs`) — a web gate parks a channel. Run it
        // under `block_in_place` so that blocking wait yields the tokio
        // worker rather than stalling it.
        let gate = self.resolve_gate(&source);
        let form = shape.clone();
        let submission = tokio::task::block_in_place(move || gate.present_form(&form));
        self.emit(Event::FormSubmitted {
            source,
            submission: submission.clone(),
            ask_id,
        });
        Ok(submission)
    }

    /// F11: the shared shape of driving one classified `Fork` hole's
    /// children to completion and assembling their answer —
    /// [`Self::drain_answerer_fork`] (direct `fork`/`forkAll`) and
    /// [`Self::service_thread_ready`]'s Fork arm (a thread's own
    /// `async (fork …)`) used to carry this ~30-line sequence as
    /// near-verbatim twins (the same drift shape that produced F3's
    /// round-loop divergence): brief normalization, the fanout element type,
    /// per-child title, sequential [`Self::drive_fork_child_agent_session`] drive,
    /// per-child [`wrap_fork_value`], then single-vs-fanout assembly. The two
    /// callers differ only in the per-child title wording (plain vs
    /// `"async "`-prefixed) and in how the ASSEMBLED answer crosses back
    /// (a node-aware resume vs a raw thread resume) — both stay with the
    /// caller. `title_single`/`title_fanout_prefix` carry the wording
    /// difference (the WORD itself changes — "fork" vs "fanout" — not just a
    /// shared prefix, so a bare prefix+index wouldn't reproduce the original
    /// titles).
    ///
    /// `Ok(Err(msg))` means one child ended in `InvocationExit` — a plain-
    /// language corrective (already retired at its own node; see
    /// [`Self::drive_fork_child_agent_session`]) — and the caller must abort
    /// the consuming block with it rather than propagate a `DriverError`.
    /// Every brief in this call is driven CONCURRENTLY, up to
    /// [`Self::concurrency_cap`] at once, via [`drive_concurrent`] — the ONE
    /// ordering/concurrency shell [`Self::service_outer_fanout`] already
    /// rides for the sibling fanout path — so a sibling that already
    /// finished has already finalized and retired independently of this
    /// return value regardless of which brief (if any) ends in `Err`; the
    /// FIRST brief (by DECLARATION order, not completion order) whose result
    /// is a mechanism error or an `InvocationExit` is what this returns.
    #[allow(clippy::too_many_arguments)]
    async fn drive_fork_children(
        &self,
        node: NodeId,
        title_single: &str,
        title_fanout_prefix: &str,
        site: crate::tree::SiteId,
        ty: Option<&str>,
        fan: &Option<FanBadge>,
        prompts: &[String],
        prompt: &str,
        source: engine::ForkSource,
        table: &DataConTable,
        fork_depth: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
        ty_label: &str,
    ) -> Result<Result<Value, String>, DriverError> {
        let briefs = engine::fork_briefs(fan, prompts, prompt)
            .map_err(|e| DriverError::Session(e.to_string()))?;
        let element_ty = match fan {
            None => ty,
            Some(_) => ty.and_then(engine::strip_list_type),
        };
        let titles: Vec<String> = if fan.is_none() {
            vec![title_single.to_string()]
        } else {
            (0..briefs.len())
                .map(|idx| format!("{title_fanout_prefix} {idx}"))
                .collect()
        };
        let cap = self.concurrency_cap;
        let briefs_ref = &briefs;
        let titles_ref = &titles;
        #[allow(clippy::type_complexity)]
        let results: Vec<(usize, Result<Result<Value, String>, DriverError>)> =
            drive_concurrent(cap, briefs.len(), |idx| {
                let brief = briefs_ref[idx];
                let title = titles_ref[idx].as_str();
                async move {
                    self.drive_fork_child_agent_session(
                        node,
                        title,
                        brief,
                        element_ty,
                        site.get(),
                        table,
                        fork_depth + 1,
                        fork_subtree,
                        ty_label,
                    )
                    .await
                }
            })
            .await;

        let mut answers = Vec::with_capacity(results.len());
        for (_, r) in results {
            let value = match r? {
                Ok(v) => v,
                Err(msg) => return Ok(Err(msg)),
            };
            answers.push(wrap_fork_value(source, value, table)?);
        }
        if fan.is_none() {
            Ok(Ok(answers.pop().expect(
                "fork_briefs yields exactly one brief for a single fork",
            )))
        } else {
            engine::build_list_value(answers, table)
                .map(Ok)
                .map_err(|e| DriverError::Session(e.to_string()))
        }
    }

    /// Drain a `SuspensionRouting::Fork` suspension on the per-loop answerer
    /// (`forkAll`/`fork` via `Tidepool.Fork`): resume it by driving each
    /// child to completion on the full pump row via
    /// [`Self::drive_fork_child_agent_session`] (fork-subsumes-split step 1 — a
    /// child can `askUser`, `fork` again, and go multi-round; it is not the
    /// one-shot general-Agent path), looping in case the parent immediately
    /// hits ANOTHER fork right after resuming (e.g. `forkAll` then `fork` in
    /// sequence). `Ok(Some(out))` means the parent landed on `Finalize` — the
    /// caller should `return Ok(out)` straight through, same as any other
    /// finalize suspension. `Ok(None)` means the parent's block ran to
    /// completion WITHOUT ever finalizing; this already reopened the node and
    /// pushed the same corrective nudge [`Self::drive_agent_session_to_finalize`]'s
    /// `Completed` arm uses, so the caller should just let its round loop
    /// keep driving. Any OTHER resumed hole (an operator form after the fork
    /// results, a `wait` on a thread spawned earlier in the block) is handed
    /// back to the dispatcher — composing fork with askUser/async in one
    /// block is an ordinary continuation.
    async fn drain_answerer_fork(
        &self,
        node: NodeId,
        ty_label: &str,
        budget: &mut ForkBudget,
        fork_depth: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
    ) -> Result<Option<TurnOutcome>, DriverError> {
        loop {
            let Some((hole, classified, table)) = self.agent.pending_suspension_full(node) else {
                return Err(DriverError::Session(
                    "fork resume: node has no pending hole to service".into(),
                ));
            };
            // The budget check covers EVERY fork this drain loop services,
            // not only the one the dispatcher saw — `forkAll` then `fork` in
            // sequence spends per iteration. Spend BEFORE spawn; a refusal
            // costs nothing.
            let cost = ForkBudget::cost(&classified.routing);
            if matches!(classified.routing, SuspensionRouting::Fork { .. }) {
                if let Some(msg) = self.check_fork_budgets(budget, cost, fork_subtree, ty_label) {
                    retry_on_turn_in_flight(|| {
                        self.agent.refuse_pending_suspension(node, msg.clone())
                    })
                    .await?;
                    self.agent.push_user_turn(node, &msg)?;
                    return Ok(None);
                }
            }
            // Children run as full sessions on the pump
            // (`drive_fork_child_agent_session` — fork-subsumes-split step 1).
            match &classified.routing {
                SuspensionRouting::Fork {
                    site,
                    ty,
                    fan,
                    prompts,
                    source,
                } => {
                    match self
                        .drive_fork_children(
                            node,
                            "fork answerer",
                            "fanout answerer",
                            *site,
                            ty.as_deref(),
                            fan,
                            prompts,
                            &classified.prompt,
                            *source,
                            &table,
                            fork_depth,
                            fork_subtree,
                            ty_label,
                        )
                        .await?
                    {
                        Ok(answer) => {
                            retry_on_turn_in_flight_async(|| {
                                self.agent.resume_with_value(node, &hole, answer.clone())
                            })
                            .await?;
                        }
                        // A child ended in `InvocationExit`: abort this
                        // block with the corrective, the same shape the
                        // budget-refusal branch above uses — the WINDOW
                        // survives, only the parked continuation dies.
                        Err(msg) => {
                            retry_on_turn_in_flight(|| {
                                self.agent.refuse_pending_suspension(node, msg.clone())
                            })
                            .await?;
                            self.agent.push_user_turn(node, &msg)?;
                            return Ok(None);
                        }
                    }
                }
                other => {
                    return Err(DriverError::Session(format!(
                        "drain_answerer_fork: expected a pending Fork hole, got {other:?}"
                    )));
                }
            }

            match self.agent.pending_suspension(node).map(|c| c.routing) {
                Some(SuspensionRouting::Fork { .. }) => continue,
                // Finalize, or any OTHER routing (an operator form after the
                // fork results, a `wait` on a thread spawned earlier in the
                // block): hand the outcome back — the dispatcher in
                // `drive_agent_session_to_finalize` routes it. Before the
                // answerer-plane green scheduler this arm hard-errored on
                // everything but Finalize; composing fork with askUser/async
                // in one block is now an ordinary continuation.
                Some(_) => {
                    return self
                        .agent
                        .pending_turn_outcome(node)
                        .map(Some)
                        .ok_or_else(|| {
                            DriverError::Session("fork resume: pending hole vanished".into())
                        });
                }
                None => break,
            }
        }

        self.agent.reopen_node(node)?;
        let ty_disp = display_ty(ty_label);
        self.agent.push_user_turn(
            node,
            &format!(
                "Round complete — the forked sub-answerers returned and their results \
                 are bound in your session (evaluate a binding to see it). The request \
                 still awaits its answer: fork another batch, keep working, or evaluate \
                 `finalize @{ty_disp} value` when ready (that ends the session)."
            ),
        )?;
        Ok(None)
    }

    /// Cleanup for a mechanism failure between a fork/branch child's GUI +
    /// tree-path registration and its [`BranchAgentSessionGuard`] guard coming into
    /// existence (F7): those registrations predate the guard, so nothing
    /// else retires them on an early `?` between them and
    /// `BranchAgentSessionGuard::from_lease` — `force_attached`/`mint_scope` are both
    /// fallible there (a checkout race is the F4 contention class; a dead
    /// parent scope is `ok_or_else`'d). Removes the `node_labels` entry and
    /// retires the GUI panel (when a label was registered), then retires the
    /// tree/session node itself through the ONE retirement path — safe
    /// whether or not `force_attached` ever ran: [`Harness::terminate_node`]
    /// is idempotent over a never-forced (still `Thunk`) node, and if
    /// `force_attached` DID succeed before the failure (a live `mint_scope`
    /// refusal), it also closes the realm the caller already assigned via
    /// [`Harness::set_node_realm`] — the "permanently Running tree node"
    /// half of the leak.
    fn abort_unguarded_child(&self, node: NodeId, label: Option<&str>, reason: &str) {
        if let Some(label) = label {
            self.node_labels.lock().remove(&node);
            self.gate.node_failed(label, reason);
            self.gate.retire_node(label);
        }
        self.fork_child_seq.lock().remove(&node);
        let _ = self.agent.terminate_node(node, reason);
    }

    /// Fork-subsumes-split STEP 1 (plans/fork-subsumes-split.md): drive ONE
    /// fork child as a full ATTACHED WINDOW on the shared session. A child
    /// on the window pump can explore across rounds, present operator
    /// forms, and answer with a REAL `finalize @T` —
    /// `ResidentError::ChildSuspended` is unreachable from here.
    ///
    /// The attach ladder: transcript forked from the LIVE parent's
    /// checkpoint (`register_fork_child_with_card` — the multi-round
    /// answerer card), child scope minted from the LIVE parent node's
    /// scope — which IS the declaration-inheritance wiring on the shared
    /// session (locked decision 4's ancestry scoping; no separate-session
    /// include dance), and the finalize contract pinned from the fork
    /// site's own resolved modules.
    ///
    /// STEP 3 (seam map §7): this child gets its own operator-GUI/tree
    /// lifecycle — a derived label/path (`Self::fork_child_label`),
    /// `node_gate`/`node_seeded` at birth (the AUTHORED brief, not the
    /// composed hole card), `node_finalized`/`node_failed` at the fold, and
    /// `retire_node` on every exit, all through [`BranchAgentSessionGuard`]
    /// so a mechanism-error `?` before the pump starts can never leak the
    /// label/path registrations or skip retirement.
    ///
    /// Exit semantics (operator decision, 2026-08-24): a child that exits
    /// without finalizing is reported via `node_failed` and retired exactly
    /// like a success, but only a MECHANISM problem still hard-fails
    /// through as `Err` — a finalized CLOSURE (v1 scope cannot carry it), a
    /// non-finalize dispatcher-contract violation, or a `DriverError` from
    /// the pump itself. A child ending in `InvocationExit` (round
    /// exhaustion, a non-answer ending, its own provider call failing) is
    /// NOT one of those: fork children are not branch positions with a
    /// typed `Left` to fold into (PRD 21 decision 6 draws that line at the
    /// concurrent `runLLMTurnFork`/`Fanout` branch position, not here), so
    /// this driver instead returns `Ok(Err(corrective))` — a plain-language
    /// message the caller ([`Self::drive_fork_children`]) hands up to
    /// [`Self::drain_answerer_fork`]/[`Self::service_thread_ready`], which
    /// abort the CONSUMING block through the same `refuse_pending_suspension`
    /// corrective plumbing [`GreenRoundExit::ForkBudgetRefused`] already
    /// uses — the parent session and the run survive; only the block that
    /// was `wait`-ing/consuming this child dies. TERMINAL FIX (seam map
    /// §7.10): a successful child is marked `NodeDone` BEFORE resource
    /// retirement (`BranchAgentSessionGuard::finalize_fork_data`), instead of
    /// the old path's accidental `NodeCancelled`-via-`terminate_node`-only
    /// ending.
    #[allow(clippy::too_many_arguments)]
    async fn drive_fork_child_agent_session(
        &self,
        parent: NodeId,
        title: &str,
        brief: &str,
        ty: Option<&str>,
        site: u32,
        table: &DataConTable,
        fork_depth: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
        ty_label: &str,
    ) -> Result<Result<Value, String>, DriverError> {
        let sid = self.outer_sid()?;
        let modules = self.agent.asks_modules(parent, site);
        let card = engine::finalize_typed_request_prompt(
            "Your parent session",
            brief,
            ty,
            &modules,
            Some(table),
            &self.agent.finalize_typed_request_prompt_effect_row(),
        );
        let node = self
            .agent
            .register_fork_child_with_card(parent, title, card)?;

        // Step 3 GUI lane: a fork child's label/path is DERIVED — see
        // `Self::fork_child_label`'s doc. Registered NOW, not on its first
        // ask/note, so the operator watches the tree grow.
        let label = self.fork_child_label(parent, brief);
        self.node_labels.lock().insert(node, label.clone());
        let _ = self.gate.node_gate(&label);
        self.gate.node_seeded(&label, brief);

        // Attach to the SHARED session: no per-node machine, no separate
        // decl plane — the child's turns run as a realm on the one machine.
        if let Err(e) = self.agent.force_attached(node, Actor::Operator, sid) {
            let reason = format!("fork child of {parent:?}: attach failed: {e}");
            self.abort_unguarded_child(node, Some(&label), &reason);
            return Err(e.into());
        }
        let realm = self.mint_realm();
        self.agent.set_node_realm(node, realm);
        // F4: a fork child's window can run concurrently against a sibling
        // fanout child (or another fork subtree entirely) on the SAME
        // shared outer session — opt in so its very first checkout (the
        // scope mint below, then every turn `drive_agent_session_to_finalize`
        // drives) waits instead of failing fast on what is "expected, benign
        // contention" everywhere else on this plane.
        self.agent.set_retry_checkout_on_contention(node, true);
        // Scope minted from the LIVE parent's scope: this is what makes the
        // parent's declarations (and its ancestors') readable and sibling
        // declarations invisible — the same scope-tree ancestry the branch
        // path gets from its frozen snapshot's scope.
        let parent_scope = self.agent.node_scope(parent);
        let child_scope = match self
            .agent
            .with_session_retrying(node, sid, |s| s.mint_scope(parent_scope))
            .await
        {
            Ok(Some(scope)) => scope,
            Ok(None) => {
                let reason = format!(
                    "fork child of {parent:?}: parent scope {parent_scope:?} is not live \
                     (its window already retired?)"
                );
                self.abort_unguarded_child(node, Some(&label), &reason);
                return Err(DriverError::Session(reason));
            }
            Err(e) => {
                let reason = format!("fork child of {parent:?}: mint_scope failed: {e}");
                self.abort_unguarded_child(node, Some(&label), &reason);
                return Err(DriverError::Session(reason));
            }
        };
        self.agent.set_node_scope(node, child_scope);
        self.agent
            .set_answer_contract(node, self.answer_contract(ty, &modules));
        self.emit(Event::TurnStart { node });

        // This child's mode, typed (`AgentSessionMode::require_one_shot`'s doc):
        // it answers exactly once, then is retired below.
        let lease = AgentSessionMode::OneShotBranch {
            node,
            realm,
            scope: child_scope,
        };
        // Every exit below this point retires exactly through `window`
        // (`fold_exit`, `finalize_fork_data`, or — for a mechanism-error `?`
        // ABOVE this point, before the guard exists — a hand-rolled cleanup
        // would be needed; there is none between here and the guard's
        // construction). See `BranchAgentSessionGuard`'s doc.
        let window = BranchAgentSessionGuard::from_lease(lease, self.agent.clone())?;

        // Box::pin: the pump drives child pumps (a fork child can itself
        // present forms, and — step 2 — fork), so this call is genuinely
        // recursive; the indirection is the async-recursion requirement,
        // nothing more.
        let outcome = Box::pin(self.drive_agent_session_to_finalize(
            node,
            ty,
            site,
            fork_depth,
            fork_subtree,
            AgentSessionExitPolicy::Interactive,
        ))
        .await;
        self.emit(Event::TurnEnd { node });

        // Every path below this point is done with this node's own GUI
        // registration — see the insert above.
        let retired_label = self.node_labels.lock().remove(&node);
        if let Some(label) = &retired_label {
            self.gate.retire_node(label);
        }
        // This node retires here regardless of outcome below — purge its
        // own fork-child-label counter (it may have spawned children of its
        // own) at the same point its other per-node bookkeeping goes, so
        // `fork_child_seq` does not grow without bound across a long-running
        // companion tree (sol cross-family review finding 11).
        self.fork_child_seq.lock().remove(&node);

        match outcome {
            Ok(Ok(TurnOutcome::Suspended { classified, .. }))
                if matches!(classified.routing, SuspensionRouting::Finalize { .. }) =>
            {
                if self.agent.finalize_is_closure(node) {
                    let reason = format!(
                        "fork child {node:?} finalized a closure — a fork answer must \
                         be plain data in this driver (v1 scope)"
                    );
                    if let Some(label) = &retired_label {
                        self.gate.node_failed(label, &reason);
                    }
                    window.fold_exit(&reason);
                    return Err(DriverError::Session(reason));
                }
                let (value, rendered) = window
                    .finalize_fork_data()
                    .await
                    .map_err(|e| DriverError::Session(format!("fork child finalize take: {e}")))?;
                if let Some(label) = &retired_label {
                    self.gate.node_finalized(label, &rendered);
                }
                self.emit(Event::Finalize {
                    node,
                    value: rendered,
                });
                Ok(Ok(value))
            }
            Ok(Ok(other)) => {
                let reason = format!(
                    "fork child {node:?} returned a non-finalize outcome from the pump \
                     ({}) — dispatcher contract violation",
                    turn_outcome_tag(&other)
                );
                if let Some(label) = &retired_label {
                    self.gate.node_failed(label, &reason);
                }
                window.fold_exit(&reason);
                Err(DriverError::Session(reason))
            }
            // Operator decision (2026-08-24): a child ending in
            // `InvocationExit` no longer kills the parent's turn — the
            // child's own node still retires and reports `node_failed`
            // (unchanged), but instead of hard-failing through as `Err`
            // this returns `Ok(Err(corrective))`, a plain-language message
            // (docs/GLOSSARY.md prompt rules: no `InvocationExit`, no
            // constructor name) naming the child by its derived path — the
            // caller aborts only the block that was consuming this child.
            Ok(Err(exit)) => {
                let reason = format!("fork child {node:?} ended without an answer: {exit}");
                if let Some(label) = &retired_label {
                    self.gate.node_failed(label, &reason);
                }
                window.fold_exit(&reason);
                let path = retired_label.clone().unwrap_or_else(|| format!("{node:?}"));
                Ok(Err(fork_child_failure_corrective(&path, &exit, ty_label)))
            }
            Err(e) => {
                let reason = "fork child retired (mechanism failure)";
                if let Some(label) = &retired_label {
                    self.gate.node_failed(label, &format!("{reason}: {e}"));
                }
                window.fold_exit(reason);
                Err(e)
            }
        }
    }

    /// Fork-subsumes-split step 3 (seam map §7.1): derive a stable GUI
    /// label/tree-path for a fork child. Unlike `runLLMTurnBranchLabeled`,
    /// a fork's brief carries no wire-carried label, so both the label and
    /// the path segment are derived here rather than read off the wire.
    ///
    /// Base path: the parent's own registered GUI label (`node_labels` —
    /// the parent is itself a labeled branch/fork child), else the fixed
    /// root id `"root"` (mirrors `tidepool_web::DEFAULT_NODE_ID` as a
    /// literal — this crate cannot depend on `tidepool-web`).
    ///
    /// Child segment: `f<idx>-<ascii-slug-of-brief-prefix>`, mirroring a
    /// structurally-labeled branch's own `root/1-child` convention so a
    /// fork child's tree position reads the same way. `idx` is a per-PARENT
    /// monotonic counter (`Self::fork_child_seq`), not threaded in from the
    /// caller — see that field's doc for why.
    fn fork_child_label(&self, parent: NodeId, brief: &str) -> String {
        let idx = {
            let mut seq = self.fork_child_seq.lock();
            let counter = seq.entry(parent).or_insert(0);
            let idx = *counter;
            *counter += 1;
            idx
        };
        let base = self
            .node_labels
            .lock()
            .get(&parent)
            .cloned()
            .unwrap_or_else(|| "root".to_string());
        format!("{base}/{}", fork_child_path_segment(idx, brief))
    }

    /// One scheduling pass of the answerer-plane green scheduler: service the
    /// NODE's own pending `Green` suspensions (node-aware resumes) and pump
    /// THREAD chains (raw resumes, shared [`Self::service_green_hole`]) until
    /// the node parks on something that isn't Green ([`GreenRoundExit::NodeParked`])
    /// or completes without finalizing ([`GreenRoundExit::NodeDone`]).
    /// `green` persists across passes within one ROUND (the dispatcher may
    /// interleave askUser/fork servicing between passes) and is swept at the
    /// round boundary by [`Self::sweep_green_round`].
    ///
    /// The node's blocked `wait` is deliberately NOT registered in
    /// `green.waiters`: each iteration re-services its pending
    /// `AsyncJoinAnyWith` (a pure winner scan when nothing settled), so a
    /// settle is observed on the very next loop — and the raw waiter-wake
    /// path structurally cannot touch the node chain.
    async fn service_green_round(
        &self,
        node: NodeId,
        green: &mut ModelRoundGreenThreadScheduler,
        budget: &mut ForkBudget,
        fork_depth: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
        ty_label: &str,
    ) -> Result<GreenRoundExit, DriverError> {
        loop {
            let Some((hole, classified, table, asks, request)) =
                self.agent.pending_suspend_artifacts(node)
            else {
                return Ok(GreenRoundExit::NodeDone);
            };
            if !matches!(classified.routing, SuspensionRouting::Green) {
                return Ok(GreenRoundExit::NodeParked);
            }
            let blocked = match self
                .service_green_hole(
                    Some(node),
                    GreenChain::Primary,
                    &hole.0,
                    &request,
                    &table,
                    &mut green.threads,
                    &mut green.waiters,
                    &mut green.next_tid,
                    &mut green.next_thread_realm,
                    &mut green.ready,
                    GreenDelivery::Node { node, hole: &hole },
                )
                .await?
            {
                GreenHoleServiced::Proceed(b) => b,
                GreenHoleServiced::Misuse(msg) => return Ok(GreenRoundExit::AsyncMisuse { msg }),
            };
            if !blocked {
                continue;
            }
            // The node is blocked on a join with no terminal candidate.
            // Drain every FORK-routed ready item out of `green.ready` and
            // drive them all CONCURRENTLY (`Self::drive_fork_ready_batch`):
            // by construction, every `async (fork @T brief)` a straight-line
            // block spawned before its first `wait` has already reached its
            // OWN `fork` suspension by the time the node blocks (spawning a
            // thread is a synchronous JIT step, no model call involved), so
            // every fork this wait could possibly be blocked on is already
            // sitting in `ready`. Everything else keeps the single-item
            // path below — cheap, immediate resumes with nothing to gain
            // from batching.
            let mut fork_batch: Vec<(GreenReady, ClassifiedSuspension)> = Vec::new();
            let mut rest: VecDeque<GreenReady> = VecDeque::with_capacity(green.ready.len());
            for item in green.ready.drain(..) {
                let classified = match &item.outcome {
                    ResidentOutcome::Suspended { request, .. } => {
                        engine::classify_hole(request, &table, &asks).ok()
                    }
                    ResidentOutcome::Completed { .. } => None,
                };
                match classified {
                    Some(c) if matches!(c.routing, SuspensionRouting::Fork { .. }) => {
                        fork_batch.push((item, c));
                    }
                    // A classify failure here is not lost: the item goes to
                    // `rest` and `service_thread_ready` below re-classifies
                    // it (and surfaces the same error) on its own turn.
                    _ => rest.push_back(item),
                }
            }
            green.ready = rest;
            if !fork_batch.is_empty() {
                match self
                    .drive_fork_ready_batch(
                        node,
                        fork_batch,
                        &table,
                        green,
                        budget,
                        fork_depth,
                        fork_subtree,
                        ty_label,
                    )
                    .await?
                {
                    ThreadServiced::Continue => continue,
                    ThreadServiced::BudgetRefused { msg } => {
                        return Ok(GreenRoundExit::ForkBudgetRefused { msg });
                    }
                    ThreadServiced::Misuse(msg) => {
                        return Ok(GreenRoundExit::AsyncMisuse { msg });
                    }
                    ThreadServiced::ChildFailed { msg } => {
                        return Ok(GreenRoundExit::ForkChildFailed { msg });
                    }
                }
            }
            // One thread step, then loop (the join re-check observes any
            // settle).
            let Some(GreenReady { chain, outcome }) = green.ready.pop_front() else {
                // Model-attributable, not a mechanism failure: the block
                // awaits a thread no ready work can ever settle — typically
                // a `wait` on a handle from an EARLIER round (swept at the
                // round boundary) or a thread deadlock. Abort the block with
                // a corrective instead of ending the whole run.
                return Ok(GreenRoundExit::AsyncMisuse {
                    msg: "your block is waiting on a thread that has no runnable work \
                          — usually a `wait` on a handle from an earlier round (thread \
                          handles do not survive a round boundary; spawn and wait in \
                          the SAME block), or threads waiting on each other"
                        .into(),
                });
            };
            match self
                .service_thread_ready(node, chain, outcome, green)
                .await?
            {
                ThreadServiced::Continue => {}
                ThreadServiced::BudgetRefused { msg } => {
                    return Ok(GreenRoundExit::ForkBudgetRefused { msg });
                }
                ThreadServiced::Misuse(msg) => {
                    return Ok(GreenRoundExit::AsyncMisuse { msg });
                }
                ThreadServiced::ChildFailed { msg } => {
                    return Ok(GreenRoundExit::ForkChildFailed { msg });
                }
            }
        }
    }

    /// Drive every FORK-routed thread-chain ready item in `batch`
    /// CONCURRENTLY, up to [`Self::concurrency_cap`] at once, via
    /// [`drive_concurrent`] and [`Self::drive_fork_children`] — called by
    /// [`Self::service_green_round`] once its own node blocks and it has
    /// drained every currently fork-routed [`GreenReady`] out of
    /// `green.ready`. This is the OUTER layer of the same concurrency shell
    /// [`Self::drive_fork_children`] already applies WITHIN one thread's own
    /// `forkAll` batch — here the batch spans DIFFERENT threads' own `fork`
    /// calls instead.
    ///
    /// Budget admission for the whole batch is checked EAGERLY, in the
    /// batch's original FIFO (== spawn) order, before any child is driven —
    /// `check_fork_budgets` is a compare-exchange spend-before-spawn (safe
    /// under overlap by construction — see the ANTI-PATTERNS note against
    /// re-deriving it), so checking the batch upfront in queue order
    /// reproduces the exact admission decisions the old fully-sequential
    /// scheduler made one ready item at a time. The FIRST refusal stops
    /// admission for the REST of the batch — mirroring the old
    /// pop-one-at-a-time loop, which never even looked at a later ready item
    /// once an earlier one aborted the round — so an item after a refusal is
    /// left unresumed; the round is about to abort regardless, and
    /// [`Self::sweep_green_round`] closes its still-`Running` thread realm.
    ///
    /// Every ADMITTED item is driven to completion regardless of a sibling's
    /// outcome (`drive_concurrent` never short-circuits), so a child that
    /// already finalized keeps its own retirement/GUI receipt even when a
    /// sibling in the SAME batch ends in `InvocationExit` — the abort this
    /// returns only discards the THREAD-level resume for the batch, never a
    /// child's own already-completed session bookkeeping.
    #[allow(clippy::too_many_arguments)]
    async fn drive_fork_ready_batch(
        &self,
        node: NodeId,
        batch: Vec<(GreenReady, ClassifiedSuspension)>,
        table: &DataConTable,
        green: &mut ModelRoundGreenThreadScheduler,
        budget: &mut ForkBudget,
        fork_depth: u32,
        fork_subtree: &std::sync::atomic::AtomicU32,
        ty_label: &str,
    ) -> Result<ThreadServiced, DriverError> {
        let sid = self.outer_sid()?;

        struct Admitted {
            chain: GreenChain,
            hole: String,
            site: crate::tree::SiteId,
            ty: Option<String>,
            fan: Option<FanBadge>,
            prompts: Vec<String>,
            prompt: String,
            source: engine::ForkSource,
        }

        let mut admitted: Vec<Admitted> = Vec::with_capacity(batch.len());
        let mut admission_refusal: Option<String> = None;
        for (item, classified) in batch {
            let ResidentOutcome::Suspended { hole, .. } = &item.outcome else {
                return Err(DriverError::Session(
                    "answerer green scheduler: a fork-routed ready item completed \
                     without AsyncDoneWith (every asyncSpawn body parks on its own \
                     settle — scheduler bug)"
                        .into(),
                ));
            };
            if admission_refusal.is_some() {
                // A stricter item already refused this batch; the round is
                // aborting — see this fn's own doc.
                continue;
            }
            // Cost is read off the intact routing BEFORE it's destructured
            // below, so admission needs no clone of `ty`/`prompts`.
            let cost = ForkBudget::cost(&classified.routing);
            let SuspensionRouting::Fork {
                site,
                ty,
                fan,
                prompts,
                source,
            } = classified.routing
            else {
                return Err(DriverError::Session(
                    "answerer green scheduler: drive_fork_ready_batch received a \
                     non-Fork classified item (scheduler bug)"
                        .into(),
                ));
            };
            if let Some(msg) = self.check_fork_budgets(budget, cost, fork_subtree, ty_label) {
                admission_refusal = Some(msg);
                continue;
            }
            admitted.push(Admitted {
                chain: item.chain,
                hole: hole.cont_id().to_string(),
                site,
                ty,
                fan,
                prompts,
                prompt: classified.prompt,
                source,
            });
        }

        let admitted_ref = &admitted;
        let cap = self.concurrency_cap;
        #[allow(clippy::type_complexity)]
        let results: Vec<(usize, Result<Result<Value, String>, DriverError>)> =
            drive_concurrent(cap, admitted.len(), |idx| {
                let a = &admitted_ref[idx];
                async move {
                    self.drive_fork_children(
                        node,
                        "async fork answerer",
                        "async fanout answerer",
                        a.site,
                        a.ty.as_deref(),
                        &a.fan,
                        &a.prompts,
                        &a.prompt,
                        a.source,
                        table,
                        fork_depth,
                        fork_subtree,
                        ty_label,
                    )
                    .await
                }
            })
            .await;

        let mut first_mech_err: Option<DriverError> = None;
        let mut first_child_failed: Option<String> = None;
        for (idx, r) in results {
            let a = &admitted[idx];
            match r {
                Ok(Ok(answer)) => {
                    let next = self
                        .agent
                        .with_session_retrying(node, sid, |s| {
                            s.resume(ResidentHole::plain(a.hole.clone()), answer)
                        })
                        .await
                        .map_err(|e| DriverError::Session(e.to_string()))?
                        .map_err(|e| {
                            DriverError::Session(format!("async fork resume failed: {e}"))
                        })?;
                    green.ready.push_back(GreenReady {
                        chain: a.chain,
                        outcome: next,
                    });
                }
                Ok(Err(msg)) => {
                    if first_child_failed.is_none() {
                        first_child_failed = Some(msg);
                    }
                }
                Err(e) => {
                    if first_mech_err.is_none() {
                        first_mech_err = Some(e);
                    }
                }
            }
        }

        if let Some(e) = first_mech_err {
            return Err(e);
        }
        if let Some(msg) = first_child_failed {
            return Ok(ThreadServiced::ChildFailed { msg });
        }
        if let Some(msg) = admission_refusal {
            return Ok(ThreadServiced::BudgetRefused { msg });
        }
        Ok(ThreadServiced::Continue)
    }

    /// Service one popped THREAD-chain ready item that is NOT fork-routed
    /// (`service_green_round` drains and batches every fork-routed item via
    /// [`Self::drive_fork_ready_batch`] before ever popping one for this
    /// dispatcher): classify it against the round's compile artifacts (read
    /// off the node's pending record — every chain of a round shares one
    /// compile) and dispatch. Green suspensions go through the SHARED
    /// [`Self::service_green_hole`] (raw resumes are correct for thread
    /// frames); `note`/`getStateJson`/`delegate` get their immediate
    /// service, raw-resumed. `askUser` and `finalize` inside a thread are
    /// refused loudly — operator forms and the window's answer belong on the
    /// main chain.
    async fn service_thread_ready(
        &self,
        node: NodeId,
        chain: GreenChain,
        outcome: ResidentOutcome,
        green: &mut ModelRoundGreenThreadScheduler,
    ) -> Result<ThreadServiced, DriverError> {
        let sid = self.outer_sid()?;
        let ResidentOutcome::Suspended { hole, request, .. } = outcome else {
            return Err(DriverError::Session(
                "answerer green scheduler: a thread chain completed without AsyncDoneWith \
                 (every asyncSpawn body parks on its own settle — scheduler bug)"
                    .into(),
            ));
        };
        let Some((_, _, table, asks, _)) = self.agent.pending_suspend_artifacts(node) else {
            return Err(DriverError::Session(
                "answerer green scheduler: node pending record vanished while a thread \
                 chain still had ready work"
                    .into(),
            ));
        };
        let classified = engine::classify_hole(&request, &table, &asks)
            .map_err(|e| DriverError::Session(format!("thread hole classify: {e}")))?;
        match classified.routing {
            SuspensionRouting::Green => {
                match self
                    .service_green_hole(
                        Some(node),
                        chain,
                        hole.cont_id(),
                        &request,
                        &table,
                        &mut green.threads,
                        &mut green.waiters,
                        &mut green.next_tid,
                        &mut green.next_thread_realm,
                        &mut green.ready,
                        GreenDelivery::Raw,
                    )
                    .await?
                {
                    GreenHoleServiced::Misuse(msg) => Ok(ThreadServiced::Misuse(msg)),
                    GreenHoleServiced::Proceed(_) => Ok(ThreadServiced::Continue),
                }
            }
            // Unreachable by construction: `service_green_round` drains and
            // batches every FORK-routed ready item (`Self::drive_fork_ready_batch`)
            // BEFORE ever popping one for this per-item dispatcher — see that
            // fn's own doc for why the whole batch is known upfront (every
            // `async (fork …)` in a straight-line block has already reached
            // its own suspension by the time the node blocks).
            SuspensionRouting::Fork { .. } => Err(DriverError::Session(
                "answerer green scheduler: a Fork-routed ready item reached the \
                 per-item dispatcher — service_green_round must drain and batch \
                 these via drive_fork_ready_batch before popping (scheduler bug)"
                    .into(),
            )),
            SuspensionRouting::Note { text } => {
                self.announce_note(FormSource::Answerer { node }, &text);
                let unit = ()
                    .to_value(&table)
                    .map_err(|e| DriverError::Session(format!("note () bridge: {e}")))?;
                let next = self
                    .agent
                    .with_session_retrying(node, sid, |s| {
                        s.resume(ResidentHole::plain(hole.cont_id()), unit)
                    })
                    .await
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(|e| DriverError::Session(format!("thread note resume failed: {e}")))?;
                green.ready.push_back(GreenReady {
                    chain,
                    outcome: next,
                });
                Ok(ThreadServiced::Continue)
            }
            SuspensionRouting::ReadState => {
                let state = self.loop_state_snapshot();
                let value = engine::json_answer_to_value(&state, &table)
                    .map_err(|e| DriverError::Session(format!("getStateJson bridge: {e}")))?;
                let next = self
                    .agent
                    .with_session_retrying(node, sid, |s| {
                        s.resume(ResidentHole::plain(hole.cont_id()), value)
                    })
                    .await
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(|e| {
                        DriverError::Session(format!("thread getStateJson resume failed: {e}"))
                    })?;
                green.ready.push_back(GreenReady {
                    chain,
                    outcome: next,
                });
                Ok(ThreadServiced::Continue)
            }
            SuspensionRouting::Subagent => {
                let value =
                    self.service_outer_subagent(&request, &table, FormSource::Answerer { node })?;
                let next = self
                    .agent
                    .with_session_retrying(node, sid, |s| {
                        s.resume(ResidentHole::plain(hole.cont_id()), value)
                    })
                    .await
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(|e| {
                        DriverError::Session(format!("thread subagent resume failed: {e}"))
                    })?;
                green.ready.push_back(GreenReady {
                    chain,
                    outcome: next,
                });
                Ok(ThreadServiced::Continue)
            }
            SuspensionRouting::Finalize { .. } => Ok(ThreadServiced::Misuse(
                "a green thread called `finalize` — the session's answer belongs on the \
                 main chain: `wait` your threads, then finalize from the top level"
                    .into(),
            )),
            SuspensionRouting::AskUser { .. } | SuspensionRouting::Ask { .. } => {
                Ok(ThreadServiced::Misuse(
                    "a green thread called `askUser` — operator forms belong on the main \
                 chain: ask before spawning, or after your `wait`s"
                        .into(),
                ))
            }
            other => Ok(ThreadServiced::Misuse(format!(
                "a green thread suspended on an effect this driver cannot service inside \
                 async ({other:?}) — keep operator forms and the final answer on the \
                 main chain"
            ))),
        }
    }

    /// Round-boundary sweep: close every still-open thread realm and clear
    /// the scheduler — the answerer-plane sibling of
    /// [`Self::run_loop_fragment_inner`]'s end-of-scope sweep. Settled
    /// realms are closed here too, not just Running ones: a settle parks
    /// the thread's `AsyncDoneWith` frame forever by design, so leaving a
    /// settled realm open leaks a permanently-parked hole on the shared
    /// session per successful `async`/`wait` — enough of them and the
    /// machine is never quiescent again, blocking rotation until the
    /// fragment ceiling kills the run. (Cancelled realms were closed
    /// eagerly by the cancel arm.) Returns how many threads were dropped
    /// MID-FLIGHT — Running only; a settled thread was not "dropped" — so
    /// the corrective prompt can say so.
    async fn sweep_green_round(
        &self,
        node: NodeId,
        green: &mut ModelRoundGreenThreadScheduler,
    ) -> usize {
        let mut dropped = 0usize;
        if let Ok(sid) = self.outer_sid() {
            for entry in green.threads.values() {
                match entry.state {
                    GreenThreadState::Running => {
                        let _ = self
                            .agent
                            .with_session_retrying(node, sid, |s| s.close_realm(entry.realm))
                            .await;
                        dropped += 1;
                    }
                    GreenThreadState::Settled(_) => {
                        let _ = self
                            .agent
                            .with_session_retrying(node, sid, |s| s.close_realm(entry.realm))
                            .await;
                    }
                    GreenThreadState::Cancelled => {}
                }
            }
        }
        green.threads.clear();
        green.waiters.clear();
        green.ready.clear();
        dropped
    }

    /// F6: sweep `green`'s still-open thread realms before an early exit out
    /// of [`Self::drive_agent_session_to_finalize`]'s inner round-servicing loop —
    /// that loop's NORMAL exits already sweep (the post-loop code, and the
    /// `ForkBudgetRefused`/`AsyncMisuse` arms' own inline sweeps before their
    /// `continue 'round`), but a `?`-propagated mechanism error from any of
    /// `drain_note_holes`/`service_askuser_hole`/`drain_answerer_fork`/
    /// `service_green_round` used to skip straight past all of them,
    /// leaking every thread realm the round had open. Wrapping the loop
    /// itself in a `?`-catching scope (the shape `run_loop_fragment_inner`'s
    /// own sweep uses) does not fit here: several arms `continue 'round` — a
    /// jump to the OUTER round loop — which cannot cross an intervening
    /// async-block boundary, so each unswept `?`/`return` site is wrapped
    /// individually instead. A no-op when `result` is `Ok` or `green` is
    /// still `None` (no threads were ever spawned this round).
    async fn sweep_green_on_err<T>(
        &self,
        node: NodeId,
        green: &mut Option<ModelRoundGreenThreadScheduler>,
        result: Result<T, DriverError>,
    ) -> Result<T, DriverError> {
        if result.is_err() {
            if let Some(g) = green.as_mut() {
                self.sweep_green_round(node, g).await;
            }
        }
        result
    }

    /// Evaluate `render(state)` against the outer session, then compose the
    /// full system message the answerer works under — author output first,
    /// then the prior compaction summary (if any), then the loop-iteration
    /// count. This composed text becomes `prompt_before`/`prompt_after`; it
    /// carries NO effects section of its own — the OUTER loop's own
    /// Available-effects section (folded over [`outer_decls`]) is never
    /// shown to the nested answerer, which sees only its own row's section
    /// (appended by the caller that builds `self.answerer_framing`, via
    /// [`typed_request_agent_framing_suffix`]). `render` itself takes only `State`
    /// (`plans/self-iterating-harness/15-generic-surface-wave.md`, "Runtime
    /// context is the runtime's job") — the compaction summary and the
    /// iteration count are runtime facts the AUTHOR no longer states.
    /// Runtime-invoked at loop boundaries ONLY (02-runtime.md LOCKED).
    /// `state_json` is `None` only for the very first cycle — then the render
    /// splice references `Loaded.initialState` directly (no JSON to decode),
    /// per [`state_cross::state_in`]. `last_compaction` is the
    /// runtime-carried summary to compose in (`self.last_compaction`, not
    /// itself decoded from any Haskell splice); `self.iteration` supplies the
    /// loop count.
    pub fn render_framing(
        &mut self,
        state_json: Option<&Json>,
        last_compaction: Option<&str>,
    ) -> Result<String, DriverError> {
        let state_decl = state_cross::state_in(state_json);
        let code = format!(
            "pure ({q}.render __selfHarnessState)",
            q = state_cross::LOADED_QUALIFIER
        );
        let compiled = self.compile_outer(&code, &state_decl, "render")?;
        self.render_framing_with(&compiled, last_compaction)
    }

    /// The shared run-and-compose tail of [`Self::render_framing`]: run an
    /// ALREADY-COMPILED `render` entry against the outer session, then
    /// compose the prior compaction summary and the loop-iteration count onto
    /// its `Text` result. Split out so [`Self::compile_loop_entry`]'s fused
    /// render entry runs through the exact same compose logic
    /// [`Self::render_framing`] uses standalone, rather than a second copy.
    fn render_framing_with(
        &mut self,
        compiled: &CompiledTurn,
        last_compaction: Option<&str>,
    ) -> Result<String, DriverError> {
        let sid = self.outer_sid()?;
        let outcome = self
            .agent
            .with_session(sid, |s| s.run("render", &compiled.expr, &compiled.table))
            .map_err(|e| DriverError::Session(e.to_string()))?
            .map_err(|e| map_run_error("render run failed", e.to_string()))?;
        let author_text = match outcome {
            ResidentOutcome::Completed { result, .. } => match result.to_json() {
                Json::String(s) => s,
                other => {
                    return Err(DriverError::Session(format!(
                        "render did not yield Text, got {other:?}"
                    )))
                }
            },
            ResidentOutcome::Suspended { .. } => {
                return Err(DriverError::Session(
                    "render suspended unexpectedly — render must be a pure function".into(),
                ))
            }
        };

        let mut framing = author_text;
        if let Some(summary) = last_compaction {
            framing.push_str("\n\nSummary of the prior context (compacted):\n");
            framing.push_str(summary);
        }
        framing.push_str(&format!("\n\nLoop count so far: {}.", self.iteration));
        if let Some(msg) = self.pending_operator_input.take() {
            framing.push_str("\n\nTHE OPERATOR SAID (between loops, addressed to you): ");
            framing.push_str(&msg);
        }
        if let Some(lost) = self.last_rotation_losses.take() {
            framing.push_str(
                "\n\nNOTE: the resident machine was rotated (bounded-lifetime \
                 maintenance). Durable state survived via the checkpoint; living \
                 session values did NOT: ",
            );
            framing.push_str(&if lost.is_empty() {
                "(none were held)".to_string()
            } else {
                lost.join(", ")
            });
        }
        Ok(framing)
    }

    /// Runtime-owned MID-LOOP emergency compaction with IN-PLACE relief: the
    /// *runtime* owns this trigger, never the loop, and it replaces the
    /// answerer's context with the summary so the loop CONTINUES — never a
    /// loop-abort. Called between the answerer's holes
    /// ([`Self::run_loop_fragment_inner`]).
    ///
    /// Watches the CURRENT loop's answerer session's REAL context size —
    /// [`Harness::node_last_input_tokens`], the LAST turn's `input_tokens`
    /// high-water mark (NOT [`Harness::node_usage`]'s summed
    /// `input_tokens`, which super-linearly over-counts across a multi-round
    /// hole because each round's provider `input_tokens` already re-includes
    /// the whole re-sent transcript) — against the threshold
    /// (`self.compaction_threshold_percent` of [`EngineConfig::context_window_tokens`],
    /// default ~80%). Under threshold, or with the budget disabled (`None`) or
    /// no live answerer, it is a no-op.
    ///
    /// Past threshold it (the SIMPLE mechanism — no separate node, no
    /// `finalize`, no transcript serialized into a prompt; the answerer already
    /// HAS the full context):
    /// 1. Pushes ONE plain "summarize everything above" turn onto the EXISTING
    ///    answerer session and captures the model's prose reply
    ///    ([`Harness::summarize_turn`]) — that model call counts against the
    ///    per-loop [`LOOP_INFERENCE_CALL_CAP`].
    /// 2. Replaces the answerer's context with `[system + summary]` IN PLACE
    ///    ([`Harness::replace_transcript_with_summary`]) — the loop's remaining
    ///    holes continue under the smaller window.
    /// 3. Records the summary as `self.cycle_compaction` (this cycle's, for
    ///    [`LoopIterationOutcome::compaction`]) and `self.last_compaction` (carried to
    ///    the NEXT [`Self::render_framing`] call to compose in, and
    ///    persisted for restart durability).
    /// 4. Emits [`Event::CompactionTrigger`] with its payload (summary, pre/post
    ///    context size, node).
    async fn maybe_compact_answerer(&mut self) -> Result<(), DriverError> {
        let Some(budget) = self.agent.cfg().context_window_tokens else {
            return Ok(());
        };
        let Some(lease) = self.answerer else {
            return Ok(());
        };
        let answerer = lease.node();
        let Some(context_tokens) = self.agent.node_last_input_tokens(answerer) else {
            return Ok(());
        };
        let threshold = (u64::from(budget) * self.compaction_threshold_percent) / 100;
        if context_tokens < threshold {
            return Ok(());
        }

        self.lifecycle = SelfHarnessState::Compacting;

        // The summarize turn is a real model call — count it against the
        // per-loop inference cap before driving it, exactly like an answerer
        // round, so compaction can never escape the 1024-call runaway guard.
        let cap = self.loop_inference_call_cap;
        if self.loop_inference_calls.load(Ordering::SeqCst) >= cap {
            return Err(DriverError::Session(format!(
                "per-loop inference-call cap ({cap}) reached during \
                 compaction — hard-stopping the loop (a runaway harness)"
            )));
        }
        self.loop_inference_calls.fetch_add(1, Ordering::SeqCst);

        // The target is a fraction of the budget, but clamp it against the
        // REAL current window (`context_tokens`) so a summary is never asked to
        // GROW context — under a low test threshold (or a tiny window) the flat
        // budget/DIVISOR could exceed what is actually there. Target strictly
        // below the current size keeps compaction a genuine reduction.
        let budget_target = u64::from(budget / COMPACTION_TARGET_DIVISOR);
        let target = budget_target.min(context_tokens.saturating_sub(1)).max(1);
        let prompt = format!(
            "This conversation has grown to roughly {context_tokens} tokens against a \
             {budget}-token context-window budget — it is time to compact before \
             continuing. Summarize EVERYTHING above (the whole conversation so far) \
             into a compact form you can continue from: a prose summary of what this \
             loop's work has accomplished and learned, targeting roughly {target} \
             tokens, preserving the load-bearing facts and decisions. Your reply will \
             REPLACE the detailed transcript above, so write it as the context you \
             will carry forward. Reply with the summary text directly (no code block)."
        );

        // The SIMPLE mechanism: one ordinary turn on the EXISTING answerer
        // session, which already holds the full context — no second node, no
        // `finalize`, no hand-serialized transcript. The model summarizes
        // itself, so it cannot confabulate.
        let (summary, post_usage) = self.agent.summarize_turn(answerer, &prompt).await?;
        let summary = summary.trim().to_string();

        // In-place relief: the answerer's context becomes `[system + summary]`,
        // so its remaining holes drive under the smaller window (loop CONTINUES).
        self.agent
            .replace_transcript_with_summary(answerer, &summary)?;

        // The trigger event carries what compaction produced — the
        // summary, the pre/post context size, and the node it fired on.
        self.emit(Event::CompactionTrigger {
            node: answerer,
            summary: summary.clone(),
            pre_input_tokens: context_tokens,
            post_input_tokens: post_usage.input_tokens,
        });

        self.cycle_compaction = Some(summary.clone());
        self.set_last_compaction(summary)?;
        self.lifecycle = SelfHarnessState::RunningLoop;
        Ok(())
    }

    /// Record `summary` as the latest compaction (`self.last_compaction`,
    /// composed into the next [`Self::render_framing`] call) — in-memory
    /// only. The loop
    /// CONTINUES under this summary immediately, but it does not reach disk
    /// on its own: [`Self::commit_checkpoint`] picks up whatever
    /// `self.last_compaction` holds at the cycle's own commit boundary, so a
    /// crash between a mid-loop compaction and that commit restores the
    /// PRIOR generation's summary, never a summary paired with a state it
    /// was never produced alongside.
    fn set_last_compaction(&mut self, summary: String) -> Result<(), DriverError> {
        self.last_compaction = Some(summary);
        Ok(())
    }

    /// Emit `event` to the configured [`Observer`] — the ONE place the
    /// driver touches the observer, so no call site hardwires logging or a
    /// future GUI push directly.
    fn emit(&self, event: Event) {
        self.observer.on_event(&event);
    }
}

#[cfg(test)]
mod tests {
    use super::{outer_decls, typed_request_agent_decls};

    /// The outer row's handled prefix must be EMPTY — every effect (including
    /// `Subagent`/`Worktree`) SUSPENDS to the driver. A reorder that puts a
    /// handled effect before `RunLLMTurn` would give the SHARED machine a
    /// non-empty established prefix and silently dispatch the answerer
    /// realms' `AskUser`/`Fork` into handler slots (see [`outer_decls`]).
    /// Decl-name-position is the whole mechanism, so this pin is pure.
    #[test]
    fn outer_row_suspends_everything() {
        let decls = outer_decls();
        assert_eq!(decls[0].type_name, "RunLLMTurn", "interposed first");
        let first_interposed = decls
            .iter()
            .position(|d| {
                matches!(
                    d.type_name,
                    "Ask" | "AskUser" | "RunLLMTurn" | "Fork" | "Finalize"
                )
            })
            .expect("outer row has an interposed effect");
        assert_eq!(
            first_interposed, 0,
            "the suspend threshold must be 0 — a non-empty handled prefix on the outer \
             session breaks the shared machine's answerer realms"
        );
        // The Subagent lane's hard companion rides along.
        assert!(decls.iter().any(|d| d.type_name == "Worktree"));
        assert!(decls.iter().any(|d| d.type_name == "Subagent"));
        // S1-L1: Console/RepoEvent/Exec join the widened outer row.
        assert!(decls.iter().any(|d| d.type_name == "Console"));
        assert!(decls.iter().any(|d| d.type_name == "RepoEvent"));
        assert!(decls.iter().any(|d| d.type_name == "Exec"));
        // Run-journal lane: Journal joins the widened outer row.
        assert!(decls.iter().any(|d| d.type_name == "Journal"));
    }

    /// The answerer's generated `Tidepool.Effects` module declares the
    /// `AskUser` GADT + `askUserRaw` helper
    /// — and does NOT declare `Ask`/`ask`/`dialogAsk` (a DIFFERENT effect,
    /// deliberately absent from `typed_request_agent_decls()`, and `dialogAsk` is
    /// deleted outright). Pure string-level check, no GHC needed.
    #[test]
    fn answerer_effects_module_declares_askuser_not_ask() {
        let src = tidepool_mcp::effects_core_module_source(&typed_request_agent_decls());
        assert!(
            src.contains("data AskUser a where"),
            "expected an AskUser GADT declaration, got:\n{src}"
        );
        assert!(
            src.contains("askUserRaw"),
            "expected the askUserRaw helper, got:\n{src}"
        );
        assert!(
            !src.contains("data Ask a where"),
            "the answerer stack must NOT declare the Ask GADT, got:\n{src}"
        );
        assert!(
            !src.contains("dialogAsk"),
            "dialogAsk is deleted and must not appear anywhere, got:\n{src}"
        );
    }

    /// The answerer's system framing names every verb of its ACTUAL
    /// compiling row (`typed_request_agent_decls()` — `AskUser`/`Fork`/`Finalize`) via
    /// the decl-driven fold (`engine::available_effects_section`), not a
    /// hand-written parenthetical. Pure string check, no GHC needed.
    #[test]
    fn typed_request_agent_framing_suffix_names_every_verb_of_the_answerer_row() {
        let framing = super::typed_request_agent_framing_suffix(
            &typed_request_agent_decls(),
            super::DEFAULT_FORK_BUDGET_PER_SESSION,
            super::DEFAULT_FORK_SUBTREE_CAP,
        );
        for decl in typed_request_agent_decls() {
            assert!(
                framing.contains(decl.type_name),
                "answerer framing missing {} — got:\n{framing}",
                decl.type_name
            );
        }
        assert!(
            framing.contains("Available effects"),
            "expected the generated section marker, got:\n{framing}"
        );
        // The concurrency teaching: the configured budget number is VISIBLE
        // up front (not just discovered by refusal), and the composed
        // example's do-block survived Rust line-continuation whitespace
        // stripping with its indentation intact (a copied unindented example
        // is a GHC parse error in the model's hands).
        assert!(
            framing.contains(&format!(
                "at most {} fork children",
                super::DEFAULT_FORK_BUDGET_PER_SESSION
            )),
            "the fork budget must be stated in the framing, got:\n{framing}"
        );
        // The tree-wide descendant budget is stated too — not left to be
        // discovered only by a refusal (F10, prompt-surface review).
        assert!(
            framing.contains(&format!(
                "descendant budget of {} across all depths",
                super::DEFAULT_FORK_SUBTREE_CAP
            )),
            "the tree-wide fork subtree cap must be stated in the framing, got:\n{framing}"
        );
        assert!(
            framing.contains("\n  ha <- async (fork @Plan"),
            "the composed example must keep its do-block indentation, got:\n{framing}"
        );
    }

    /// The framing folds whatever row it is HANDED, not a hardcoded plain
    /// roster — passing the delegating row
    /// ([`typed_request_agent_decls_with_delegate`], `Subagent`/`Worktree`
    /// prepended) must surface those two effects' cards, which the plain
    /// row never carries. This is what makes the delegating path's own
    /// framing (built from `self.agent.cfg().decls` in
    /// [`SelfHarnessDriver::run_one_loop_iteration`]) stop omitting Subagent
    /// once that config is the delegating one.
    #[test]
    fn typed_request_agent_framing_suffix_reflects_the_passed_row_not_a_hardcoded_one() {
        let plain = super::typed_request_agent_framing_suffix(
            &typed_request_agent_decls(),
            super::DEFAULT_FORK_BUDGET_PER_SESSION,
            super::DEFAULT_FORK_SUBTREE_CAP,
        );
        assert!(
            !plain.contains("**Subagent**") && !plain.contains("**Worktree**"),
            "the plain row's framing must not mention Subagent/Worktree, got:\n{plain}"
        );
        let delegating = super::typed_request_agent_framing_suffix(
            &super::typed_request_agent_decls_with_delegate(),
            super::DEFAULT_FORK_BUDGET_PER_SESSION,
            super::DEFAULT_FORK_SUBTREE_CAP,
        );
        assert!(
            delegating.contains("**Subagent**") && delegating.contains("**Worktree**"),
            "the delegating row's framing must name Subagent/Worktree since \
             they are genuinely in its configured row, got:\n{delegating}"
        );
    }

    /// A `Not in scope: fork`/`forkAll` GHC diagnostic — the exact shape a
    /// model hits when it reaches for `fork`/`forkAll` without importing
    /// `Tidepool.Fork` — must be recognized so the corrective-retry loop can
    /// name the fix, and the internal head-swap targets `forkSited`/
    /// `forkAllSited` must NOT trip the same check (a model should never be
    /// naming those directly, and a false-positive hint there would be
    /// confusing advice).
    #[test]
    fn error_names_unimported_fork_recognizes_fork_and_forkall_not_forksited() {
        assert!(super::SelfHarnessDriver::error_names_unimported_fork(
            "error: Variable not in scope: fork"
        ));
        assert!(super::SelfHarnessDriver::error_names_unimported_fork(
            "error: Variable not in scope: forkAll :: [Text] -> M [a]"
        ));
        assert!(!super::SelfHarnessDriver::error_names_unimported_fork(
            "error: Variable not in scope: forkSited"
        ));
        assert!(!super::SelfHarnessDriver::error_names_unimported_fork(
            "error: Variable not in scope: forkAllSited"
        ));
        assert!(!super::SelfHarnessDriver::error_names_unimported_fork(
            "error: Couldn't match expected type 'Int' with actual type 'Text'"
        ));
    }

    /// The outer loop's own decl row (`outer_decls()` — `RunLLMTurn`/
    /// `AskUser`) is a DIFFERENT row from the answerer's
    /// (`AskUser`/`Fork`/`Finalize`), so the two surfaces' generated
    /// sections must differ — each folds over its own compiling row, not a
    /// shared hand-written table.
    #[test]
    fn outer_and_answerer_available_effects_sections_differ_by_row() {
        let outer_section = crate::engine::available_effects_section(&super::outer_decls());
        let answerer_section =
            crate::engine::available_effects_section(&typed_request_agent_decls());

        assert!(outer_section.contains("**RunLLMTurn**"));
        assert!(!answerer_section.contains("**RunLLMTurn**"));
        assert!(outer_section.contains("**Console**"));
        assert!(!answerer_section.contains("**Console**"));
        assert!(!outer_section.contains("**Fork**"));
        assert!(answerer_section.contains("**Fork**"));
        assert_ne!(outer_section, answerer_section);
    }

    /// Every outer fragment compile (`compile_outer`, via `outer_template`)
    /// renders WITHOUT the `paginateResult` result wrapper — see
    /// `outer_template`'s doc for the production failure this pins: the
    /// paginated wrapper's oversized branch calls `putStrLn` on the outer
    /// row's Console, which suspends, which the post-loop render's purity
    /// refusal turns into a cycle-discarding crash loop the first time a
    /// rendered framing exceeds 4096 bytes. Pure string check, no GHC.
    #[test]
    fn outer_template_is_unpaginated() {
        let src = super::outer_template(
            "'[RunLLMTurn, AskUser, Console, Finalize Void]",
            "pure (Loaded.render __selfHarnessState)",
            "qualified Harness as Loaded",
            "",
        );
        // The preamble always DECLARES the `paginateResult` alias; what must
        // not appear is a CALL routing the result through it.
        assert!(
            !src.contains("paginateResult 4096"),
            "outer fragments must not route their result through paginateResult — \
             its oversized branch suspends on Console, and truncation corrupts \
             driver-consumed JSON. Got:\n{src}"
        );
        assert!(
            src.contains("pure (toJSON _r)"),
            "expected the unpaginated result binding, got:\n{src}"
        );
    }

    /// H1 (test-architecture review, 2026-08-23): `fork_child_path_segment`
    /// slugs a MODEL-AUTHORED fork brief into a GUI/DOM node id segment —
    /// `tidepool-web`'s loopback trust model rests on "`node_id` is always a
    /// substrate identifier, never model-produced text"
    /// (`tidepool-web/src/lib.rs` interpolates `id="panel-<node_id>"` into
    /// the DOM). Recovers the deleted `row_11_node_ids_are_containment_safe`'s
    /// hostile corpus (git show
    /// `bdfc334a^:tidepool-harness/tests/companion_recursive_slice.rs`,
    /// `HOSTILE_TITLE`/`HOSTILE_FRAGMENTS`) as a pure, zero-GHC pin directly
    /// on the slugging function, in place of the deleted full companion-tree
    /// run.
    #[test]
    fn fork_child_path_segment_contains_hostile_briefs() {
        const HOSTILE_TITLE: &str = "Beta!! <b>risk</b> ünïcode";
        const HOSTILE_FRAGMENTS: [&str; 5] = ["!", "<", ">", "/b", "ü"];
        let long_brief = "x".repeat(super::FORK_LABEL_SLUG_BUDGET * 2);

        let cases: &[(&str, &[&str])] = &[
            (HOSTILE_TITLE, &HOSTILE_FRAGMENTS),
            ("a/b/c", &["/"]),
            ("say \"hi\"", &["\""]),
            ("   ", &[]),
            ("!!!@@@###", &[]),
            ("", &[]),
            (long_brief.as_str(), &[]),
        ];

        for (i, (brief, hostile_fragments)) in cases.iter().enumerate() {
            let idx = i as u32;
            let segment = super::fork_child_path_segment(idx, brief);

            // (1) shape: `f<idx>` or `f<idx>-<slug>`, slug ⊆ [a-z0-9-], within budget.
            let prefix = format!("f{idx}");
            let rest = segment
                .strip_prefix(prefix.as_str())
                .unwrap_or_else(|| panic!("segment {segment:?} must start with {prefix:?}"));
            if !rest.is_empty() {
                let slug = rest.strip_prefix('-').unwrap_or_else(|| {
                    panic!("segment {segment:?} must join its slug to the index with '-'")
                });
                assert!(
                    !slug.is_empty() && slug.len() <= super::FORK_LABEL_SLUG_BUDGET,
                    "slug {slug:?} (from brief {brief:?}) must be 1..={} chars, got segment \
                     {segment:?}",
                    super::FORK_LABEL_SLUG_BUDGET
                );
                assert!(
                    slug.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                    "slug {slug:?} (from brief {brief:?}) must be drawn from [a-z0-9-] only, \
                     got segment {segment:?}"
                );
                // (4) no leading/trailing/double hyphen.
                assert!(
                    !slug.starts_with('-') && !slug.ends_with('-') && !slug.contains("--"),
                    "slug {slug:?} (from brief {brief:?}) must have no leading/trailing/double \
                     hyphen, got segment {segment:?}"
                );
            }

            // (2) none of the hostile fragments survive.
            for fragment in *hostile_fragments {
                assert!(
                    !segment.contains(fragment),
                    "hostile fragment {fragment:?} from brief {brief:?} reached node id \
                     segment {segment:?}"
                );
            }
        }

        // (3) the empty/all-symbol/whitespace-only case yields the bare
        // `f<idx>` fallback — never an empty or bare-hyphen segment.
        assert_eq!(super::fork_child_path_segment(2, ""), "f2");
        assert_eq!(super::fork_child_path_segment(3, "!!!@@@###"), "f3");
        assert_eq!(super::fork_child_path_segment(4, "   "), "f4");
    }
}
