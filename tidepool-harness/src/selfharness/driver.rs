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
//! [`SelfHarnessDriver::run_loop`]/[`SelfHarnessDriver::run_one_cycle`] are
//! `async fn` and `.await` the nested [`Harness`]'s turn loop
//! ([`service_runllm_hole`](SelfHarnessDriver::service_runllm_hole)) directly
//! — every entry point here must still be called from a thread with an
//! ACTIVE tokio runtime (`#[tokio::main]`/`#[tokio::test(flavor =
//! "multi_thread")]`), because the [`crate::selfharness::operator::OperatorGate`]
//! park (`present_form`/`await_continue`) is SYNC-BLOCKING by frozen contract
//! (a web gate parks a channel), so a call into it from this async code runs
//! under `tokio::task::block_in_place` — a genuinely blocking call yielding
//! the tokio worker to other tasks, not a sync-to-async bridge — which
//! requires the multi-thread runtime flavor. The resident JIT run/resume
//! calls the loop also drives are CPU-blocking and sit inside these `async
//! fn`s unchanged (they already blocked a tokio worker before this
//! conversion); see [`Self::drive_answerer_to_finalize`]'s doc for why they
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
    self, ClassifiedHole, CompiledTurn, EngineConfig, EngineError, HoleRouting, InvocationExit,
    TurnOutcome,
};
use crate::harness::{AnswerContract, ContextRef, Harness, HarnessError, OUTER_REALM};
use crate::log::Actor;
use crate::selfharness::harness_source::HarnessSource;
use crate::selfharness::lifecycle::SelfHarnessState;
use crate::selfharness::observer::{AskId, Event, FormSource, Observer};
use crate::selfharness::operator::{FormShape, OperatorGate, StdinGate};
use crate::selfharness::persistence::{self, PersistenceError};
use crate::selfharness::state_cross;
use crate::snapshot::SnapshotDigest;
use crate::timing;
use crate::tree::{FanBadge, NodeId};

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
/// cycle's outcome — [`SelfHarnessDriver::run_one_cycle`]'s return value,
/// what a spine test asserts against. `state_json` is what the caller
/// persists and threads into the NEXT cycle's `prior_state`.
#[derive(Debug, Clone)]
pub struct CycleOutcome {
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
    /// [`SelfHarnessDriver::run_one_cycle`] carries forward as the NEXT
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
/// [`HoleRouting::Green`] is the deliberate exception, checked and rejected
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
enum ServicedHole {
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
    fn any_sentinel(v: &Value) -> bool {
        match v {
            Value::Con(id, fields) => {
                (id.0 == u64::MAX && fields.is_empty()) || fields.iter().any(any_sentinel)
            }
            _ => false,
        }
    }
    matches!(request, Value::Con(_, fields) if fields.get(idx).is_some_and(any_sentinel))
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

struct OuterSession {
    /// The SHARED session's registry id (one-session collapse): the outer
    /// render/loop fragments AND every answerer node's turns run on this one
    /// machine — the driver holds the id, the registry holds the machine,
    /// every access goes through the checkout discipline
    /// ([`crate::harness::Harness::with_session`]).
    sid: tidepool_repr::SessionId,
    cfg: EngineConfig,
    module_name: String,
    /// The author modules every answerer turn imports
    /// ([`HarnessSource::answerer_imports`]), so the hole's answer type is in
    /// scope and resolves to the SAME defining module the outer loop used.
    answerer_imports: Vec<String>,
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
/// [`HoleRouting`]), not the `OuterEffectKind`/`dispatch_outer_effect` class.
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
        .push(delegate_branches_decl())
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

/// PRD 21 C5's read-back half: `takeDelegatedBranches path` lets the
/// AUTHORED outer loop (`Harness.hs`'s `foldAt`) consume the runtime-stamped
/// branch(es) a node's own coalgebra delegation produced, keyed by that
/// node's rendered `NodePath` text. Hand-built as an `EffectDecl` literal
/// here rather than via a `tidepool-mcp` `*_effect_def!` macro: this verb has
/// no Rust-registry `<Eff>Req`/`EffectHandler` at all (see
/// [`engine::HoleRouting::DelegatedBranches`]'s doc) — it is serviced
/// entirely inline by [`SelfHarnessDriver::service_delegated_branches`],
/// exactly like `ReadState`/`FreezeContext`.
///
/// NEVER pushed into [`answerer_decls`]/[`answerer_decls_with_delegate`] —
/// that omission is what makes this verb structurally unreachable from any
/// model-authored block, the mechanism behind "the model never attests to
/// its own execution" (locked decision 7) applied to delegation outcomes:
/// there is no row a model's block could ever compile against that names
/// `DelegateBranches`.
///
/// "Take": the driver's own record for `path` is CONSUMED (removed) on
/// read — sound because a node's own `foldAt` runs exactly once. Ordered
/// oldest-first; `Harness.hs` decides "last wins" for `answerMergeBranch`
/// when a node delegated more than once, and journals when it did (PRD 21
/// C5's multiple-delegation policy — see `Harness.hs`'s `foldAt`).
///
/// `pub`, unlike [`outer_decls`] itself, for exactly one reason:
/// `tests/dogfood_harness_typecheck.rs`'s `outer_row_decls()` hand-mirrors
/// `outer_decls()`'s CONTENTS (that file's own doc: "so no two of them can
/// compile a harness against different rows") because `outer_decls` itself
/// is private — every entry there is already a public `tidepool_mcp::
/// *_decl()` call except this one, so this one needs to be reachable the
/// same way rather than a second, drift-prone copy of its GADT/helper text.
pub fn delegate_branches_decl() -> tidepool_mcp::EffectDecl {
    tidepool_mcp::EffectDecl {
        type_name: "DelegateBranches",
        description: "Internal runtime channel (PRD 21 C5): the driver's own \
            record of completed per-node delegation branches. Never reachable \
            from a model-authored block.",
        prompt_card: None,
        constructors: &["TakeDelegatedBranchesWith :: Text -> DelegateBranches [Text]"],
        type_defs: &[],
        extra_imports: &[],
        helpers: &[
            "takeDelegatedBranches :: forall effs. Member DelegateBranches effs => Text -> Eff effs [Text]",
            "takeDelegatedBranches p = send (TakeDelegatedBranchesWith p)",
        ],
        type_params: &[],
        default_row_args: &[],
        // Stable-effects-core: every vocabulary effect's helpers must be
        // row-polymorphic to live in the stable `Tidepool.Effects.Core`
        // module (it has no `M` alias of its own — see
        // `tidepool_mcp::effects_core_module_source`'s assertion). This
        // effect is never model-reachable (see the doc above), so the
        // signature's shape is otherwise inert — flipped for uniformity with
        // every other effect definition, not because anything NEW depends on
        // it being polymorphic.
        helpers_row_polymorphic: true,
    }
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

/// The nested answerer Agent's scoped decl row: `[AskUser, Fork, ReadState, Finalize]`.
/// It declares no base effects (`Console`/`KV`/`Fs`/`Lsp`/`Http`/`Exec`/`Git`/
/// `Time`/`Meta`) and no `RunLLMTurn`/`Ask`, so an answerer turn compiles
/// against a `Tidepool.Effects` that never defines those verbs — the answerer
/// structurally cannot run a shell command, read files, hit the network, or
/// suspend an in-context `runLLMTurn`. Its whole surface: `askUser` (present a
/// typed form to a human operator, riding `AskUser`), `fork`/`forkAll`
/// (delegate to bounded parallel sub-answerers, riding `Fork` — the driver
/// services the resulting suspension via [`Harness::answer_fanout`]/
/// [`Harness::answer_fork`], and each child compiles against a fork-free leaf
/// row so it cannot itself fork), and `finalize` (the answer path).
///
/// `AskUser` comes first because [`EngineConfig::from_decls`] takes the first
/// interposed effect as the suspend threshold; `Fork`/`Finalize` land at or
/// past it regardless of position.
pub fn answerer_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![
        tidepool_mcp::askuser_decl(),
        tidepool_mcp::fork_decl(),
        tidepool_mcp::readstate_decl(),
        tidepool_mcp::finalize_decl(),
    ]
}

/// [`answerer_decls`] with `Subagent` and `Worktree` PREPENDED, in that
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
/// Does NOT widen `answerer_decls()` itself — every other harness (dev-tree,
/// the general Agent stack) keeps compiling exactly as before.
pub fn answerer_decls_with_delegate() -> Vec<tidepool_mcp::EffectDecl> {
    let mut decls = vec![tidepool_mcp::subagent_decl(), tidepool_mcp::worktree_decl()];
    decls.extend(answerer_decls());
    decls
}

fn not_bootstrapped() -> DriverError {
    DriverError::Session("outer session not bootstrapped (call run_loop/run_one_cycle)".into())
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
const ANSWERER_NUDGE_ROUNDS: u32 = 16;

/// Per-hole HARD cap: after this many non-finalize model rounds on one hole,
/// hard-fail the `runLLMTurn` effect with a [`DriverError`].
const ANSWERER_MAX_ROUNDS: u32 = 32;

/// Cap on CONSECUTIVE `askUser` re-presentations within the servicing of ONE
/// hole: `askUser` re-prompts by RECURSION on a decode failure — no
/// `Either` — and the frozen headless `StdinGate::present_form` returns an EMPTY
/// JSON object on EOF rather than erroring, so a non-interactive gate with
/// closed stdin composes into an unbounded hot loop that NEITHER
/// `ANSWERER_MAX_ROUNDS` nor `LOOP_INFERENCE_CALL_CAP` catches (both only
/// count `drive_turn` model rounds, and a form resume deliberately does not
/// count as one). This counter is a SEPARATE, independent budget: it
/// increments each time the answerer re-suspends on another `AskUser` hole
/// without making progress, and resets the moment a resume yields anything
/// else (a `Finalize` suspension, a plain completion, a compile error to
/// correct). Past the cap, [`SelfHarnessDriver::drive_answerer_to_finalize`]
/// hard-fails the hole with a [`DriverError::Session`] naming the cause,
/// rather than spinning at full CPU. 8 leaves ample room for genuine operator
/// typos while making a broken/closed gate terminate loudly and fast.
const ASKUSER_MAX_REPROMPTS: u32 = 8;

/// Per-LOOP hard cap on TOTAL model inference calls across every hole + round.
/// Keeps a misbehaving harness from running away regardless of per-hole
/// budgets or compaction.
const LOOP_INFERENCE_CALL_CAP: u32 = 1024;

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

/// The narrow answerer instruction appended after `render`'s output to form
/// the per-loop answerer session's system message. Scoped to the answerer's
/// surface — `askUser`, `fork`/`forkAll`, `finalize` — not the full eval
/// surface [`crate::engine::SYSTEM_FRAMING`] advertises. This is
/// belt-and-braces, not the enforcement mechanism: the scoped stack
/// ([`answerer_decls`]) is what makes any verb this framing omits fail to
/// compile.
///
/// The per-verb signatures/examples are NOT hand-narrated here: they fold
/// over [`answerer_decls`] via [`engine::available_effects_section`] — the
/// same [`tidepool_mcp::EffectDecl::prompt_card`]/`description` single
/// source the eval tool description is assembled from — so a row with a
/// different effect set gets a correspondingly different cheatsheet, sent
/// ONCE per loop in the system framing rather than re-narrated every hole.
fn answerer_framing_suffix() -> String {
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
         bind with `x <- …` persists into your NEXT turn like GHCi, so you can \
         branch on it.\n\
         \n\
         YOUR WINDOW'S MECHANICS: you have up to {} model rounds in this cognition \
         window before you must finalize (a reminder arrives at round {}). Rounds \
         accumulate: bindings and `let` helpers from earlier rounds stay in scope. \
         A block that is ONLY top-level declarations (type signatures, function \
         definitions, data types) is a DEFINE block — those declarations go onto \
         your session's decl plane and persist BEYOND this window, across every \
         future one: your growing library. Define what you will want again.\n\
         \n\
         THE OPERATOR CANNOT INITIATE: they see your notes and the forms you \
         present, and between windows they may attach a message that arrives in \
         your framing. If you want their input NOW, present a form (`askUser`/\
         `choose`); silence from them mid-window is structural, not meaningful.\n\
         \n\
         {}\n\
         \n\
         Your block is a PROGRAM, not a single question: sequence several \
         consultations in one `do` block and branch on earlier answers with \
         ordinary `case`/`if` — each runs without another model round. Plan \
         the whole consultation up front when the branches are predictable; \
         end the turn without finalizing only when an answer genuinely needs \
         fresh judgment. Bind results, then `finalize`.\n\
         \n\
         When you have the answer, COMMIT it by evaluating `finalize @T value`. This \
         ends your turn and hands the typed value back to the loop. `T` is the type \
         named in the request. Do not call any other effect to answer; `finalize` is \
         how you resolve the request.",
        ANSWERER_MAX_ROUNDS,
        ANSWERER_NUDGE_ROUNDS,
        engine::available_effects_section(&answerer_decls())
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
    /// The operator's between-loops message ([`ContinueSignal::ContinueWithInput`]),
    /// threaded into the NEXT cognition window's framing as their utterance,
    /// then cleared. Their one channel for initiating.
    pending_operator_input: Option<String>,
    /// The current cycle's ENTRY state (what `getStateJson` serves): durable
    /// state as of the window's start — this window's edit and operator
    /// ingestion are deliberately not in it (documented semantics of the
    /// ReadState effect). `None` on the very first cycle (initialState —
    /// served as JSON `null`, which the authored `getStateJson` docs cover).
    cycle_state_json: Option<Json>,
    /// The nested multi-node orchestrator that answers a `runLLMTurn` hole
    /// by driving an Agent turn loop (`run_to_hole_or_done`) to a
    /// `finalize`. Shared, not owned exclusively, so a future GUI/inspector
    /// can observe the same node tree.
    agent: Arc<Harness>,
    lifecycle: SelfHarnessState,
    observer: Arc<dyn Observer>,
    /// The LATEST emergency-compaction `Text`, fed as the NEXT
    /// [`Self::run_one_cycle`] call's `lastCompaction` — driver-owned state
    /// rather than a threaded parameter, since the *runtime* (not the
    /// caller) owns the compaction lifecycle. Updated MID-LOOP by
    /// [`Self::maybe_compact_answerer`] the moment a
    /// compaction fires (the loop then CONTINUES under the summary). `None`
    /// until the first compaction fires.
    last_compaction: Option<String>,
    /// The compaction `Text` produced DURING the current cycle's loop, if one
    /// fired (in-place mid-loop relief) — distinct from
    /// [`Self::last_compaction`], which also carries a PRIOR cycle's summary
    /// forward. Reset (`take`n) into [`CycleOutcome::compaction`] at the end of
    /// [`Self::run_one_cycle`], so a test asserts on THIS cycle's compaction,
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
    /// followed by [`answerer_framing_suffix`]. Set in
    /// [`Self::run_one_cycle`] right after the pre-loop `render`, read when the
    /// answerer session is created. `None` before the first loop's render.
    answerer_framing: Option<String>,
    /// The CURRENT loop's single render-seeded answerer node: created
    /// ONCE per loop in [`Self::run_loop_fragment`], reused for every
    /// `runLLMTurn` hole so hole #2's answerer sees hole #1's exchange (the
    /// accumulating context window — the fused hylo intermediate). Retired
    /// (dropped) at loop end so the next loop gets a fresh render-seeded
    /// session. `None` between loops. Always [`WindowLease::ReusableLoop`]
    /// — see that type's doc for the distinction it exists to enforce.
    answerer: Option<WindowLease>,
    /// Total model inference calls across the CURRENT loop's holes + rounds:
    /// reset in [`Self::run_loop_fragment`], incremented
    /// per answerer `drive_turn`. The loop hard-stops with a [`DriverError`]
    /// if it reaches [`LOOP_INFERENCE_CALL_CAP`]. Atomic (not a plain `u32`)
    /// because concurrent fanout/fork children (S1-L4,
    /// [`Self::service_outer_fanout`]) each increment it from `&self`
    /// alongside the single reused [`Self::answerer`]'s rounds — one shared
    /// budget regardless of how many windows are open at once.
    loop_inference_calls: AtomicU32,
    /// Per-hole soft cap (nudge threshold), default [`ANSWERER_NUDGE_ROUNDS`].
    /// Configurable via [`Self::set_answerer_round_caps`] so a test can trip
    /// the nudge/hard-fail deterministically with a few small scripted turns
    /// instead of the full 16/32 (each round is a real GHC compile).
    answerer_nudge_rounds: u32,
    /// Per-hole hard cap, default [`ANSWERER_MAX_ROUNDS`]. See
    /// [`Self::set_answerer_round_caps`].
    answerer_max_rounds: u32,
    /// Per-loop total inference-call cap, default
    /// [`LOOP_INFERENCE_CALL_CAP`] (1024). Configurable via
    /// [`Self::set_loop_inference_call_cap`] so a test can prove a specific
    /// model call — e.g. the compaction summarize turn — counts
    /// against it with a small cap instead of scripting 1024 real turns.
    loop_inference_call_cap: u32,
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
    /// restored, kept around so [`Self::mark_awaiting_continue`] can flip its
    /// gate-park marker (`Checkpoint::with_awaiting_continue`) WITHOUT
    /// deriving a new generation the way [`Self::commit_checkpoint`] does —
    /// the marker write records a live park, not a newly completed cycle.
    /// `None` before either a commit or a restore has happened, which is
    /// exactly when [`Self::between_loops_gate`] is never reached (see
    /// [`Self::run_loop`]'s `first`-gating).
    last_checkpoint: Option<persistence::Checkpoint>,
    /// The number of loop cycles completed so far — a runtime fact, NOT part
    /// of the authored `State` (`plans/self-iterating-harness/
    /// 15-generic-surface-wave.md`, "Runtime context is the runtime's job").
    /// `0` before any cycle has completed. Incremented once per successful
    /// [`Self::run_one_cycle`], right after that cycle's `loop` completes;
    /// fed into [`Self::render_framing`]'s composed loop-metadata line and
    /// persisted in the checkpoint envelope ([`Self::commit_checkpoint`]) —
    /// never in `state_json` — so a restart resumes counting from the right
    /// number ([`Self::restore`]).
    iteration: u64,
    /// The operator-input seam: the driver blocks on this for `askUser`
    /// form presentation
    /// ([`Self::drive_answerer_to_finalize`]) and the between-loops human
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
    /// `RunLLMTurn`/`AskUser` (which have their own dedicated servicing
    /// paths) — `Console`/`Worktree`/`RepoEvent`/`Exec`/`Subagent`/`Journal`.
    /// Each is DRIVER-owned, never a handler stack on the outer session,
    /// whose handled prefix must stay empty on the shared machine (see
    /// [`outer_decls`]); a suspension against an unwired handler fails
    /// LOUDLY with the wiring instruction, never a hang. Wire one via
    /// [`Self::set_console_handler`]/[`Self::set_worktree_handler`]/
    /// [`Self::set_event_handler`]/[`Self::set_exec_handler`]/
    /// [`Self::set_subagent_handler`]/[`Self::set_journal_handler`].
    /// `Mutex`-wrapped so [`Self::service_outer_subagent`] — reached from a
    /// concurrent `runLLMTurnBranchFanout` sibling's own `delegate` call via
    /// [`Self::drain_note_holes`] — can dispatch through the shared
    /// `SubagentHandler` from `&self`; two siblings delegating in the same
    /// bulk window simply serialize on the dispatch call itself (the same
    /// synchronous handler this driver has always called, never made to run
    /// two dispatches at once).
    handlers: Mutex<OuterHandlers>,
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
    /// PRD 21 C5's final wiring: which recursive-companion `NodePath` (as
    /// rendered text — see [`engine::parse_companion_node_path`]) a given
    /// branch-child window is servicing, populated by
    /// [`Self::service_outer_branch`]/[`Self::drive_branch_fanout_child`]
    /// right when that window's `NodeId` is minted and removed once it
    /// finishes (success, exit, or closure — every path). Lets
    /// [`Self::drain_note_holes`]'s `Subagent` arm — which only ever has the
    /// `NodeId` in scope — attribute a completed delegation to the right
    /// entry in [`Self::delegated_branches`]. `Mutex`-wrapped (not a plain
    /// map behind `&mut self`): concurrent `runLLMTurnBranchFanout` siblings
    /// (`Self::service_outer_branch_fanout`) each insert/remove their own
    /// entry from `&self`, so a bare `HashMap` would need `&mut self` at
    /// exactly the point several siblings are running at once.
    branch_node_paths: Mutex<HashMap<NodeId, String>>,
    /// PRD 21 C5 GUI lane: which per-node operator-gate label a
    /// `runLLMTurnBranchLabeled`/`runLLMTurnBranchFanout` child window
    /// carries — populated right when that window's `NodeId` is minted
    /// (mirrors `branch_node_paths` above) and removed once it finishes
    /// (success, exit, or closure — every path), at which point
    /// [`crate::selfharness::operator::OperatorGate::retire_node`] is called
    /// on the default gate. [`Self::present_askuser_form`]/
    /// [`Self::announce_note`] look a node up here to resolve
    /// [`crate::selfharness::operator::OperatorGate::node_gate`] instead of
    /// the default gate; a node absent here (every unlabeled node, including
    /// the outer loop's own asks and the ordinary per-loop answerer) always
    /// falls back to the default gate — byte-identical to before this field
    /// existed. `Mutex`-wrapped for the same concurrent-siblings reason as
    /// `branch_node_paths`.
    node_labels: Mutex<HashMap<NodeId, String>>,
    /// PRD 21 C5's runtime-stamped record: completed delegation branches,
    /// keyed by the DELEGATING node's own rendered `NodePath` text, in
    /// completion order. Populated by [`Self::drain_note_holes`] the moment
    /// a `SubagentAwait` this driver services decodes a bound worktree
    /// branch ([`engine::decode_completed_delegation_branch`]); consumed
    /// (removed) by [`Self::service_delegated_branches`] when
    /// `Harness.hs`'s `foldAt` reads it back via `takeDelegatedBranches`.
    /// Never touched by, or visible to, any model window — see
    /// [`delegate_branches_decl`]'s doc. `Mutex`-wrapped for the same
    /// concurrent-siblings reason as `branch_node_paths` — two siblings
    /// under one parent can each `delegate` in the SAME bulk window.
    delegated_branches: Mutex<HashMap<String, Vec<String>>>,
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
    subagent: Option<tidepool_handlers::SubagentHandler>,
    journal: Option<tidepool_handlers::JournalHandler>,
}

/// Which of the two answerer-window modes a node is running under — the
/// path review's own negative evidence (P2.2/P3 typestate opportunity):
/// [`Self::run_loop_fragment`]'s per-loop answerer (kept open across every
/// ordinary `runLLMTurn` hole, its cumulative transcript IS the specified
/// context window) and [`Self::service_outer_branch`]'s branch child
/// (forked off a frozen prefix, retired after exactly one result) share
/// the same finalize-driving code (`drive_answerer_to_finalize`) but must
/// NOT share their post-finalize behavior — merging them would either
/// discard the loop's accumulating window between ordinary holes or leak a
/// one-shot branch child past its single result. Previously distinguished
/// only by comments and which local variable a raw `NodeId` happened to
/// live in; now a real two-variant sum whose own methods refuse the wrong
/// mode instead of silently reusing/discarding the wrong window.
///
/// The `OneShotBranch` half is superseded at its one call site by
/// [`BranchWindow`], a consuming guard over the same three fields — this
/// sum is what proves the two modes are typed as distinct in the first
/// place; `BranchWindow` is the deeper, ownership-tracked treatment of the
/// one-shot half alone.
#[derive(Debug, Clone, Copy)]
enum WindowLease {
    /// [`Self::answerer`]'s mode: never retired between holes, only ever
    /// read via [`Self::retire_answerer`] at loop end.
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

impl WindowLease {
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

/// A `runLLMTurnBranch` child's window transaction — the deeper,
/// ownership-tracked treatment of [`WindowLease::OneShotBranch`] alone
/// (P3.2's typestate opportunity). [`Self::service_outer_branch`] used to
/// retire its node by hand in four places (a mechanism error, a
/// non-finalize exit, a closure rejection, and success), linked only by
/// sequencing and a bare `NodeId` a reader had to trust every future error
/// arm would remember to terminate. This guard makes retiring exactly
/// once, on every exit, structural instead of a four-site discipline:
/// [`Self::finalize_data`] freezes then retires, [`Self::fold_exit`]
/// retires then produces the exit, and `Drop` retires an unfinished
/// window — the mechanism-failure `?` early return that used to need its
/// OWN hand-written `terminate_node` call now needs none.
///
/// Non-Clone: at most one guard exists per branch child.
struct BranchWindow {
    agent: Arc<Harness>,
    node: NodeId,
    realm: tidepool_codegen::jit_machine::RealmId,
    scope: tidepool_codegen::scope::ScopeId,
    validated_ref: ContextRef,
    retired: bool,
}

// Every field here (`Arc`, `NodeId`, `RealmId`, `ScopeId`, `ContextRef`,
// `bool`) is independently Clone, so a `#[derive(Clone)]` would compile
// silently — and then a clone's `retired` flag would diverge from the
// original's, letting `finalize_data`/`fold_exit` and the panic-safety `Drop`
// each believe THEY own retiring the window, double-retiring the node this
// guard exists to retire exactly once.
static_assertions::assert_not_impl_any!(BranchWindow: Clone, Copy);

impl BranchWindow {
    /// Mint a guard from an already-established [`WindowLease::OneShotBranch`]
    /// — `require_one_shot` refuses to hand back node/realm/scope if `lease`
    /// were ever (by a future refactor) the loop's reusable answerer instead
    /// of a branch child's own, so this is where that check is load-bearing.
    fn from_lease(
        lease: WindowLease,
        agent: Arc<Harness>,
        validated_ref: ContextRef,
    ) -> Result<Self, DriverError> {
        let (node, realm, scope) = lease.require_one_shot()?;
        Ok(Self {
            agent,
            node,
            realm,
            scope,
            validated_ref,
            retired: false,
        })
    }

    fn node(&self) -> NodeId {
        self.node
    }

    /// Success: take the finalized answer, freeze THIS child's own
    /// post-finalize prefix (before retirement — `freeze_snapshot` reads
    /// the live convo, which `terminate_node` removes), then retire.
    /// Consumes the window; the only way to reach a post-finalize digest.
    fn finalize_data(mut self) -> Result<(Value, String, SnapshotDigest), HarnessError> {
        let (value, rendered) = self.agent.take_finalized_value_keep_open(self.node)?;
        let digest = self.agent.freeze_snapshot(self.node)?;
        self.agent
            .terminate_node(self.node, "branch child retired")?;
        self.retired = true;
        Ok((value, rendered, digest))
    }

    /// A failure ATTRIBUTABLE TO THIS CHILD's window (round exhaustion, a
    /// non-finalize suspension, a closure answer this driver cannot
    /// carry): retire with `reason`, producing nothing further. Consumes
    /// the window.
    fn fold_exit(mut self, reason: &str) {
        let _ = self.agent.terminate_node(self.node, reason);
        self.retired = true;
    }
}

impl Drop for BranchWindow {
    /// Covers exactly the mechanism-failure path: `service_outer_branch`
    /// returns `Err(e)` via `?` before ever reaching
    /// [`Self::finalize_data`]/[`Self::fold_exit`], and this guard simply
    /// goes out of scope. Idempotent with the two consuming methods
    /// (`retired` is set the instant either runs), so this never
    /// double-retires an already-finished window.
    fn drop(&mut self) {
        if !self.retired {
            tracing::warn!(
                node = ?self.node,
                realm = ?self.realm,
                scope = ?self.scope,
                validated_ref = ?self.validated_ref,
                "branch window dropped without an explicit exit (mechanism failure)"
            );
            let _ = self.agent.terminate_node(
                self.node,
                "branch window dropped without an explicit exit (mechanism failure)",
            );
        }
    }
}

/// A cycle's loop-entry decision, minted once per cycle by
/// [`SelfHarnessDriver::take_loop_entry`] and consumed by whichever
/// compilation path runs this cycle — the fused
/// [`SelfHarnessDriver::compile_cycle_entry`] or the unfused
/// [`SelfHarnessDriver::run_loop_fragment_inner`]. Its whole reason to
/// exist is [`SelfHarnessDriver::resume`]'s destructive `self.resume.take()`:
/// before this type, both compile sites called `take_loop_entry` directly,
/// each independently reading `self.resume`, and the fact that only one of
/// them runs per cycle was a RUNTIME CONVENTION a reader had to trust
/// rather than something the types enforced — exactly the shape a future
/// third call site (a preparatory/fallback compile) could violate. Non-Clone:
/// at most one plan is ever live, so a resume fold cannot be injected twice
/// or consumed by the wrong compile.
struct CycleEntryPlan {
    code: String,
    helpers: String,
}

impl CycleEntryPlan {
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
    /// bootstrapped until the first [`Self::run_loop`]/[`Self::run_one_cycle`]
    /// call (it needs the loaded [`HarnessSource`] first).
    pub fn new(agent: Arc<Harness>, observer: Arc<dyn Observer>) -> Self {
        SelfHarnessDriver {
            outer: None,
            iteration_realm: AtomicU64::new(0),
            last_rotation_losses: None,
            pending_operator_input: None,
            cycle_state_json: None,
            agent,
            lifecycle: SelfHarnessState::Idle,
            observer,
            last_compaction: None,
            cycle_compaction: None,
            compaction_threshold_percent: DEFAULT_COMPACTION_THRESHOLD_PERCENT,
            answerer_framing: None,
            answerer: None,
            loop_inference_calls: AtomicU32::new(0),
            answerer_nudge_rounds: ANSWERER_NUDGE_ROUNDS,
            answerer_max_rounds: ANSWERER_MAX_ROUNDS,
            loop_inference_call_cap: LOOP_INFERENCE_CALL_CAP,
            concurrency_cap: DEFAULT_CONCURRENCY_CAP,
            checkpoint_path: persistence::default_checkpoint_path(),
            checkpoint_generation: None,
            last_checkpoint: None,
            iteration: 0,
            gate: Arc::new(StdinGate),
            ask_id_counter: AtomicU64::new(0),
            handlers: Mutex::new(OuterHandlers::default()),
            resume: None,
            branch_node_paths: Mutex::new(HashMap::new()),
            node_labels: Mutex::new(HashMap::new()),
            delegated_branches: Mutex::new(HashMap::new()),
        }
    }

    /// The driver's current lifecycle state.
    pub fn lifecycle(&self) -> &SelfHarnessState {
        &self.lifecycle
    }

    /// Refuse to proceed while [`SelfHarnessState::Poisoned`] — the guard every
    /// public entry point (`run_one_cycle`/`run_loop`/`restore`) calls first.
    fn refuse_if_poisoned(&self) -> Result<(), DriverError> {
        match &self.lifecycle {
            SelfHarnessState::Poisoned { reason } => Err(DriverError::Poisoned(reason.clone())),
            _ => Ok(()),
        }
    }

    /// Discard every mutable resident component a cycle may have left behind:
    /// retire the per-loop answerer, clear its framing and this cycle's
    /// compaction, reset the inference-call counter, and drop the outer
    /// session — which may be parked mid-fragment on a hole. Dropping `outer`
    /// IS the discard: [`Self::bootstrap`] rebuilds it from the harness
    /// source the next time it is called, since it only no-ops while `outer`
    /// is `Some`.
    fn discard_resident_state(&mut self) {
        self.retire_answerer();
        self.answerer_framing = None;
        self.cycle_compaction = None;
        self.loop_inference_calls.store(0, Ordering::SeqCst);
        self.outer = None;
    }

    /// The author modules an answerer turn imports, once bootstrapped — what
    /// brings the hole's answer type into scope.
    fn answerer_imports(&self) -> &[String] {
        self.outer.as_ref().map_or(&[], |o| &o.answerer_imports)
    }

    /// The [`AnswerContract`] for a hole of type `ty`: pin `finalize` to it and
    /// import the author modules so the type resolves — to the SAME defining
    /// module the outer loop resolved, so the finalized value's constructor ids
    /// match at the crossing.
    ///
    /// `None` when the hole's type is unknown (no `asks.json` entry): there is
    /// nothing to pin `finalize` to, so the turn keeps the polymorphic verb.
    fn answer_contract(&self, ty: Option<&str>) -> Option<AnswerContract> {
        Some(AnswerContract {
            ty: ty?.to_string(),
            imports: self.answerer_imports().to_vec(),
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
    fn types_in_scope_hint(&self, ty: &str, error: &str) -> Option<String> {
        if !(error.contains("Not in scope") && error.contains(ty)) {
            return None;
        }
        let imported = match self.answerer_imports() {
            [] => "no author modules are importable by this stack".to_string(),
            mods => format!("this turn imports {}", mods.join(", ")),
        };
        Some(format!(
            "\n\nNOTE: `{ty}` is not in scope and {imported}. The answering stack \
             cannot import the module that defines `loop` (its `runLLMTurn` is not \
             in this effect row), so the harness author must move `{ty}` into a \
             separate module that `loop`'s module imports."
        ))
    }

    /// Override the operator-input gate (default [`StdinGate`]). A web/GUI
    /// implementation of [`OperatorGate`] replaces the headless stdin
    /// behavior; a test can inject a scripted gate instead of driving real
    /// stdin. Also wires the SAME gate into the underlying [`Harness`]'s
    /// escalation ladder ([`Harness::set_escalation_gate`]), so a fork/fanout
    /// child's rung-2 cap-exhaustion escalation reaches this gate too — one
    /// call covers both `askUser` and escalation asks.
    pub fn set_gate(&mut self, gate: Arc<dyn OperatorGate>) {
        self.agent.set_escalation_gate(gate.clone());
        self.gate = gate;
    }

    /// Wire the subagent seam: the handler a `Subagent` suspension from the
    /// AUTHORED loop dispatches into ([`Self::service_outer_subagent`]).
    /// Construct it with the target repo as its source repository and its
    /// registry/worktree/binding roots OUTSIDE any git work tree; back it
    /// with `MockBackend` in tests and `CodexAgentBackend` live.
    pub fn set_subagent_handler(&mut self, handler: tidepool_handlers::SubagentHandler) {
        self.handlers.lock().subagent = Some(handler);
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
    /// [`ANSWERER_NUDGE_ROUNDS`]/[`ANSWERER_MAX_ROUNDS`], 16/32).
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
    /// after the most recent [`Self::run_one_cycle`].
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
            answerer_imports: source.answerer_imports.clone(),
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
    /// `Member <Eff> effs => ... -> Eff effs T` now validates at define time
    /// AND persists across turns/windows — Core's tycons are the same ones
    /// every later turn's compile sees, so a bound call site unifies cleanly.
    /// A declaration that instead spells the per-window `M` alias still fails
    /// validation with an ordinary GHC "not in scope" error (the shim isn't
    /// on this plane's include path) — the narrowed structural guard, not the
    /// old blanket one. The OUTER render/loop compiles never see this plane
    /// (their include never carries it): the authored harness cannot silently
    /// depend on model-authored names (pillar D) — unaffected by this change.
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
    /// [`Self::compile_cycle_entry`] in ONE `tidepool-extract` spawn. Named in
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
    /// [`Self::resume`]'s doc), minted into a [`CycleEntryPlan`] a caller
    /// then consumes exactly once. [`Self::run_one_cycle`] mints ONE plan
    /// per cycle and passes it down to [`Self::compile_cycle_entry`] (the
    /// fused, production path); the unfused [`Self::run_loop_fragment_inner`]
    /// — a direct fragment API `run_one_cycle` never itself calls, used by a
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
    fn take_loop_entry(&mut self) -> CycleEntryPlan {
        let q = state_cross::LOADED_QUALIFIER;
        match self.resume.take() {
            Some(pending) if !pending.fold.is_empty() => CycleEntryPlan {
                code: format!("{q}.resumeLoop __selfHarnessResume __selfHarnessState"),
                helpers: state_cross::resume_in(&pending.fold),
            },
            _ => CycleEntryPlan {
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
    /// `plan` is this cycle's [`CycleEntryPlan`] — minted ONCE by the caller
    /// ([`Self::run_one_cycle`]) via [`Self::take_loop_entry`] and consumed
    /// HERE, never minted by this method itself: see `CycleEntryPlan`'s doc
    /// for why that split is the point.
    fn compile_cycle_entry(
        &mut self,
        prior_state: Option<&Json>,
        plan: CycleEntryPlan,
    ) -> Result<(CompiledTurn, CompiledTurn), DriverError> {
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

        let (loop_code, resume_helpers) = plan.into_code_and_helpers();
        let helpers = format!(
            "{}{}{}",
            state_cross::state_in(prior_state),
            state_cross::operator_msg_in(self.pending_operator_input.as_deref()),
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
        let mut turns = engine::compile_turns(
            &extract_bin,
            &src,
            &["result", Self::LOOP_ENTRY_TARGET],
            &include,
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

    /// Run ONE `render` → `loop` → (service each `runLLMTurn` hole) →
    /// `render` cycle: bootstrap the outer session if needed, render the
    /// pre-loop prompt ([`Self::render_framing`] — the author's `render`
    /// output composed with the prior compaction summary and the
    /// loop-iteration count), run `loop state` as a suspendable fragment
    /// (servicing every `runLLMTurn` hole via
    /// [`Self::service_runllm_hole`]), serialize the returned `State`
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
    pub async fn run_one_cycle(
        &mut self,
        source: &HarnessSource,
        prior_state: Option<&Json>,
    ) -> Result<CycleOutcome, DriverError> {
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
        // down to `compile_cycle_entry` rather than letting that method
        // mint its own (`CycleEntryPlan`'s doc).
        let plan = self.take_loop_entry();

        // Compile the pre-loop `render` and this cycle's `loop` fragment
        // TOGETHER, in ONE spawn (`Self::compile_cycle_entry`), then run the
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
        let (prompt_before, loop_turn) = match self.compile_cycle_entry(prior_state, plan) {
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
        self.answerer_framing = Some(format!("{prompt_before}\n\n{}", answerer_framing_suffix()));

        self.lifecycle = SelfHarnessState::RunningLoop;
        // The driver must not strand the lifecycle in `RunningLoop`/`Compacting`
        // on any exit from the loop body: a runaway-cap hard-fail, a failed
        // resume, or a compaction error all leave a mutable resident session
        // (the outer session, the per-loop answerer) that outlives this call.
        // Run the fallible body, then publish `Idle` on success or `Failed`
        // (after discarding that resident state) on error — never `Idle` on
        // a path that didn't actually finish.
        let result: Result<CycleOutcome, DriverError> = async {
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

            Ok(CycleOutcome {
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
    /// yet), then run [`Self::run_one_cycle`] FOREVER, threading each
    /// cycle's returned `State` into the next one (each cycle commits its
    /// own checkpoint on success — see [`Self::commit_checkpoint`] — so
    /// this loop does no persistence of its own). Production entry point —
    /// see the module doc for why this (and everything it calls) must run
    /// on a thread with an active multi-thread tokio runtime.
    ///
    /// Between-loops human gate: before each new cycle
    /// AFTER the first, unless `auto` is set, print "press Enter to continue"
    /// and block on a line from stdin — a human checkpoint that keeps a
    /// misbehaving harness from running away across loops. `auto` (the
    /// binary's `--yes`/`--auto` flag) skips the gate for CI/replay. The
    /// acceptance path drives [`Self::run_one_cycle`] directly and has NO gate.
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
        // A restored checkpoint carrying the gate-park marker means the prior
        // process was killed WHILE PARKED on `between_loops_gate` — restart-
        // acts-as-continue is exactly the defect this closes, so this run
        // must re-park BEFORE any turn work rather than silently deciding
        // "continue" on the operator's behalf. Forcing `first = false` routes
        // through the ordinary gate check below. A checkpoint with no marker
        // (never parked, or written before the marker field existed) leaves
        // `first = true`, byte-for-byte today's straight-into-turn-1 behavior
        // — including the very first run ever, which has no checkpoint at all
        // (`last_checkpoint` is `None`, so `is_some_and` is `false`).
        let mut first = !self
            .last_checkpoint
            .as_ref()
            .is_some_and(persistence::Checkpoint::awaiting_continue);
        loop {
            if !first && !auto {
                self.between_loops_gate()?;
            }
            first = false;
            let outcome = match self.run_one_cycle(source, state_json.as_ref()).await {
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
                    self.run_one_cycle(source, state_json.as_ref()).await?
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
        // Kept whole (including its `awaiting_continue` marker) so
        // `Self::run_loop` can decide whether to re-park before any turn work,
        // and so `Self::mark_awaiting_continue` has a same-generation
        // checkpoint to flip the marker on without a fresh `run_one_cycle`
        // commit in between.
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
    /// (already advanced by [`Self::run_one_cycle`] before this call) go
    /// into one [`persistence::Checkpoint`], written atomically under the
    /// next generation. Called once, at the end of [`Self::run_one_cycle`]'s
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

    /// Overwrite the persisted checkpoint's `awaiting_continue` marker in
    /// place, at the SAME generation ([`persistence::Checkpoint::with_awaiting_continue`])
    /// — this is not a newly completed cycle, only a record of whether the
    /// driver is currently parked on the between-turns gate. Requires
    /// `self.last_checkpoint` to already be `Some`: [`Self::between_loops_gate`]
    /// (the only caller) is only ever reached after a checkpoint has been
    /// committed (a prior successful cycle, this same process or a restored
    /// one) or restored, both of which set it. Used ONLY for the park-write
    /// (`awaiting_continue = true`, before the gate blocks) — the clear-write
    /// after a continue signal arrives goes through
    /// [`Self::clear_awaiting_continue`] instead, since that write must ALSO
    /// carry the operator's text.
    fn mark_awaiting_continue(&mut self, awaiting_continue: bool) -> Result<(), DriverError> {
        #[allow(
            clippy::expect_used,
            reason = "between_loops_gate is only reached after a checkpoint has been \
                      committed or restored (see run_loop's first-gating)"
        )]
        let checkpoint = self
            .last_checkpoint
            .as_ref()
            .expect(
                "between_loops_gate is only reached after a checkpoint has been \
                 committed or restored (see run_loop's first-gating)",
            )
            .with_awaiting_continue(awaiting_continue);
        persistence::save_checkpoint(&self.checkpoint_path, &checkpoint)?;
        self.last_checkpoint = Some(checkpoint);
        Ok(())
    }

    /// Clear `awaiting_continue` and, in the SAME `save_checkpoint` call,
    /// record `pending_operator_input` (the operator's between-loops message,
    /// if any) and the current ask-id high-water mark — this is the ONE
    /// write [`Self::between_loops_gate`] performs the instant
    /// `await_continue` returns, so a kill anywhere after it cannot separate
    /// "gate cleared" from "operator text captured" (review finding [1]): both
    /// land in one committed checkpoint or neither does. `self.pending_operator_input`
    /// is updated from the SAME value written to disk, so the in-memory and
    /// durable copies never diverge.
    fn clear_awaiting_continue(
        &mut self,
        pending_operator_input: Option<String>,
    ) -> Result<(), DriverError> {
        #[allow(
            clippy::expect_used,
            reason = "between_loops_gate is only reached after a checkpoint has been \
                      committed or restored (see run_loop's first-gating)"
        )]
        let checkpoint = self
            .last_checkpoint
            .as_ref()
            .expect(
                "between_loops_gate is only reached after a checkpoint has been \
                 committed or restored (see run_loop's first-gating)",
            )
            .with_awaiting_continue(false)
            .with_pending_operator_input(pending_operator_input.clone())
            .with_ask_id_high_water(self.ask_id_counter.load(Ordering::SeqCst));
        persistence::save_checkpoint(&self.checkpoint_path, &checkpoint)?;
        self.pending_operator_input = pending_operator_input;
        self.last_checkpoint = Some(checkpoint);
        Ok(())
    }

    /// The between-loops human checkpoint: block on [`OperatorGate::await_continue`]
    /// — the human-clicks-continue gate. The default [`StdinGate`] keeps the
    /// original headless behavior (block on a stdin line); a web/GUI gate
    /// parks on a button click instead.
    ///
    /// The gate park is a live continuation only — nothing about it lives in
    /// `State` or anywhere else a checkpoint otherwise captures — so a kill
    /// while parked here needs its OWN durable record: [`Self::mark_awaiting_continue`]
    /// writes the marker `true` right before blocking, and
    /// [`Self::clear_awaiting_continue`] writes it `false` — ATOMICALLY WITH
    /// the operator's continue text, if any (review finding [1]) — right after
    /// the continue signal arrives, so a checkpoint read at any instant this
    /// process might die tells a restart which side of the gate it was on
    /// (see [`Self::run_loop`]'s restore-time check) and never loses the
    /// operator's steering text to a crash between two separate writes.
    fn between_loops_gate(&mut self) -> Result<(), DriverError> {
        self.mark_awaiting_continue(true)?;
        // `OperatorGate::await_continue` is SYNC-BLOCKING by frozen contract
        // (`selfharness/operator.rs`) — a web gate parks a channel. Run the
        // park under `block_in_place` so that blocking wait yields the tokio
        // worker to other tasks instead of stalling it.
        let gate = Arc::clone(&self.gate);
        let signal = tokio::task::block_in_place(move || gate.await_continue());
        let operator_text = match signal {
            crate::selfharness::operator::ContinueSignal::ContinueWithInput(text) => {
                self.emit(Event::OperatorMessage { text: text.clone() });
                Some(text)
            }
            crate::selfharness::operator::ContinueSignal::Continue => None,
        };
        self.clear_awaiting_continue(operator_text)?;
        Ok(())
    }

    /// Drive `Loaded.loop __selfHarnessState` (spliced via
    /// [`state_cross::state_in`]) as a suspendable fragment on the outer
    /// session, servicing every `runLLMTurn` hole it suspends on via
    /// [`Self::service_runllm_hole`] until it completes. Returns the
    /// completed `State` value and the DataConTable its OWN compile produced
    /// (the table every hole along this same continuation classifies
    /// against — `resume` never recompiles).
    ///
    /// `precompiled`, when `Some`, is this cycle's loop entry from
    /// [`Self::compile_cycle_entry`] — used AS-IS instead of compiling one
    /// here, which is how [`Self::run_one_cycle`] pays only ONE fused spawn
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
        self.cycle_state_json = prior_state.cloned();

        // Create the ONE render-seeded answerer session for this whole
        // loop, up front — every `runLLMTurn` hole pushes onto it, so hole #2
        // sees hole #1's exchange (the accumulating context window). Retired
        // in `retire_answerer` once the loop completes (or errors out).
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
        self.answerer = Some(WindowLease::ReusableLoop {
            node: answerer,
            realm,
        });

        let result = self.run_loop_fragment_inner(prior_state, precompiled).await;
        self.retire_answerer();
        result
    }

    /// Retire the current loop's answerer node (terminalize it and drop its
    /// session), so the next loop starts from a fresh render-seeded one.
    /// Idempotent — a no-op if no answerer is live.
    fn retire_answerer(&mut self) {
        if let Some(lease) = self.answerer.take() {
            let _ = self
                .agent
                .terminate_node(lease.node(), "loop answerer retired");
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
                // `run_one_cycle` (which mints one plan and passes it to
                // `compile_cycle_entry` instead). See `CycleEntryPlan`'s doc.
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

        // EVERY branch below hands back a `ServicedHole` instead of directly
        // pushing to `ready`/breaking the loop — see that type's doc for why:
        // a servicing arm that computed a next outcome and merely assigned it
        // to a dead local, rather than returning it, is the incident this
        // subsumes into the type. `HoleRouting::Green` is the one exception,
        // documented at `ServicedHole` and at `Self::service_green_hole`.
        let outcome_result: Result<(Value, DataConTable), DriverError> = loop {
            let Some(GreenReady { chain, outcome }) = ready.pop_front() else {
                break Err(DriverError::Session(
                    "green scheduler starved: no ready work and the outer loop never completed \
                     (a parked thread with no waiter and no completion path)"
                        .into(),
                ));
            };
            let serviced: ServicedHole = match outcome {
                ResidentOutcome::Completed { result, .. } => ServicedHole::Completed {
                    result: result.into_value(),
                    table: compiled.table.clone(),
                },
                ResidentOutcome::Suspended { hole, request, .. } => {
                    let classified =
                        engine::classify_hole(&request, &compiled.table, &compiled.asks)?;
                    match &classified.routing {
                        HoleRouting::RunLLMTurn { site, ty } => {
                            let answer = self
                                .service_runllm_hole(
                                    site.get(),
                                    ty.as_deref(),
                                    &classified.prompt,
                                    &compiled.table,
                                )
                                .await?;
                            // Between holes — if the answerer's accumulated
                            // context has crossed threshold, compact + replace its
                            // context IN PLACE now, so the NEXT hole drives under
                            // the smaller window.
                            //
                            // This runs only AFTER `service_runllm_hole` has
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
                            ServicedHole::Resumed(GreenReady {
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
                        HoleRouting::AskUser { .. } | HoleRouting::Note { .. } => {
                            let next = self
                                .service_outer_askuser_hole(
                                    hole.clone(),
                                    classified.routing.clone(),
                                    &compiled,
                                )
                                .await?;
                            ServicedHole::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        // The AUTHORED loop called a Subagent verb
                        // (`spawnAgent`/`spawnAgentRaw`) — dispatch the
                        // ORIGINAL request into the driver-owned handler
                        // (suspension-serviced; the outer handled prefix
                        // stays empty) and resume with its typed response.
                        HoleRouting::Subagent => {
                            let value = self.service_outer_subagent(&request, &compiled.table)?;
                            let sid = self.outer_sid()?;
                            let next = self
                                .agent
                                .with_session(sid, |s| s.resume(hole, value))
                                .map_err(|e| DriverError::Session(e.to_string()))?
                                .map_err(|e| {
                                    DriverError::Session(format!("subagent resume failed: {e}"))
                                })?;
                            ServicedHole::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        // Console/Worktree/RepoEvent/Exec (S1-L1) / Journal
                        // (run-journal lane) — same suspension-servicing
                        // shape as Subagent above, generalized over
                        // `OuterEffectKind`.
                        HoleRouting::OuterEffect(kind) => {
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
                            // genuinely parked — `ServicedHole::LeaveParked`,
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
                                        ServicedHole::LeaveParked(GreenReady {
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
                                        ServicedHole::Resumed(GreenReady {
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
                                ServicedHole::Resumed(GreenReady {
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
                        // `ServicedHole` (see that type's doc), so it owns
                        // `ready` directly and this arm hands nothing back.
                        HoleRouting::Green => {
                            self.service_green_hole(
                                chain,
                                hole.cont_id(),
                                &request,
                                &compiled.table,
                                &mut threads,
                                &mut waiters,
                                &mut next_tid,
                                &mut next_thread_realm,
                                &mut ready,
                            )?;
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
                        HoleRouting::Fork {
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
                            ServicedHole::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        // PRD 21 lane C3 GAP 1: `freezeContext` — immediate,
                        // no operator, no model round (mirrors `ReadState`'s
                        // service shape above it).
                        HoleRouting::FreezeContext => {
                            let value = self.service_outer_freeze_context(&compiled.table)?;
                            let sid = self.outer_sid()?;
                            let next = self
                                .agent
                                .with_session(sid, |s| s.resume(hole, value))
                                .map_err(|e| DriverError::Session(e.to_string()))?
                                .map_err(|e| {
                                    DriverError::Session(format!("freezeContext resume failed: {e}"))
                                })?;
                            ServicedHole::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        // PRD 21 C5: `takeDelegatedBranches path`, called
                        // only from `Harness.hs`'s own `foldAt` — immediate,
                        // no operator, no model round (mirrors
                        // `FreezeContext`/`ReadState`'s service shape).
                        HoleRouting::DelegatedBranches { path } => {
                            let value =
                                self.service_delegated_branches(path, &compiled.table)?;
                            let sid = self.outer_sid()?;
                            let next = self
                                .agent
                                .with_session(sid, |s| s.resume(hole, value))
                                .map_err(|e| DriverError::Session(e.to_string()))?
                                .map_err(|e| {
                                    DriverError::Session(format!(
                                        "takeDelegatedBranches resume failed: {e}"
                                    ))
                                })?;
                            ServicedHole::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        // PRD 21 lane C3 GAP 1: `runLLMTurnBranch @T ref
                        // prompt` — fork a child off the frozen prefix `ref`
                        // names (never an empty root) and resume with `(T,
                        // ContextRef)`.
                        HoleRouting::Branch {
                            site,
                            ty,
                            context_ref,
                            label,
                        } => {
                            let value = self
                                .service_outer_branch(
                                    site.get(),
                                    ty.as_deref(),
                                    context_ref,
                                    label.as_deref(),
                                    &classified.prompt,
                                    &compiled.table,
                                )
                                .await?;
                            let sid = self.outer_sid()?;
                            let next = self
                                .agent
                                .with_session(sid, |s| s.resume(hole, value))
                                .map_err(|e| DriverError::Session(e.to_string()))?
                                .map_err(|e| {
                                    DriverError::Session(format!("branch resume failed: {e}"))
                                })?;
                            ServicedHole::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        // The bulk sibling of `HoleRouting::Branch`: N
                        // children fork off ONE parent `context_ref`,
                        // driven CONCURRENTLY (operator decision: sibling
                        // branch windows are ALWAYS concurrent, never a
                        // model-visible choice).
                        HoleRouting::BranchFanout {
                            site,
                            ty,
                            context_ref,
                            labels,
                            prompts,
                        } => {
                            let value = self
                                .service_outer_branch_fanout(
                                    site.get(),
                                    ty.as_deref(),
                                    context_ref,
                                    labels,
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
                                    DriverError::Session(format!(
                                        "branch fanout resume failed: {e}"
                                    ))
                                })?;
                            ServicedHole::Resumed(GreenReady {
                                chain,
                                outcome: next,
                            })
                        }
                        other => {
                            break Err(DriverError::Session(format!(
                                "outer loop suspended on an unserviceable hole ({other:?}) — \
                                 the Harness monad exposes runLLMTurn, runLLMTurnBranch, \
                                 runLLMTurnBranchFanout, freezeContext, askUser, note, \
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
                ServicedHole::Completed { result, table } => break Ok((result, table)),
                ServicedHole::Resumed(gr) | ServicedHole::LeaveParked(gr) => {
                    ready.push_back(gr);
                }
            }
        };

        // Structured-concurrency scope exit: every thread this loop spawned
        // is scoped to this ONE `loop` fragment run — close whatever is left
        // running (never joined/cancelled by the authored code) so its
        // realm's frames/handles don't outlive the cycle that created them.
        // Idempotent and cheap on the common case (every thread already
        // joined or cancelled leaves nothing to close).
        if let Ok(sid) = self.outer_sid() {
            for entry in threads.values() {
                if matches!(entry.state, GreenThreadState::Running) {
                    let _ = self.agent.with_session(sid, |s| s.close_realm(entry.realm));
                }
            }
        }
        outcome_result
    }

    /// Service one `Tidepool.Async` suspension (PRD 20 S1-L4): decode which
    /// of the six `Async*With` verbs `request` is by CONSTRUCTOR NAME (never
    /// in [`engine::classify_hole`] — the payload may carry a live closure,
    /// see [`HoleRouting::Green`]'s doc) and act, mutating the scheduler's
    /// thread table / waiter map / ready queue in place.
    ///
    /// Deliberately returns `Result<(), DriverError>`, not a [`ServicedHole`]
    /// — checked and rejected before the rest of the dispatcher adopted that
    /// sum. Its six arms push zero (`AsyncJoinAnyWith` with no terminal
    /// candidate, `AsyncDoneWith` on a cancelled/already-settled thread),
    /// one, two (`AsyncSpawnWith`: the resumed spawner and the freshly
    /// started thread), or an arbitrary N (`AsyncDoneWith`/`AsyncCancelWith`
    /// waking every parked joiner) items onto `ready`, and several never
    /// resume the triggering hole at all (`AsyncDoneWith`'s own hole stays
    /// parked forever, its frame reclaimed only when the thread's realm
    /// eventually closes). `ServicedHole::{Resumed,LeaveParked}` both assume
    /// "exactly one hole, exactly one outcome, handed back once" — this
    /// method's job is precisely to not have that shape, so forcing it into
    /// the sum would mean returning `Vec<ServicedHole>` (or a payload-free
    /// `Handled` marker), neither of which catches anything a caller
    /// forgetting to `?` this `Result` doesn't already catch today. Mirrors
    /// [`Self::service_outer_subagent`]'s shape (driver-owned, suspension-
    /// serviced, no handler) but is not a single dispatch-then-resume: a
    /// spawn starts a NEW top-level run and a park-until-terminal join may
    /// register a waiter instead of answering immediately.
    #[allow(clippy::too_many_arguments)]
    fn service_green_hole(
        &mut self,
        chain: GreenChain,
        hole: &str,
        request: &Value,
        table: &DataConTable,
        threads: &mut HashMap<i64, GreenThread>,
        waiters: &mut HashMap<i64, Vec<(GreenChain, String)>>,
        next_tid: &mut i64,
        next_realm: &mut u64,
        ready: &mut VecDeque<GreenReady>,
    ) -> Result<(), DriverError> {
        let sid = self.outer_sid()?;
        match engine::con_name(request, table) {
            // Field 1 is the thread body — ALWAYS a closure by construction
            // (`asyncSpawn` wraps every body in a lambda so the
            // closure-sentinel scan fires even for `async (pure 5)`; see
            // `tidepool-mcp/src/effect_defs.rs`'s `green_effect_def!` doc).
            Some("AsyncSpawnWith") => {
                let body = self
                    .agent
                    .with_session(sid, |s| s.finalized_handle(hole))
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .ok_or_else(|| {
                        DriverError::Session(
                            "AsyncSpawnWith: spawner frame carries no untaken body closure".into(),
                        )
                    })?;
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
                // CONSUME THE BODY CUSTODY FIRST, before anything fallible.
                //
                // `body`'s custody was minted above by `finalized_handle`, and
                // that mint cannot move later: it reads the payload off the
                // SPAWNER's frame, which only exists while that frame is still
                // parked. So the window between mint and consume is inherent —
                // what is not inherent is putting a `?` inside it. Any early
                // return there drops an unconsumed `RootCustody`, whose `Drop`
                // panics, which REPLACES the real `DriverError` with a
                // bookkeeping panic and hides why the spawn actually failed.
                // Starting the thread first closes the window entirely.
                //
                // Scheduling is unaffected: spawner-continues-first is a
                // property of the READY QUEUE order, which is preserved below,
                // not of which call happens first. Both frames are ordinary
                // registry members here — a new top-level run while another
                // frame is parked is exactly what the multi-hole registry is
                // for.
                let thread_start = self
                    .agent
                    .with_session(sid, |s| {
                        s.run_forked("async_thread", body, realm, Some(table))
                    })
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(|e| DriverError::Session(format!("run_forked failed: {e}")))?;
                let tid_value = tid
                    .to_value(table)
                    .map_err(|e| DriverError::Session(format!("AsyncSpawnWith tid box: {e}")))?;
                let spawner_next = self
                    .agent
                    .with_session(sid, |s| s.resume(ResidentHole::plain(hole), tid_value))
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(|e| {
                        DriverError::Session(format!("AsyncSpawnWith spawner resume failed: {e}"))
                    })?;
                ready.push_back(GreenReady {
                    chain,
                    outcome: spawner_next,
                });
                ready.push_back(GreenReady {
                    chain: GreenChain::Thread(tid),
                    outcome: thread_start,
                });
                Ok(())
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
                    return self.wake_green_waiters(tid, table, sid, waiters, ready);
                }
                let answer = if green_field_is_closure(request, 1) {
                    // Owned by the SESSION's realm, deliberately (not the
                    // thread's own) — a result must outlive the thread realm
                    // that produced it, since cancelling or retiring this
                    // thread must not invalidate a waiter's already-delivered
                    // handle.
                    let handle = self
                        .agent
                        .with_session(sid, |s| s.finalized_handle_owned_by(hole, OUTER_REALM))
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
                    // `Tidepool.Async.waitEvent`/`Tidepool.Event` completion
                    // watch (PRD 20 S1-L4 wave 2). `records_result` above
                    // already established this is a genuine Running→Settled
                    // transition, so this always fires exactly once per
                    // settle. No-op if `RepoEvent` was never wired —
                    // `WatchAsync` is unusable without it anyway.
                    if let Some(h) = self.handlers.lock().event.as_mut() {
                        h.registry_mut().publish_async_done(tid);
                    }
                }
                self.wake_green_waiters(tid, table, sid, waiters, ready)
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
                        let next = self
                            .agent
                            .with_session(sid, |s| {
                                s.resume(ResidentHole::plain(hole), winner_value)
                            })
                            .map_err(|e| DriverError::Session(e.to_string()))?
                            .map_err(|e| {
                                DriverError::Session(format!("AsyncJoinAnyWith resume failed: {e}"))
                            })?;
                        ready.push_back(GreenReady {
                            chain,
                            outcome: next,
                        });
                    }
                    None => {
                        // None terminal yet — park this caller as a waiter on
                        // EVERY listed thread; whichever settles/cancels
                        // first wakes it. The hole stays parked; nothing goes
                        // on `ready` — this chain is genuinely blocked.
                        for tid in ids {
                            waiters
                                .entry(tid)
                                .or_default()
                                .push((chain, hole.to_string()));
                        }
                    }
                }
                Ok(())
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
                let next = self
                    .agent
                    .with_session(sid, |s| s.resume(ResidentHole::plain(hole), code_value))
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(|e| {
                        DriverError::Session(format!("AsyncStatusWith resume failed: {e}"))
                    })?;
                ready.push_back(GreenReady {
                    chain,
                    outcome: next,
                });
                Ok(())
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
                        return Err(DriverError::Session(format!(
                            "AsyncResultWith: thread {tid} has not settled (gate with \
                             asyncStatus first)"
                        )))
                    }
                };
                let next = self
                    .agent
                    .with_session(sid, |s| match answer {
                        GreenResult::Value(v) => s.resume(ResidentHole::plain(hole), v),
                        GreenResult::Root(h) => s.resume_handle_borrowed(hole, h),
                    })
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(|e| {
                        DriverError::Session(format!("AsyncResultWith resume failed: {e}"))
                    })?;
                ready.push_back(GreenReady {
                    chain,
                    outcome: next,
                });
                Ok(())
            }
            Some("AsyncCancelWith") => {
                let tid = green_int_field(request, 0, table);
                if let Some(entry) = threads.get_mut(&tid) {
                    if matches!(entry.state, GreenThreadState::Running) {
                        let realm = entry.realm;
                        entry.state = GreenThreadState::Cancelled;
                        self.agent
                            .with_session(sid, |s| {
                                s.close_realm(realm);
                            })
                            .map_err(|e| DriverError::Session(e.to_string()))?;
                        // A cancel is a terminal-state transition exactly like
                        // a settle — `waitEvent` must fire for either, so it
                        // shares the same publish (see the `AsyncDoneWith` arm
                        // above).
                        if let Some(h) = self.handlers.lock().event.as_mut() {
                            h.registry_mut().publish_async_done(tid);
                        }
                        self.wake_green_waiters(tid, table, sid, waiters, ready)?;
                    }
                    // Idempotent: a terminal thread's cancel is a no-op.
                }
                let unit = ()
                    .to_value(table)
                    .map_err(|e| DriverError::Session(format!("AsyncCancelWith () bridge: {e}")))?;
                let next = self
                    .agent
                    .with_session(sid, |s| s.resume(ResidentHole::plain(hole), unit))
                    .map_err(|e| DriverError::Session(e.to_string()))?
                    .map_err(|e| {
                        DriverError::Session(format!("AsyncCancelWith resume failed: {e}"))
                    })?;
                ready.push_back(GreenReady {
                    chain,
                    outcome: next,
                });
                Ok(())
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
    fn wake_green_waiters(
        &mut self,
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
                .agent
                .with_session(sid, |s| {
                    s.resume(ResidentHole::plain(whole.clone()), tid_value.clone())
                })
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
    /// [`crate::engine::HoleRouting::RunLLMTurn`], `prompt` the hole's
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
    /// budget — at [`ANSWERER_NUDGE_ROUNDS`] the answerer is nudged to
    /// finalize, at [`ANSWERER_MAX_ROUNDS`] the hole hard-fails — and
    /// against the per-loop [`LOOP_INFERENCE_CALL_CAP`] total.
    pub async fn service_runllm_hole(
        &mut self,
        site: u32,
        ty: Option<&str>,
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
                "service_runllm_hole called with no per-loop answerer (run_loop_fragment \
                 must create it first)"
                    .into(),
            )
        })?;
        // This hole finishes by taking the finalized answer and keeping the
        // node open for the NEXT hole — only a `ReusableLoop` lease may do
        // that (see `WindowLease::require_reusable`'s doc).
        let node = lease.require_reusable()?;

        // Declare THIS hole's answer contract on the (reused) answerer node
        // before it takes a turn: the type pins `finalize`, and the harness's
        // types module puts that type in scope. Set per hole, because
        // consecutive holes in one loop can want different types.
        self.agent
            .set_answer_contract(node, self.answer_contract(ty));

        // Push the hole card onto the EXISTING answerer node, accumulating
        // context rather than spawning a fresh one. The SCOPED answerer card
        // (`[AskUser, Finalize]`) names `finalize @T`, NOT the generic
        // `resume expr` (which does not compile against this stack).
        let child_prompt = engine::answerer_hole_card(
            prompt,
            ty,
            self.answerer_imports(),
            Some(table),
            &self.agent.hole_card_effect_row(),
        );
        self.agent.push_user_turn(node, &child_prompt)?;
        self.emit(Event::TurnStart { node });

        // An in-context window has NO branch position and no siblings — its
        // failure IS this turn's failure, which is why `runLLMTurn @T` keeps
        // a bare answer (PRD 21 decision 6's asymmetry, stated at the verb
        // declaration). So a typed exit from the shared round loop collapses
        // back into a hard failure HERE, unchanged from before the exit
        // plumbing existed.
        let outcome = match self.drive_answerer_to_finalize(node, ty, site).await? {
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
                if matches!(classified.routing, HoleRouting::Finalize { .. })
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
            let handle = self.agent.take_finalized_handle_keep_open(node)?;
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

    /// Service a `freezeContext` suspension (PRD 21 lane C3, closing GAP 1):
    /// mint a `ContextRef` naming the CURRENT loop's per-loop answerer
    /// window's frozen prefix, right now — immediately, no operator, no
    /// model round ([`HoleRouting::ReadState`]'s service shape).
    /// `freeze_snapshot` is idempotent, so calling this more than once
    /// without an intervening `runLLMTurn`/`runLLMTurnBranch` returns the
    /// SAME digest rather than writing a second receipt.
    fn service_outer_freeze_context(&mut self, table: &DataConTable) -> Result<Value, DriverError> {
        let lease = self.answerer.ok_or_else(|| {
            DriverError::Session(
                "freezeContext called with no per-loop answerer (run_loop_fragment \
                 must create it first)"
                    .into(),
            )
        })?;
        let digest = self.agent.freeze_snapshot(lease.node())?;
        engine::build_context_ref_value(digest.as_str(), table)
            .map_err(|e| DriverError::Session(e.to_string()))
    }

    /// Service `takeDelegatedBranches path` (PRD 21 C5): hand back — and
    /// CONSUME — this driver's own record of `path`'s completed delegation
    /// branches, oldest first. An empty `Vec` (no delegation ever recorded
    /// for `path`) is the ordinary, byte-unchanged case, not an error.
    fn service_delegated_branches(
        &mut self,
        path: &str,
        table: &DataConTable,
    ) -> Result<Value, DriverError> {
        use tidepool_bridge::ToCore;
        let branches = self
            .delegated_branches
            .lock()
            .remove(path)
            .unwrap_or_default();
        let items = branches
            .into_iter()
            .map(|b| b.to_value(table))
            .collect::<Result<Vec<Value>, _>>()
            .map_err(|e| DriverError::Session(format!("delegated branch text to Value: {e}")))?;
        engine::build_list_value(items, table).map_err(|e| DriverError::Session(e.to_string()))
    }

    /// Service a `runLLMTurnBranch @T ref prompt` suspension raised DIRECTLY
    /// by the AUTHORED outer loop (PRD 21 lane C3, closing GAP 1): fork a
    /// FRESH child window off the frozen prefix `context_ref` names — via
    /// [`Harness::resolve_context_ref`]/[`Harness::fork_from_context_ref`],
    /// C2's `fork_from_snapshot` seam under a typed capability rather than an
    /// empty root — drive it to `finalize @T` (reusing
    /// [`Self::drive_answerer_to_finalize`] UNCHANGED: the node arrives
    /// already seeded with its hole card as the branch's inherited-prefix
    /// opening turn, exactly the "already seeded" precondition that method
    /// already documents), and resume with
    /// `Either InvocationExit (T, ContextRef)` — on success the child's
    /// answer plus a ref to ITS OWN post-finalize frozen prefix so it can be
    /// branched again.
    ///
    /// A branch child is a BRANCH POSITION, so PRD 21 locked decision 6
    /// applies here exactly as it does to fork/fanout: a failure of THIS
    /// WINDOW (round exhaustion, non-finalization) folds as `Left exit` into
    /// the answer, and a failure of the MECHANISM still hard-fails the turn —
    /// notably [`Harness::resolve_context_ref`] refusing an unknown or stale
    /// ref, which is a capability that was never valid rather than a window
    /// that failed. The `Either` wraps the WHOLE pair because a window that
    /// never finalized has no post-finalize prefix, so there would be no
    /// honest `ContextRef` to hand back beside the failure.
    ///
    /// The child's SCOPE is minted as a child of the frozen window's own
    /// scope ([`Harness::context_ref_scope`]) — locked decision 2's "its
    /// compiled blocks and declarations" clause, joined to C2's scope trees
    /// (§1–3) rather than left at the flat `ScopeId::ROOT` every other
    /// fanout/fork child defaults to: a branch child sees its ancestor
    /// chain's declarations and its own defines stay local, never leaking to
    /// a sibling branch or back up to the frozen window.
    ///
    /// Sequential by construction (the AUTHORED loop's `do`-block sequences
    /// `runLLMTurnBranch` calls, each its own suspend/resume round-trip), so
    /// — unlike [`Self::drive_fanout_child`] — this is `&mut self` and needs
    /// no realm-checkout retry dance against concurrent siblings.
    async fn service_outer_branch(
        &mut self,
        site: u32,
        ty: Option<&str>,
        context_ref: &str,
        label: Option<&str>,
        prompt: &str,
        table: &DataConTable,
    ) -> Result<Value, DriverError> {
        self.lifecycle = SelfHarnessState::SuspendedOnHole;
        self.emit(Event::RunLLMTurnHole {
            site,
            ty: ty.map(String::from),
            prompt: prompt.to_string(),
        });

        // The ONE typed checkpoint (possession-is-permission): an
        // unknown/stale ref refuses HERE, as `HarnessError::UnknownSnapshot`
        // — never a silent fresh-root fallback. Everything below only ever
        // sees an ALREADY-VALIDATED `ContextRef`.
        let cref = self.agent.resolve_context_ref(context_ref)?;

        let sid = self.outer_sid()?;
        let hole_card = engine::answerer_hole_card(
            prompt,
            ty,
            self.answerer_imports(),
            Some(table),
            &self.agent.hole_card_effect_row(),
        );
        let node = self.agent.fork_from_context_ref(&cref, &hole_card)?;
        // PRD 21 C5: this branch child's prompt is the ONE place its domain
        // `NodePath` is observable from the runtime side (see
        // `engine::parse_companion_node_path`'s doc). Recorded BEFORE
        // driving so a delegation mid-turn (`HoleRouting::Subagent`,
        // serviced by `drain_note_holes`) can attribute its completed
        // branch to this node; removed unconditionally once the branch
        // finishes, below.
        if let Some(path) = engine::parse_companion_node_path(prompt) {
            self.branch_node_paths.lock().insert(node, path);
        }
        // PRD 21 C5 GUI lane: a `runLLMTurnBranchLabeled` child's label rides
        // the wire structurally (never parsed out of `prompt`, unlike
        // `branch_node_paths` above) — recorded here so
        // `present_askuser_form`/`announce_note` can route this node's own
        // asks/notes to a per-node operator gate; removed unconditionally
        // once the branch finishes, below (mirrors `branch_node_paths`).
        if let Some(label) = label {
            self.node_labels.lock().insert(node, label.to_string());
            // Register the node's panel NOW, not on its first ask/note: the
            // operator watches the tree GROW — a window that works silently
            // (the common case) must still appear the moment it opens and
            // grey at its fold, or the strip only ever shows the noisy nodes.
            let _ = self.gate.node_gate(label);
            // The seed is the AUTHORED brief, not the composed hole card —
            // the operator asked what a node is doing; the answer is what it
            // was told to do, not the harness plumbing around it.
            self.gate.node_seeded(label, prompt);
        }
        self.agent.force_attached(node, Actor::Operator, sid)?;
        let realm = self.mint_realm();
        self.agent.set_node_realm(node, realm);

        let parent_scope = self.agent.context_ref_scope(&cref);
        let child_scope = self
            .agent
            .with_session(sid, |s| s.mint_scope(parent_scope))
            .map_err(|e| DriverError::Session(e.to_string()))?
            .ok_or_else(|| {
                DriverError::Session(format!(
                    "runLLMTurnBranch: the frozen window's scope {parent_scope:?} is not \
                     live (its owning session was rotated or the window already retired)"
                ))
            })?;
        self.agent.set_node_scope(node, child_scope);
        self.agent
            .set_answer_contract(node, self.answer_contract(ty));
        self.emit(Event::TurnStart { node });

        // This child's mode, typed (`WindowLease::require_one_shot`'s doc):
        // it answers exactly once, then is frozen and retired below — it
        // must never be mistaken for the loop's reusable answerer.
        let lease = WindowLease::OneShotBranch {
            node,
            realm,
            scope: child_scope,
        };

        // Every exit below this point retires exactly through `window`
        // (`fold_exit`, `finalize_data`, or — for the mechanism-error `?`
        // below — its `Drop`). See `BranchWindow`'s doc for the four
        // hand-written call sites this replaces.
        let window = BranchWindow::from_lease(lease, self.agent.clone(), cref)?;

        // A branch child is a BRANCH POSITION, so from here on this window's
        // own failures are DATA — folded as `Left exit` into the answer
        // instead of aborting the outer turn (PRD 21 locked decision 6; see
        // `service_outer_fanout`'s doc for the child-attributable/mechanism
        // line, which holds identically here). Everything ABOVE this point is
        // mechanism and still hard-fails: `resolve_context_ref` refusing an
        // unknown or stale ref is a capability that was never valid, not a
        // window that failed, and the scope/fork/force steps are bookkeeping.
        //
        // The `Err(e)` arm below is a MECHANISM failure — it returns via `?`
        // without calling `fold_exit`, so `window` simply drops here; its
        // `Drop` impl is what retires the node on this path now.
        let mut exit: Option<InvocationExit> = None;
        let outcome = match self
            .drive_answerer_to_finalize(window.node(), ty, site)
            .await?
        {
            Ok(o) => Some(o),
            Err(e) => {
                exit = Some(e);
                None
            }
        };
        // Every path below this point (exit, closure, success) is done with
        // this node's own delegation window — see the insert above.
        self.branch_node_paths.lock().remove(&window.node());
        // The node's terminate/fold point (PRD 21 C5 GUI lane): a labeled
        // child's per-node gate is retired here, regardless of which outcome
        // follows — the default gate stays the fallback for this node from
        // this point on, and a `WebGate` marks the panel done rather than
        // dropping it. The label is KEPT past retirement so the outcome
        // paths below can attribute the node's ending (`node_failed`) or its
        // answer (`node_finalized`, which only exists after `finalize_data`
        // freezes the window) to the same section.
        let retired_label = self.node_labels.lock().remove(&window.node());
        if let Some(label) = &retired_label {
            self.gate.retire_node(label);
        }
        self.emit(Event::TurnEnd {
            node: window.node(),
        });

        if let Some(outcome) = &outcome {
            let is_finalize = matches!(
                outcome,
                TurnOutcome::Suspended { classified, .. }
                    if matches!(classified.routing, HoleRouting::Finalize { .. })
            );
            if !is_finalize {
                // NON-FINALIZATION — the window ended on something that is not
                // an answer. Decision 6 names this class; it folds at the
                // branch, it does not take the turn down.
                exit = Some(InvocationExit::NotFinalized(format!(
                    "runLLMTurnBranch child {:?} did not suspend on finalize (got {})",
                    window.node(),
                    turn_outcome_tag(outcome)
                )));
            }
        }

        if let Some(exit) = exit {
            tracing::warn!(
                node = ?window.node(),
                exit = %exit,
                "branch child exited without an answer — folding it as data at its \
                 branch position"
            );
            if let Some(label) = &retired_label {
                self.gate.node_failed(label, &exit.to_string());
            }
            window.fold_exit("branch child retired (exit)");
            self.lifecycle = SelfHarnessState::RunningLoop;
            // The `Either` wraps the WHOLE pair: a window that never finalized
            // has no post-finalize prefix, so there is no honest `ContextRef`
            // to put beside the failure.
            return engine::build_child_answer_value(Err(exit), table)
                .map_err(|e| DriverError::Session(e.to_string()));
        }

        if self.agent.finalize_is_closure(window.node()) {
            // NOT a typed exit, for the same reason as the fanout path: the
            // window DID answer, and it is this driver that cannot carry a
            // closure across the branch pair (v1 scope). Our gap fails as ours.
            if let Some(label) = &retired_label {
                self.gate.node_failed(
                    label,
                    "answered with a closure — cannot cross the branch pair (v1 scope)",
                );
            }
            window.fold_exit("branch child retired (closure)");
            return Err(DriverError::Session(
                "runLLMTurnBranch answer must be plain data — a closure cannot cross \
                 the branch pair (v1 scope)"
                    .into(),
            ));
        }

        // Success: `finalize_data` takes the finalized answer, freezes
        // THIS child's own post-finalize prefix, and retires — the one
        // place a `ContextRef` digest for this window can come from.
        let node = window.node();
        let (value, rendered, child_digest) = window.finalize_data()?;
        if let Some(label) = &retired_label {
            self.gate.node_finalized(label, &rendered);
        }
        self.emit(Event::Finalize {
            node,
            value: rendered,
        });
        self.lifecycle = SelfHarnessState::RunningLoop;

        let ref_value = engine::build_context_ref_value(child_digest.as_str(), table)
            .map_err(|e| DriverError::Session(e.to_string()))?;
        let pair = engine::build_pair_value(value, ref_value, table)
            .map_err(|e| DriverError::Session(e.to_string()))?;
        engine::build_child_answer_value(Ok(pair), table)
            .map_err(|e| DriverError::Session(e.to_string()))
    }

    /// Service a `runLLMTurnBranchFanout @T` suspension — the BULK sibling of
    /// [`Self::service_outer_branch`] (operator decision: sibling branch
    /// windows are ALWAYS driven concurrently, transparently — scheduling is
    /// never a model-visible choice). Every `(label, prompt)` pair forks its
    /// OWN child window off the SAME parent `context_ref`, resolved and
    /// scoped ONCE here (the one typed checkpoint — an unknown/stale ref
    /// refuses HERE, never a silent fresh-root fallback), then driven
    /// CONCURRENTLY via [`Self::drive_branch_fanout_child`] up to
    /// [`Self::concurrency_cap`] at once, exactly like
    /// [`Self::service_outer_fanout`]/[`Self::drive_fanout_child`] — see that
    /// pair's doc for why this is `&mut self` with an inner `&*self`
    /// reborrow, and for the child-attributable/mechanism line (PRD 21
    /// locked decision 6), which holds identically per sibling here: a
    /// sibling's own round exhaustion/non-finalization/provider failure
    /// folds as `Left exit` AT ITS OWN POSITION in the resumed list, never
    /// erasing another sibling's already-finished answer; a broken mechanism
    /// (cardinality, table assembly, session bookkeeping, the per-loop
    /// inference-call cap) still hard-fails the turn via `?`.
    ///
    /// Each sibling is driven by [`Self::drive_branch_fanout_child`] through
    /// the FULL [`Self::drive_answerer_to_finalize`] capability set — unlike
    /// [`Self::drive_fanout_child_inner`]'s finalize-only v1 scope, a branch
    /// sibling CAN delegate or ask the operator mid-window, exactly as a
    /// sequential [`Self::service_outer_branch`] child could — see that
    /// method's own doc for why.
    async fn service_outer_branch_fanout(
        &mut self,
        site: u32,
        ty: Option<&str>,
        context_ref: &str,
        labels: &[String],
        prompts: &[String],
        table: &DataConTable,
    ) -> Result<Value, DriverError> {
        self.lifecycle = SelfHarnessState::SuspendedOnHole;

        if labels.len() != prompts.len() {
            return Err(DriverError::Session(format!(
                "runLLMTurnBranchFanout: {} label(s) but {} prompt(s) — the wire's \
                 labels/prompts fields disagree",
                labels.len(),
                prompts.len()
            )));
        }

        // The ONE typed checkpoint (possession-is-permission), resolved
        // ONCE for the whole sibling group — every child below only ever
        // sees an ALREADY-VALIDATED `ContextRef`/scope pair.
        let cref = self.agent.resolve_context_ref(context_ref)?;
        let parent_scope = self.agent.context_ref_scope(&cref);

        // A fanout site's recorded type is the LIST type (`[T]`); the per-
        // child answer type is what `answer_contract`/the hole card need —
        // mirrors `service_outer_fanout`'s `element_ty` derivation exactly.
        let element_ty = ty.and_then(engine::strip_list_type);

        for prompt in prompts {
            self.emit(Event::RunLLMTurnHole {
                site,
                ty: element_ty.map(String::from),
                prompt: prompt.clone(),
            });
        }

        let sid = self.outer_sid()?;

        // Mint every sibling's own child scope SEQUENTIALLY, here, before any
        // concurrent driving starts. `Harness::with_session` — unlike a
        // node-level checkout — has no contention retry: it hard-refuses the
        // instant another caller holds the shared machine
        // (`HarnessError::Resident("... already running a turn")`). Minting
        // N scopes concurrently (one `with_session` call per child, all
        // racing the SAME `sid`) would trip that refusal under real
        // concurrency; minting is a fast, non-suspending, purely mechanical
        // step, so paying for it up front — once, in declaration order —
        // costs nothing a model window would notice and removes the race
        // entirely.
        let mut child_scopes = Vec::with_capacity(labels.len());
        for _ in labels {
            let child_scope = self
                .agent
                .with_session(sid, |s| s.mint_scope(parent_scope))
                .map_err(|e| DriverError::Session(e.to_string()))?
                .ok_or_else(|| {
                    DriverError::Session(format!(
                        "runLLMTurnBranchFanout: the frozen window's scope {parent_scope:?} \
                         is not live (its owning session was rotated or the window already \
                         retired)"
                    ))
                })?;
            child_scopes.push(child_scope);
        }

        let cap = self.concurrency_cap;
        // A shared borrow of `self` — see `service_outer_fanout`'s doc for
        // why this needs no `Arc<Self>`/`tokio::spawn`.
        let this = &*self;
        #[allow(clippy::type_complexity)]
        let mut results: Vec<(
            usize,
            Result<Result<(Value, String), InvocationExit>, DriverError>,
        )> = stream::iter(
            labels
                .iter()
                .zip(prompts.iter())
                .zip(child_scopes.iter())
                .enumerate(),
        )
        .map(|(idx, ((label, prompt), child_scope))| {
            let cref = cref.clone();
            let child_scope = *child_scope;
            async move {
                let value = this
                    .drive_branch_fanout_child(
                        sid,
                        site,
                        idx,
                        label,
                        prompt,
                        element_ty,
                        cref,
                        child_scope,
                        table,
                    )
                    .await;
                (idx, value)
            }
        })
        .buffer_unordered(cap)
        .collect()
        .await;
        // Completion order is whatever `buffer_unordered` happened to finish
        // in — re-sort to DECLARATION order before assembly, exactly like
        // `service_outer_fanout`.
        results.sort_by_key(|(idx, _)| *idx);

        self.lifecycle = SelfHarnessState::RunningLoop;

        let mut answers = Vec::with_capacity(results.len());
        for (idx, r) in results {
            let outcome = r?;
            let answer = match outcome {
                Ok((value, child_digest)) => {
                    let ref_value = engine::build_context_ref_value(&child_digest, table)
                        .map_err(|e| DriverError::Session(e.to_string()))?;
                    engine::build_pair_value(value, ref_value, table)
                        .map(Ok)
                        .map_err(|e| DriverError::Session(e.to_string()))?
                }
                Err(exit) => {
                    tracing::warn!(
                        child = idx,
                        exit = %exit,
                        "branch fanout child exited without an answer — folding it as \
                         data at its branch position; siblings are unaffected"
                    );
                    Err(exit)
                }
            };
            answers.push(
                engine::build_child_answer_value(answer, table)
                    .map_err(|e| DriverError::Session(e.to_string()))?,
            );
        }

        engine::build_list_value(answers, table).map_err(|e| DriverError::Session(e.to_string()))
    }

    /// Drive ONE `runLLMTurnBranchFanout` sibling: fork off the SHARED parent
    /// `cref` (never an empty root — PRD 21 locked decision 2), apply its
    /// ALREADY-MINTED `child_scope` (minted sequentially by
    /// [`Self::service_outer_branch_fanout`] before any concurrent driving
    /// starts — see that method's doc for why scope-minting itself cannot
    /// happen here, concurrently, without racing `Harness::with_session`'s
    /// checkout), and drive it via [`Self::drive_answerer_to_finalize`] —
    /// the SAME full capability [`Self::service_outer_branch`]'s sequential
    /// child gets. That reuse is load-bearing, not a convenience: a
    /// companion coalgebra window can `delegate` (raise a `Subagent`
    /// suspension) or ask the operator mid-window (`OperatorSteering`), and
    /// batching siblings into one bulk call must not silently drop that
    /// capability — the concurrent `runLLMTurnFanout` machinery's own
    /// finalize-only v1 scope (no nested askUser/note/fork) was built for a
    /// bare fork/fanout child that never had those capabilities in the
    /// first place, and is the wrong model for a branch child that did.
    /// This is why `drive_answerer_to_finalize` and everything it calls —
    /// `drain_note_holes`/`service_askuser_hole`/`drain_answerer_fork` — are
    /// `&self`, and why `branch_node_paths`/`node_labels`/
    /// `delegated_branches` are `Mutex`-wrapped: two siblings under one
    /// parent can each be mid-delegate or mid-form at once.
    ///
    /// Mirrors [`Self::service_outer_branch`]'s own body: the same
    /// [`BranchWindow`] retirement discipline and the same
    /// exit/closure/success ladder, labeled and path-stamped the same way.
    /// Differences: the scope arrives ALREADY minted rather than minted
    /// here; the label is REQUIRED, never `Option`, since every
    /// `runLLMTurnBranchFanout` sibling carries one; and the return is the
    /// bare `(value, digest)` pair, not the fully-assembled `Either` —
    /// [`Self::service_outer_branch_fanout`] does that assembly itself, once
    /// per sibling, after re-sorting every result to declaration order.
    ///
    /// `&self`, for the same reason [`Self::drive_fanout_child`] is: up to
    /// [`Self::concurrency_cap`] of these run concurrently via
    /// `buffer_unordered`, all borrowing the same `&SelfHarnessDriver`.
    #[allow(clippy::too_many_arguments)]
    async fn drive_branch_fanout_child(
        &self,
        sid: tidepool_repr::SessionId,
        site: u32,
        idx: usize,
        label: &str,
        prompt: &str,
        element_ty: Option<&str>,
        cref: ContextRef,
        child_scope: tidepool_codegen::scope::ScopeId,
        table: &DataConTable,
    ) -> Result<Result<(Value, String), InvocationExit>, DriverError> {
        let hole_card = engine::answerer_hole_card(
            prompt,
            element_ty,
            self.answerer_imports(),
            Some(table),
            &self.agent.hole_card_effect_row(),
        );
        let node = self.agent.fork_from_context_ref(&cref, &hole_card)?;
        // PRD 21 C5: this sibling's prompt is the ONE place its domain
        // `NodePath` is observable from the runtime side — recorded BEFORE
        // driving so a delegation mid-turn (`HoleRouting::Subagent`,
        // serviced by `drain_note_holes`) can attribute its completed
        // branch to this node; removed unconditionally once it finishes,
        // below. Mirrors `service_outer_branch` exactly, just against the
        // `Mutex`-wrapped map concurrent siblings share.
        if let Some(path) = engine::parse_companion_node_path(prompt) {
            self.branch_node_paths.lock().insert(node, path);
        }
        // Every `runLLMTurnBranchFanout` sibling is labeled (unlike a plain
        // `runLLMTurnBranch`, whose label is optional) — register its panel
        // NOW, not on its first ask/note, so the operator sees the tree
        // GROW rather than only its noisy nodes.
        self.node_labels.lock().insert(node, label.to_string());
        let _ = self.gate.node_gate(label);
        self.gate.node_seeded(label, prompt);

        self.agent.force_attached(node, Actor::Operator, sid)?;
        let realm = self.mint_realm();
        self.agent.set_node_realm(node, realm);
        self.agent.set_node_scope(node, child_scope);
        // Siblings share this ONE session's machine — a checkout race
        // against another sibling's turn is expected, benign contention,
        // not a real conflict — mirrors `drive_fanout_child`'s own opt-in.
        self.agent.set_retry_checkout_on_contention(node, true);
        self.agent
            .set_answer_contract(node, self.answer_contract(element_ty));
        self.emit(Event::TurnStart { node });

        // This child's mode, typed (`WindowLease::require_one_shot`'s doc):
        // it answers exactly once, then is frozen and retired below.
        let lease = WindowLease::OneShotBranch {
            node,
            realm,
            scope: child_scope,
        };
        // Every exit below this point retires exactly through `window`
        // (`fold_exit`, `finalize_data`, or — for the mechanism-error `?`
        // below — its `Drop`), same discipline `service_outer_branch` uses.
        let window = BranchWindow::from_lease(lease, self.agent.clone(), cref)?;

        // A branch child is a BRANCH POSITION, so from here on this window's
        // own failures are DATA — folded as `Left exit` at ITS OWN POSITION
        // in the resumed list (PRD 21 locked decision 6), never erasing a
        // sibling's already-finished answer.
        let mut exit: Option<InvocationExit> = None;
        let outcome = match self
            .drive_answerer_to_finalize(window.node(), element_ty, site)
            .await?
        {
            Ok(o) => Some(o),
            Err(e) => {
                exit = Some(e);
                None
            }
        };
        self.branch_node_paths.lock().remove(&window.node());
        self.node_labels.lock().remove(&window.node());
        self.gate.retire_node(label);
        self.emit(Event::TurnEnd {
            node: window.node(),
        });

        if let Some(outcome) = &outcome {
            let is_finalize = matches!(
                outcome,
                TurnOutcome::Suspended { classified, .. }
                    if matches!(classified.routing, HoleRouting::Finalize { .. })
            );
            if !is_finalize {
                // NON-FINALIZATION — the window ended on something that is
                // not an answer. Decision 6 names this class; it folds at
                // the branch, it does not take the turn down.
                exit = Some(InvocationExit::NotFinalized(format!(
                    "branch fanout child {idx} did not suspend on finalize (got {})",
                    turn_outcome_tag(outcome)
                )));
            }
        }

        if let Some(exit) = exit {
            tracing::warn!(
                child = idx,
                exit = %exit,
                "branch fanout child exited without an answer — folding it as data at \
                 its branch position; siblings are unaffected"
            );
            self.gate.node_failed(label, &exit.to_string());
            window.fold_exit("branch fanout child retired (exit)");
            return Ok(Err(exit));
        }

        if self.agent.finalize_is_closure(window.node()) {
            // NOT a typed exit: the window DID answer, and it is this
            // driver that cannot carry a closure across the branch pair
            // (v1 scope). Our gap fails as ours.
            self.gate.node_failed(
                label,
                "answered with a closure — cannot cross the branch pair (v1 scope)",
            );
            window.fold_exit("branch fanout child retired (closure)");
            return Err(DriverError::Session(format!(
                "branch fanout child {idx} finalized a closure — a concurrent branch \
                 fanout answer must be plain data in this driver (v1 scope)"
            )));
        }

        // Success: `finalize_data` takes the finalized answer, freezes
        // THIS sibling's own post-finalize prefix, and retires — the one
        // place a `ContextRef` digest for this window can come from.
        let node = window.node();
        let (value, rendered, child_digest) = window.finalize_data()?;
        self.gate.node_finalized(label, &rendered);
        self.emit(Event::Finalize {
            node,
            value: rendered,
        });

        Ok(Ok((value, child_digest.as_str().to_string())))
    }

    /// Service a `runLLMTurnFork @T`/`runLLMTurnFanout @T` suspension raised
    /// DIRECTLY by the AUTHORED outer loop (PRD 20 S1-L4, "concurrent
    /// cognition windows") — `fan: Some(_)` for a fanout (`prompts` one per
    /// child, answered as `[T]`), `fan: None` for a single fork (answered as
    /// bare `T`, `single_prompt` the one task text). Unlike the nested
    /// answerer's own [`Harness::answer_fanout`] (sequential BY DESIGN —
    /// this driver's other fork-servicing path, [`Self::drain_answerer_fork`],
    /// reuses it unchanged), every child here gets its own freshly-minted
    /// answerer realm on the SHARED outer machine and is driven
    /// CONCURRENTLY, up to [`Self::concurrency_cap`] at once
    /// ([`Self::drive_fanout_child`]/[`buffer_unordered`]): only machine
    /// occupancy serializes a child's actual compile+run, everything else
    /// (assembling its prompt, awaiting the provider) overlaps freely.
    /// Completion order is never observable — results are re-sorted back to
    /// DECLARATION order before assembly, exactly like `answer_fanout`'s own
    /// order contract, just reached by a different (order-insensitive
    /// completion, order-preserving assembly) route.
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
    async fn service_outer_fanout(
        &mut self,
        site: u32,
        ty: Option<&str>,
        fan: Option<FanBadge>,
        single_prompt: &str,
        prompts: &[String],
        table: &DataConTable,
    ) -> Result<Value, DriverError> {
        self.lifecycle = SelfHarnessState::SuspendedOnHole;

        let is_fanout = fan.is_some();
        // A fanout site's recorded type is the LIST type (`[T]`); a plain
        // fork's is already the element type — mirrors
        // `Harness::answer_fanout`'s `element_ty` derivation.
        let element_ty = if is_fanout {
            ty.and_then(engine::strip_list_type)
        } else {
            ty
        };
        // Normalize fork (one implicit prompt) and fanout (N explicit
        // prompts) to ONE prompt list, so a single concurrent path serves
        // both — see this method's doc.
        let owned_prompts: Vec<String>;
        let prompts: &[String] = if prompts.is_empty() && !is_fanout {
            owned_prompts = vec![single_prompt.to_string()];
            &owned_prompts
        } else {
            prompts
        };

        // Cardinality integrity — same check `answer_fanout` makes: a
        // dropped non-Text prompt element must fail loud, never silently
        // under-answer a `[T]` the type system already committed to.
        if let Some(FanBadge::Exact { n }) = fan {
            if n as usize != prompts.len() {
                return Err(DriverError::Session(format!(
                    "outer fanout cardinality mismatch: fan={n} but {} prompt(s) decoded \
                     — a non-Text prompt element was dropped, or the fan/prompts wire \
                     fields disagree",
                    prompts.len()
                )));
            }
        }

        for prompt in prompts {
            self.emit(Event::RunLLMTurnHole {
                site,
                ty: element_ty.map(String::from),
                prompt: prompt.clone(),
            });
        }

        let sid = self.outer_sid()?;
        let cap = self.concurrency_cap;
        // A shared borrow of `self` — every concurrent child needs only
        // `&self`-reachable state (the `Arc`-shared `agent`/`gate`, the
        // atomic counters, the plain round-cap config); none of them
        // outlives this `.await`, so no `Arc<Self>`/`tokio::spawn` is
        // needed (see `drive_fanout_child`'s doc for why `tokio::spawn`
        // itself doesn't fit here).
        let this = &*self;
        #[allow(clippy::type_complexity)]
        let mut results: Vec<(usize, Result<Result<Value, InvocationExit>, DriverError>)> =
            stream::iter(prompts.iter().enumerate())
                .map(|(idx, prompt)| async move {
                    let value = this
                        .drive_fanout_child(sid, site, idx, prompt, element_ty, table)
                        .await;
                    (idx, value)
                })
                .buffer_unordered(cap)
                .collect()
                .await;
        // Completion order is whatever `buffer_unordered` happened to
        // finish in (nondeterministic) — re-sort to DECLARATION order
        // before assembly, so the resumed answer never depends on it.
        results.sort_by_key(|(idx, _)| *idx);

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
    /// single-hole path drives), drive its round loop
    /// ([`Self::drive_fanout_child_inner`]), and retire the node either way
    /// (realm scope-exit, never session removal — same discipline
    /// [`Self::retire_answerer`] uses for the reused answerer).
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
    async fn drive_fanout_child(
        &self,
        sid: tidepool_repr::SessionId,
        site: u32,
        idx: usize,
        prompt: &str,
        element_ty: Option<&str>,
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
            .drive_fanout_child_inner(node, site, idx, prompt, element_ty, table)
            .await;
        let _ = self.agent.terminate_node(node, "fanout child retired");
        result
    }

    /// The round loop for one fanout/fork child — a `&self` sibling of
    /// [`Self::drive_answerer_to_finalize`], simplified: a concurrent
    /// fanout child supports `finalize` only (explore/define rounds and
    /// compile-error correction, exactly like the single-hole path) — it
    /// does NOT service a nested `askUser`/`note`/`fork` suspension (v1
    /// scope; the answerer row still declares them, so a child that reaches
    /// for one gets a clear error naming the gap rather than a hang).
    /// `drive_turn`'s OWN checkout waits out sibling contention
    /// transparently (`Harness::checkout_run_retrying`, opted into by
    /// [`Self::drive_fanout_child`]); [`retry_on_turn_in_flight`] covers the
    /// one OTHER call here that can lose the same race
    /// (`take_finalized_value_keep_open`, safe to retry as a whole).
    ///
    /// # What is a typed exit here and what is not
    ///
    /// `Ok(Err(exit))` — THIS WINDOW ended without an answer, and nothing
    /// about the driver is broken:
    /// - round exhaustion ([`InvocationExit::RoundsExhausted`]);
    /// - a suspension on a non-`finalize` hole, i.e. the window ended on
    ///   something that is not an answer ([`InvocationExit::NotFinalized`]);
    /// - the window's own provider call failing
    ///   ([`InvocationExit::RuntimeFailure`]).
    ///
    /// `Err(..)` — the MECHANISM is broken, and calling that "the model
    /// failed" would be a false receipt:
    /// - the per-loop inference-call cap (a runaway HARNESS, not a runaway
    ///   window — and it is shared, so the next child would trip it too);
    /// - a finalized CLOSURE. Note the difference from the cases above: the
    ///   window DID answer, and this driver cannot carry the answer it gave
    ///   (v1 scope). The gap is ours, so it fails as ours.
    /// - session/registry faults, and any `Harness` error that is not the
    ///   window's own compile (handled in-loop) or provider call.
    async fn drive_fanout_child_inner(
        &self,
        node: NodeId,
        site: u32,
        idx: usize,
        prompt: &str,
        element_ty: Option<&str>,
        table: &DataConTable,
    ) -> Result<Result<Value, InvocationExit>, DriverError> {
        self.agent
            .set_answer_contract(node, self.answer_contract(element_ty));
        let child_prompt = engine::answerer_hole_card(
            prompt,
            element_ty,
            self.answerer_imports(),
            Some(table),
            &self.agent.hole_card_effect_row(),
        );
        self.agent.push_user_turn(node, &child_prompt)?;
        self.emit(Event::TurnStart { node });

        let ty_label = element_ty.unwrap_or("A");
        let max_rounds = self.answerer_max_rounds;
        let nudge_rounds = self.answerer_nudge_rounds;
        let hard_rounds = max_rounds.saturating_add(2);
        let mut rounds: u32 = 0;
        let mut nudged = false;
        let mut ultimatum = false;
        loop {
            let cap = self.loop_inference_call_cap;
            if self.loop_inference_calls.load(Ordering::SeqCst) >= cap {
                return Err(DriverError::Session(format!(
                    "per-loop inference-call cap ({cap}) reached while servicing \
                     concurrent fanout child {idx} — hard-stopping the loop (a runaway \
                     harness)"
                )));
            }
            if rounds >= hard_rounds {
                // ROUND EXHAUSTION — this window's own budget, spent. Data at
                // the branch position, not an exception that would erase every
                // sibling's finished answer (PRD 21 locked decision 6).
                return Ok(Err(InvocationExit::RoundsExhausted(format!(
                    "fanout child {idx} exceeded {hard_rounds} rounds (cap {max_rounds} \
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
                        "You are approaching this window's round limit. Finalize now: \
                         evaluate `finalize @{} value` with your best answer.",
                        display_ty(ty_label)
                    ),
                )?;
                nudged = true;
            }

            self.loop_inference_calls.fetch_add(1, Ordering::SeqCst);
            rounds += 1;
            let outcome = self.agent.drive_turn(node).await;
            match &outcome {
                Ok(TurnOutcome::Suspended { .. } | TurnOutcome::Completed { .. }) => {
                    self.emit(Event::AnswererRound {
                        node,
                        site,
                        round: rounds,
                        error: None,
                    });
                }
                Err(HarnessError::Compile(msg)) => {
                    self.emit(Event::AnswererRound {
                        node,
                        site,
                        round: rounds,
                        error: Some(msg.clone()),
                    });
                }
                Ok(TurnOutcome::NoBlock { .. }) | Err(_) => {}
            }
            match outcome {
                Ok(TurnOutcome::Suspended { classified, .. })
                    if matches!(classified.routing, HoleRouting::Finalize { .. }) =>
                {
                    break;
                }
                Ok(TurnOutcome::Suspended { classified, .. }) => {
                    // NON-FINALIZATION — the window ended on something that is
                    // not an answer (a concurrent fanout/fork child cannot
                    // present an operator form, note, or nested fork in this
                    // driver, v1 scope), so it has no answer to give. Decision
                    // 6 names this class explicitly.
                    return Ok(Err(InvocationExit::NotFinalized(format!(
                        "fanout child {idx} suspended on a non-finalize hole ({:?}) — a \
                         concurrent fanout/fork child cannot present an operator form, \
                         note, or nested fork in this driver (v1 scope)",
                        classified.routing
                    ))));
                }
                Ok(TurnOutcome::Completed { .. }) => {
                    self.agent.reopen_node(node)?;
                    let ty_disp = display_ty(ty_label);
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "Round complete — your window continues, and that round's \
                             definitions/bindings persist. The request still awaits its \
                             answer: when ready, evaluate `finalize @{ty_disp} value` \
                             (that ends the window)."
                        ),
                    )?;
                }
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
                Err(HarnessError::Compile(msg)) => {
                    let hint = self.types_in_scope_hint(ty_label, &msg).unwrap_or_default();
                    let ty_disp = display_ty(ty_label);
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "A block did not compile — your window continues; everything \
                             that already ran persists. Reply with corrected ```haskell \
                             blocks. Another define/explore round is fine (top-level \
                             declarations are welcome and persist); when you are ready to \
                             answer, evaluate \
                             `finalize @{ty_disp} value`.\n\nGHC error:\n{msg}{hint}"
                        ),
                    )?;
                }
                // A provider fault is THIS WINDOW's own runtime failure —
                // decision 6's "runtime failure" class. Every other
                // `HarnessError` is driver/session machinery and hard-fails
                // the turn.
                Err(HarnessError::Engine(EngineError::Provider(pe))) => {
                    return Ok(Err(InvocationExit::RuntimeFailure(format!(
                        "fanout child {idx} provider call failed: {pe}"
                    ))));
                }
                Err(e) => return Err(e.into()),
            }
        }
        self.emit(Event::TurnEnd { node });

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

    /// Drive `node` (the per-loop answerer, already seeded with this hole's
    /// card) turn-by-turn until it suspends on `finalize`, applying the
    /// runaway caps: count each non-finalize model round; at
    /// [`ANSWERER_NUDGE_ROUNDS`] push a one-time "finalize now" nudge; at
    /// [`ANSWERER_MAX_ROUNDS`] hard-fail the hole; and abort the whole loop if
    /// the per-loop [`LOOP_INFERENCE_CALL_CAP`] is hit. A `Completed`
    /// (non-finalize) or `NoBlock` turn is treated as a wasted round —
    /// re-prompted toward `finalize` — rather than accepted, since the
    /// answerer's contract is to resolve the hole via `finalize`, not return a
    /// plain value.
    ///
    /// Each round `.await`s [`Harness::drive_turn`] directly — the resident
    /// JIT run it performs is CPU-blocking and sits inside this `async fn`
    /// unchanged; it already blocked a tokio worker before this method was
    /// `async` (called straight from async test bodies and `#[tokio::main]`
    /// with no bridge), so nothing about that changes here. It is not
    /// `spawn_blocking`'d: the resident session is not `Send`-shaped for
    /// that, and doing so is a separate piece of work.
    /// The return NESTING is the child-attributable/mechanism line, same
    /// contract as [`Self::drive_fanout_child_inner`]: `Ok(Err(exit))` means
    /// THIS WINDOW ended without an answer (round exhaustion, its own
    /// provider call failing), `Err(..)` means the mechanism is broken (the
    /// per-loop inference-call cap, session faults). Whether an exit is DATA
    /// or fatal is the CALLER's to decide, because it depends on whether the
    /// window sits at a branch position: [`Self::service_outer_branch`] folds
    /// it as `Left` at that branch, while [`Self::service_runllm_hole`] —
    /// answering IN CONTEXT on the outer turn's own continuation, with no
    /// siblings and no position — still hard-fails, exactly as before.
    async fn drive_answerer_to_finalize(
        &self,
        node: NodeId,
        ty: Option<&str>,
        site: u32,
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
        loop {
            let cap = self.loop_inference_call_cap;
            if self.loop_inference_calls.load(Ordering::SeqCst) >= cap {
                return Err(DriverError::Session(format!(
                    "per-loop inference-call cap ({cap}) reached — \
                     hard-stopping the loop (a runaway harness)"
                )));
            }
            if rounds >= hard_rounds {
                // ROUND EXHAUSTION — this window's own budget, spent. Data
                // for a caller that has a branch position to fold it at;
                // `service_runllm_hole` still turns it into a hard failure.
                return Ok(Err(InvocationExit::RoundsExhausted(format!(
                    "runLLMTurn answerer exceeded {hard_rounds} rounds (cap {max_rounds} \
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
                        "You are approaching this window's round limit. Finalize now: \
                         evaluate `finalize @{} value` with your best answer.",
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
            // block went on to do. `NoBlock` never reaches a compile, so it is
            // not a round for this fold's purposes.
            match &outcome {
                Ok(TurnOutcome::Suspended { .. } | TurnOutcome::Completed { .. }) => {
                    self.emit(Event::AnswererRound {
                        node,
                        site,
                        round: rounds,
                        error: None,
                    });
                    // Show the operator what the answerer actually ran —
                    // once per COMPILED round (a failed compile has no
                    // executed source to show; `post_turn_source` is a
                    // default-no-op on headless gates). Routed per-node like
                    // asks/notes: a labeled branch child's turns belong on
                    // its own section, not the default one.
                    if let Some(src) = self.agent.last_turn_source(node) {
                        self.resolve_gate(&FormSource::Answerer { node })
                            .post_turn_source(&src);
                    }
                }
                Err(HarnessError::Compile(msg)) => {
                    self.emit(Event::AnswererRound {
                        node,
                        site,
                        round: rounds,
                        error: Some(msg.clone()),
                    });
                }
                Ok(TurnOutcome::NoBlock { .. }) | Err(_) => {}
            }
            match outcome {
                Ok(out @ TurnOutcome::Suspended { .. }) => {
                    // A Finalize suspension is the answer. An AskUser suspension
                    // (operator gui) is SERVICED here via the operator gate
                    // (`service_askuser_hole`, looping on askUser's Haskell-side
                    // decode-failure re-prompt). A Fork suspension (`forkAll`/
                    // `fork` delegation) is serviced via the EXISTING fanout/fork
                    // machinery (`drain_answerer_fork`, REUSED not reimplemented).
                    // A Note suspension (`note text`, display-only) is drained
                    // FIRST, purely via resumes (no model round): the block may
                    // read `note "..." >> choose [...]`, so the FIRST classified
                    // hole here is routinely `Note`, not the thing that follows
                    // it. Any OTHER suspension is a hard error: the scoped
                    // answerer stack (`[AskUser, Fork, ReadState, Finalize]`) can reach
                    // nothing else, and this driver has no operator for it.
                    let TurnOutcome::Suspended { hole, classified } = out else {
                        unreachable!("matched TurnOutcome::Suspended above");
                    };
                    let (hole, classified) =
                        match self.drain_note_holes(node, hole, classified).await? {
                            Some(pair) => pair,
                            None => {
                                // The note chain resolved (the answerer's block
                                // completed) WITHOUT finalize — same corrective
                                // retry as a plain Completed turn below. A note
                                // resume is NOT a model round: `rounds` stays
                                // untouched, only this outer loop repeats.
                                self.agent.reopen_node(node)?;
                                self.agent.push_user_turn(
                                    node,
                                    &format!(
                                        "That did not resolve the request. Answer by \
                                         evaluating `(finalize @{ty_label} value :: M \
                                         {ty_label})` — the whole expression must carry \
                                         the type annotation, not just the argument."
                                    ),
                                )?;
                                continue;
                            }
                        };
                    if matches!(classified.routing, HoleRouting::Finalize { .. }) {
                        return Ok(Ok(TurnOutcome::Suspended { hole, classified }));
                    }
                    if let HoleRouting::AskUser { shape } = &classified.routing {
                        match self.service_askuser_hole(node, shape).await? {
                            Some(finalize_outcome) => return Ok(Ok(finalize_outcome)),
                            None => {
                                // The askUser chain resolved (the answerer's block
                                // completed) WITHOUT finalize — same corrective
                                // retry as a plain Completed turn below. A form
                                // resume is NOT a model round (see
                                // `service_askuser_hole`'s doc): `rounds` stays
                                // untouched, only this outer loop repeats.
                                self.agent.reopen_node(node)?;
                                self.agent.push_user_turn(
                                    node,
                                    &format!(
                                        "That did not resolve the request. Answer by \
                                         evaluating `(finalize @{ty_label} value :: M \
                                         {ty_label})` — the whole expression must carry \
                                         the type annotation, not just the argument."
                                    ),
                                )?;
                                continue;
                            }
                        }
                    }
                    // The answerer delegated to `forkAll`/`fork`: service it via
                    // the existing fanout/fork machinery (REUSED, not
                    // reimplemented) rather than handing it to an operator that
                    // doesn't exist here.
                    if matches!(classified.routing, HoleRouting::Fork { .. }) {
                        if let Some(out) = self.drain_answerer_fork(node, ty_label).await? {
                            return Ok(Ok(out));
                        }
                        // The parent completed without ever finalizing —
                        // `drain_answerer_fork` already reopened the node and
                        // pushed a corrective nudge. Keep driving.
                        continue;
                    }
                    // A non-finalize, non-askUser, non-fork suspension: the
                    // answerer parked awaiting input this driver cannot service.
                    // Hard error rather than silently hanging.
                    return Err(DriverError::Session(format!(
                        "runLLMTurn answerer suspended on a non-finalize, non-askUser, \
                         non-fork hole ({:?}) — the self-harness driver has no operator \
                         to answer it",
                        classified.routing
                    )));
                }
                // A plain value: the block ran to completion WITHOUT
                // `finalize`, so the node is now `Done`. Reopen it
                // (`Done`→`Running`) before the corrective re-prompt, so the
                // same accumulating node keeps driving toward `finalize` (a
                // wasted round, already counted).
                Ok(TurnOutcome::Completed { .. }) => {
                    // A completed non-finalize round is a VALID explore/define
                    // round, not a failure — the window is multi-round by
                    // design, and scolding here taught the model that only
                    // `finalize` is admitted (companion dogfood, 2026-08-13:
                    // it reported exactly that, accurately). Acknowledge and
                    // keep the request standing.
                    self.agent.reopen_node(node)?;
                    let ty_disp = display_ty(ty_label);
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "Round complete — your window continues, and that round's \
                             definitions/bindings persist. The request still awaits its \
                             answer: when ready, evaluate `finalize @{ty_disp} value` \
                             (that ends the window)."
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
                    let hint = self.types_in_scope_hint(ty_label, &msg).unwrap_or_default();
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
                            "A block did not compile — your window continues; everything \
                             that already ran persists. Reply with corrected ```haskell \
                             blocks. Another define/explore round is fine (top-level \
                             declarations are welcome and persist); when you are ready to \
                             answer, evaluate \
                             `finalize @{ty_disp} value`.\n\nGHC error:\n{msg}{hint}"
                        ),
                    )?;
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
    /// Returns `Ok(Some(outcome))` when the chain resolves to a `Finalize`
    /// suspension — built from [`Harness::pending_hole_full`] read right
    /// after the resume, since `answer_dialog` itself returns no outcome —
    /// the caller returns it as the hole's answer. Returns `Ok(None)` when a
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
            self.agent.answer_dialog(node, submission).await?;

            let Some((hole, classified, _table)) = self.agent.pending_hole_full(node) else {
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
            if matches!(classified.routing, HoleRouting::Finalize { .. }) {
                return Ok(Some(TurnOutcome::Suspended { hole, classified }));
            }
            if let HoleRouting::AskUser { shape: next_shape } = classified.routing {
                shape = next_shape;
                continue;
            }
            return Err(DriverError::Session(
                "runLLMTurn answerer suspended on a non-finalize, non-askUser hole \
                 after an operator form resume — the self-harness driver has no \
                 operator to answer it"
                    .to_string(),
            ));
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
    /// `HoleRouting::AskUser` (a typed form) or `HoleRouting::Note`
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
    /// `block_in_place`: `CodexAgentBackend` owns its own runtime and
    /// `block_on`s it — the same discipline every `OperatorGate` call uses.
    /// A lane-1 coupled spawn blocks this loop turn for the agent's whole
    /// cycle (~30–120s live), by design (`plans/companion-memory.md`).
    fn service_outer_subagent(
        &self,
        request: &Value,
        table: &DataConTable,
    ) -> Result<Value, DriverError> {
        let mut handlers = self.handlers.lock();
        let handler = handlers.subagent.as_mut().ok_or_else(|| {
            DriverError::Session(
                "the authored loop called a Subagent verb (spawnAgent/spawnAgentRaw) but no \
                 subagent handler is configured — wire one with \
                 SelfHarnessDriver::set_subagent_handler (the tidepool-selfharness binary \
                 does this when TIDEPOOL_MEMORY_REPO is set)"
                    .into(),
            )
        })?;
        let started = std::time::Instant::now();
        let value = Self::dispatch_outer_effect(handler, request, table)
            .map_err(|e| DriverError::Session(format!("subagent dispatch: {e}")))?;
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            "outer subagent suspension serviced"
        );
        Ok(value)
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
    /// `HoleRouting::OuterEffect` servicing arm (PRD 20 S1-L4 wave 2): decode
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
        routing: HoleRouting,
        compiled: &CompiledTurn,
    ) -> Result<ResidentOutcome, DriverError> {
        let mut hole = hole;
        let mut routing = routing;
        let mut reprompts: u32 = 0;
        loop {
            let outcome = match routing {
                HoleRouting::AskUser { shape } => {
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
                HoleRouting::Note { text } => {
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
                        HoleRouting::AskUser { .. } | HoleRouting::Note { .. }
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

    /// Drain a leading run of `note` holes on `node`, starting from
    /// `classified` (which may or may not already be `HoleRouting::Note` —
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
        mut classified: ClassifiedHole,
    ) -> Result<Option<(String, ClassifiedHole)>, DriverError> {
        loop {
            match classified.routing.clone() {
                HoleRouting::Note { text } => {
                    self.service_note_hole(node, &text).await?;
                }
                HoleRouting::ReadState => {
                    // Immediate resume with the cycle's entry state — no
                    // operator, no model round (note's service shape).
                    let state = self.cycle_state_json.clone().unwrap_or(Json::Null);
                    self.agent.answer_dialog(node, state).await?;
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
                HoleRouting::Subagent => {
                    let (pending_hole, _classified, table, request) =
                        self.agent.pending_hole_with_request(node).ok_or_else(|| {
                            DriverError::Session(format!(
                                "node {node:?} has no pending Subagent hole to service"
                            ))
                        })?;
                    let value = self.service_outer_subagent(&request, &table)?;
                    // PRD 21 C5: a `delegate` call's `SubagentAwait` is
                    // serviced on THIS exact node's own turn, so any branch
                    // it decodes is unambiguously this node's own —
                    // recorded under the domain path `service_outer_branch`
                    // stamped for it (a harness whose branch children don't
                    // carry that stamp, or a completion this isn't
                    // `SubagentAwait`/doesn't decode, simply records
                    // nothing).
                    if engine::con_name(&request, &table) == Some("SubagentAwait") {
                        if let Some(branch) =
                            engine::decode_completed_delegation_branch(&value, &table)
                        {
                            if let Some(path) = self.branch_node_paths.lock().get(&node).cloned() {
                                self.delegated_branches
                                    .lock()
                                    .entry(path)
                                    .or_default()
                                    .push(branch);
                            }
                        }
                    }
                    self.agent
                        .resume_with_value(node, &pending_hole, value)
                        .await?;
                }
                _ => break,
            }
            match self.agent.pending_hole_full(node) {
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

    /// Drain a `HoleRouting::Fork` suspension on the per-loop answerer
    /// (`forkAll`/`fork` via `Tidepool.Fork`): resume it via the EXISTING
    /// [`Harness::answer_fanout`]/[`Harness::answer_fork`] machinery — REUSED,
    /// never reimplemented — looping in case the parent immediately hits
    /// ANOTHER fork right after resuming (e.g. `forkAll` then `fork` in
    /// sequence). `Ok(Some(out))` means the parent landed on `Finalize` — the
    /// caller should `return Ok(out)` straight through, same as any other
    /// finalize suspension. `Ok(None)` means the parent's block ran to
    /// completion WITHOUT ever finalizing; this already reopened the node and
    /// pushed the same corrective nudge [`Self::drive_answerer_to_finalize`]'s
    /// `Completed` arm uses, so the caller should just let its round loop
    /// keep driving. Any other resumed hole (an operator form) or a
    /// mid-fanout child that itself suspended
    /// ([`crate::harness::HarnessError::Aborted`], surfaced from
    /// `answer_fanout`/`answer_fork` via `?`) is a hard error — the
    /// self-harness driver has no operator inside a fork child (v1).
    async fn drain_answerer_fork(
        &self,
        node: NodeId,
        ty_label: &str,
    ) -> Result<Option<TurnOutcome>, DriverError> {
        loop {
            let routing = self
                .agent
                .pending_hole(node)
                .map(|c| c.routing)
                .ok_or_else(|| {
                    DriverError::Session("fork resume: node has no pending hole to service".into())
                })?;
            match routing {
                HoleRouting::Fork { fan: Some(_), .. } => {
                    self.agent.answer_fanout(node, Actor::Operator).await?;
                }
                HoleRouting::Fork { fan: None, .. } => {
                    self.agent.answer_fork(node, Actor::Operator).await?;
                }
                other => {
                    return Err(DriverError::Session(format!(
                        "drain_answerer_fork: expected a pending Fork hole, got {other:?}"
                    )));
                }
            }

            match self.agent.pending_hole(node).map(|c| c.routing) {
                Some(HoleRouting::Finalize { .. }) => {
                    return self
                        .agent
                        .pending_turn_outcome(node)
                        .map(Some)
                        .ok_or_else(|| {
                            DriverError::Session("fork resume: finalize pending vanished".into())
                        });
                }
                Some(HoleRouting::Fork { .. }) => continue,
                Some(other) => {
                    return Err(DriverError::Session(format!(
                        "fork answerer resumed onto a non-finalize/non-fork hole \
                         ({other:?}) — no operator to answer it"
                    )));
                }
                None => break,
            }
        }

        self.agent.reopen_node(node)?;
        self.agent.push_user_turn(
            node,
            &format!(
                "The fork results did not resolve the request. Answer by evaluating \
                 `(finalize @{ty_label} value :: M {ty_label})` — the whole expression \
                 must carry the type annotation, not just the argument."
            ),
        )?;
        Ok(None)
    }

    /// Evaluate `render(state)` against the outer session, then compose the
    /// full system message the answerer works under — author output first,
    /// then the prior compaction summary (if any), then the loop-iteration
    /// count. This composed text becomes `prompt_before`/`prompt_after`; it
    /// carries NO effects section of its own — the OUTER loop's own
    /// Available-effects section (folded over [`outer_decls`]) is never
    /// shown to the nested answerer, which sees only its own row's section
    /// (appended by the caller that builds `self.answerer_framing`, via
    /// [`answerer_framing_suffix`]). `render` itself takes only `State`
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
    /// its `Text` result. Split out so [`Self::compile_cycle_entry`]'s fused
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
            framing.push_str("\n\nSummary of the prior window:\n");
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
    ///    [`CycleOutcome::compaction`]) and `self.last_compaction` (carried to
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
            "This work window has grown to roughly {context_tokens} tokens against a \
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
    use super::{answerer_decls, outer_decls};

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
    /// deliberately absent from `answerer_decls()`, and `dialogAsk` is
    /// deleted outright). Pure string-level check, no GHC needed.
    #[test]
    fn answerer_effects_module_declares_askuser_not_ask() {
        let src = tidepool_mcp::effects_core_module_source(&answerer_decls());
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
    /// compiling row (`answerer_decls()` — `AskUser`/`Fork`/`Finalize`) via
    /// the decl-driven fold (`engine::available_effects_section`), not a
    /// hand-written parenthetical. Pure string check, no GHC needed.
    #[test]
    fn answerer_framing_suffix_names_every_verb_of_the_answerer_row() {
        let framing = super::answerer_framing_suffix();
        for decl in answerer_decls() {
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
    }

    /// The outer loop's own decl row (`outer_decls()` — `RunLLMTurn`/
    /// `AskUser`) is a DIFFERENT row from the answerer's
    /// (`AskUser`/`Fork`/`Finalize`), so the two surfaces' generated
    /// sections must differ — each folds over its own compiling row, not a
    /// shared hand-written table.
    #[test]
    fn outer_and_answerer_available_effects_sections_differ_by_row() {
        let outer_section = crate::engine::available_effects_section(&super::outer_decls());
        let answerer_section = crate::engine::available_effects_section(&answerer_decls());

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
            "'[RunLLMTurn, AskUser, Console, Finalize NoAnswer]",
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
}
