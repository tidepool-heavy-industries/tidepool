//! The runtime driver spine — the outer `render`/`loop` alternation over the
//! OUTER Harness-monad resident session, servicing each `runLLMTurn` hole by
//! driving a per-loop answerer [`crate::harness::Harness`] node (an ordinary
//! Agent node, reusing `run_to_hole_or_done`) to a `finalize` and delivering
//! the result back in-heap to resume `loop`.
//!
//! ONE SESSION: the outer
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
//! [`Harness::take_live_payload_handle_keep_open`] — a
//! custody of the payload's own machine-side root, never deep-forced or
//! serialized — delivered straight
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
//! # Boot fold and entry selection
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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value as Json;
use tidepool_eval::value::Value;

use crate::engine::{self, TurnOutcome};
use crate::harness::{Harness, HarnessError};
use crate::selfharness::lifecycle::SelfHarnessState;
use crate::selfharness::observer::{Event, Observer};
use crate::selfharness::operator::{OperatorGate, StdinGate};
use crate::selfharness::persistence::{self, PersistenceError};
use crate::selfharness::state_cross;
use crate::tree::NodeId;

mod contract;
mod corrective;
mod delegate;
mod fork;
mod green;
mod lifecycle;
mod suspension;

pub use contract::{typed_request_agent_decls, typed_request_agent_decls_with_delegate};
// The outer session's own bootstrapped state now lives in `lifecycle`; kept
// in this module's namespace since it names a struct field below.
use lifecycle::OuterSession;

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
/// verbatim on the shared heap — the closure path (the reason the
/// collapse exists).
pub enum FinalAnswer {
    Value(Value),
    Handle(tidepool_runtime::session::RootCustody),
}

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
/// How deep model-driven forking may nest: a session at depth 8 may not fork
/// further. Depth alone is not the real bound — the subtree budget below
/// is — but it caps pathological chains.
const DEFAULT_MAX_FORK_DEPTH: u32 = 8;

/// Total DESCENDANT sessions one top-level agent session's whole fork tree
/// may spawn, counted atomically at every spawn across all depths and both
/// fork styles (direct + green-thread). The real resource bound.
const DEFAULT_FORK_SUBTREE_CAP: u32 = 32;

/// This is a total-per-node budget. Multi-wave forking (fork, fold, then fork
/// again within one window) should fit without per-harness tuning.
const DEFAULT_FORK_BUDGET_PER_SESSION: u32 = 32;

/// Default cap on how many `RunLLMTurn` fanout/fork children
/// ([`SelfHarnessDriver::service_outer_fanout`]) may be concurrently
/// mid-window (concurrent cognition windows) — each in its
/// own freshly-minted answerer realm on the shared outer machine. Only
/// machine occupancy serializes past this point (a window spends most of
/// its wall time in provider inference, off-machine, with nothing checked
/// out); this bounds how many windows may be open — and contending for the
/// machine when their turn comes — at once. Configurable via
/// [`SelfHarnessDriver::set_concurrency_cap`].
const DEFAULT_CONCURRENCY_CAP: usize = 8;

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
/// [`Event::MachineStats`] evidence.
const DEFAULT_FRAGMENT_CEILING: u64 = 4096;

pub struct SelfHarnessDriver {
    /// The Harness-monad resident session. `None` before bootstrap.
    outer: Option<OuterSession>,
    /// The living session values the LAST machine rotation lost — surfaced
    /// once in the next render as a legible-loss note, then cleared.
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
    /// `RunLLMTurn` windows
    /// ([`Self::service_outer_fanout`]) — default [`DEFAULT_CONCURRENCY_CAP`]
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
    /// of the authored `State` (runtime context is the runtime's own job).
    /// `0` before any cycle has completed. Incremented once per successful
    /// [`Self::run_one_loop_iteration`], right after that cycle's `loop` completes;
    /// fed into [`Self::render_framing`]'s composed loop-metadata line and
    /// persisted in the checkpoint envelope ([`Self::commit_checkpoint`]) —
    /// never in `state_json` — so a restart resumes counting from the right
    /// number ([`Self::restore`]).
    iteration: u64,
    /// Monotonic ownership epochs for cycle-scoped RepoEvent subscriptions.
    /// Separate from the successful-iteration counter: failed cycles must
    /// still receive a distinct owner and be cleaned up conclusively.
    event_owner_epoch: u64,
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
    /// Each is driver-owned rather than installed on the outer session, which
    /// uses `SuspendAll`; a suspension against an unwired handler fails
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
    /// bounds the blast radius to Subagent alone. `Arc`-wrapped so
    /// [`Self::service_outer_subagent`] can clone the handle into a
    /// `tokio::task::spawn_blocking` closure (`spawn_blocking` needs a
    /// `'static` closure; a plain `&Mutex` borrowed from `&self` cannot
    /// cross that boundary) — see that method's doc for why the dispatch
    /// itself still serializes on this ONE lock (the handler's own
    /// `SubagentSpawn`/`SubagentAwait` methods take `&mut self` for their
    /// full synchronous duration; `tidepool-handlers`/`tidepool-agent` are
    /// out of this crate's reach), and
    /// `CONCURRENT_SIBLINGS_SPIKE_FINDINGS.md`'s addendum for why that still
    /// yields real overlap for `spawnAgent`'s two-suspension
    /// (`spawnAsync`/`awaitAgent`) shape. Two siblings delegating in the
    /// same bulk window (`Self::drain_note_holes`'s concurrent
    /// `runLLMTurnBranchFanout` case) still serialize on THIS lock — the same
    /// synchronous handler this driver has always called, never made to run
    /// two dispatches at once — that part is unchanged. Wire it via
    /// [`Self::set_subagent_handler`].
    subagent: Arc<Mutex<Option<tidepool_handlers::SubagentHandler>>>,
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
    /// Which per-node operator-gate label a
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
    /// The next `idx` to assign
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
    /// checking first.
    fork_child_seq: Mutex<HashMap<NodeId, u32>>,
    /// The operator listen channel's publisher handle, wired by the
    /// composition root via [`Self::set_listen_server`] — `None` until then
    /// (and always `None` for an embedder/test driver that never wires one).
    /// Deliberately NOT threaded into [`OperatorGate`]: that trait is the
    /// frozen ask/form contract, and the listen channel is a separate
    /// outbound-notify mechanism (`crate::listen`) future harness code can
    /// publish through via [`Self::listen_server`].
    listen: Option<Arc<crate::listen::ListenServer>>,
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

/// Which of the two answerer-window modes a node is running under:
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
        realm: tidepool_codegen::suspension::RealmId,
    },
    /// A `runLLMTurnBranch` child's mode: answers exactly once, then is
    /// frozen and retired.
    OneShotBranch {
        node: NodeId,
        realm: tidepool_codegen::suspension::RealmId,
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
    /// [`Harness::take_live_payload_handle_keep_open`]) is meaningless applied
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
            tidepool_codegen::suspension::RealmId,
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

impl SelfHarnessDriver {
    /// Construct a driver over an already-booted [`Harness`] (the nested
    /// orchestrator for `runLLMTurn`-answering Agent sessions) and an event
    /// [`Observer`]. The outer Harness-monad session itself is not
    /// bootstrapped until the first [`Self::run_loop`]/[`Self::run_one_loop_iteration`]
    /// call (it needs the loaded [`HarnessSource`] first).
    pub fn new(agent: Arc<Harness>, observer: Arc<dyn Observer>) -> Self {
        SelfHarnessDriver {
            outer: None,
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
            event_owner_epoch: 0,
            gate: Arc::new(StdinGate),
            ask_id_counter: AtomicU64::new(0),
            handlers: Mutex::new(OuterHandlers::default()),
            subagent: Arc::new(Mutex::new(None)),
            resume: None,
            node_labels: Mutex::new(HashMap::new()),
            fork_child_seq: Mutex::new(HashMap::new()),
            listen: None,
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
    /// Retiring means removing the session from the registry through
    /// [`crate::harness::Harness::retire_adopted_session`], not merely dropping
    /// this struct's `sid`. The registry owns the machine, heap, code arena,
    /// and parked frames; a later bootstrap creates a different session id.
    fn discard_resident_state(&mut self) {
        self.retire_typed_request_agent();
        self.answerer_framing = None;
        self.cycle_compaction = None;
        self.loop_inference_calls.store(0, Ordering::SeqCst);
        if let Some(outer) = self.outer.take() {
            self.agent.retire_adopted_session(outer.sid);
        }
    }

    /// Override the operator-input gate (default [`StdinGate`]). A web/GUI
    /// implementation of [`OperatorGate`] replaces the headless stdin
    /// behavior; a test can inject a scripted gate instead of driving real
    /// stdin.
    pub fn set_gate(&mut self, gate: Arc<dyn OperatorGate>) {
        self.gate = gate;
    }

    /// Wire the operator listen channel's publisher handle — the
    /// composition root constructs a [`crate::listen::ListenServer`] at boot
    /// (see `tidepool-selfharness`'s `main`) and hands it here so any future
    /// harness code can publish outbound frames via [`Self::listen_server`].
    pub fn set_listen_server(&mut self, listen: Arc<crate::listen::ListenServer>) {
        self.listen = Some(listen);
    }

    /// The wired listen-channel publisher, if any — `None` until
    /// [`Self::set_listen_server`] has been called.
    pub fn listen_server(&self) -> Option<&Arc<crate::listen::ListenServer>> {
        self.listen.as_ref()
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
        self.handlers.lock().journal = Some(
            tidepool_handlers::JournalHandler::resuming(
                acquired.segment.clone(),
                acquired.segment_ordinal,
            )
            .map_err(|e| DriverError::Session(format!("journal segment header stamp: {e}")))?,
        );
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

    /// Emit `event` to the configured [`Observer`] — the ONE place the
    /// driver touches the observer, so no call site hardwires logging or a
    /// future GUI push directly.
    fn emit(&self, event: Event) {
        self.observer.on_event(&event);
    }
}

#[cfg(test)]
mod tests {
    use super::contract::{outer_decls, outer_template};
    use super::corrective::typed_request_agent_framing_suffix;
    use super::fork::{fork_child_path_segment, FORK_LABEL_SLUG_BUDGET};
    use super::typed_request_agent_decls;

    /// The outer row exposes every effect family serviced by the driver.
    /// Suspension is selected explicitly on the resident session, not encoded
    /// in this declaration order.
    #[test]
    fn outer_row_contains_driver_serviced_effects() {
        let decls = outer_decls();
        assert!(decls.iter().any(|d| d.type_name == "RunLLMTurn"));
        assert!(decls.iter().any(|d| d.type_name == "AskUser"));
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

    /// Universal Core names both request effects, while the answerer's
    /// concrete row grants `AskUser` but not `Ask`.
    #[test]
    fn answerer_row_grants_askuser_not_ask() {
        let src = tidepool_mcp::effects_core_module_source();
        assert!(
            src.contains("data AskUser a where"),
            "expected an AskUser GADT declaration, got:\n{src}"
        );
        assert!(
            src.contains("askUserRaw"),
            "expected the askUserRaw helper, got:\n{src}"
        );
        assert!(
            src.contains("data Ask a where"),
            "universal Core must declare the Ask GADT, got:\n{src}"
        );
        let row = typed_request_agent_decls();
        assert!(row.iter().any(|decl| decl.type_name == "AskUser"));
        assert!(!row.iter().any(|decl| decl.type_name == "Ask"));
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
        let framing = typed_request_agent_framing_suffix(
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
        // State the tree-wide descendant budget up front rather than waiting
        // for a refusal to reveal it.
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
        let plain = typed_request_agent_framing_suffix(
            &typed_request_agent_decls(),
            super::DEFAULT_FORK_BUDGET_PER_SESSION,
            super::DEFAULT_FORK_SUBTREE_CAP,
        );
        assert!(
            !plain.contains("**Subagent**") && !plain.contains("**Worktree**"),
            "the plain row's framing must not mention Subagent/Worktree, got:\n{plain}"
        );
        let delegating = typed_request_agent_framing_suffix(
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
        let outer_section = crate::engine::available_effects_section(&outer_decls());
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
        let src = outer_template(
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

    /// `fork_child_path_segment` slugs a model-authored fork brief into a
    /// GUI/DOM node id segment —
    /// `tidepool-web`'s loopback trust model rests on "`node_id` is always a
    /// substrate identifier, never model-produced text"
    /// (`tidepool-web/src/lib.rs` interpolates `id="panel-<node_id>"` into
    /// the DOM). The hostile corpus pins containment directly on the pure
    /// slugging function.
    #[test]
    fn fork_child_path_segment_contains_hostile_briefs() {
        const HOSTILE_TITLE: &str = "Beta!! <b>risk</b> ünïcode";
        const HOSTILE_FRAGMENTS: [&str; 5] = ["!", "<", ">", "/b", "ü"];
        let long_brief = "x".repeat(FORK_LABEL_SLUG_BUDGET * 2);

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
            let segment = fork_child_path_segment(idx, brief);

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
                    !slug.is_empty() && slug.len() <= FORK_LABEL_SLUG_BUDGET,
                    "slug {slug:?} (from brief {brief:?}) must be 1..={} chars, got segment \
                     {segment:?}",
                    FORK_LABEL_SLUG_BUDGET
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
        assert_eq!(fork_child_path_segment(2, ""), "f2");
        assert_eq!(fork_child_path_segment(3, "!!!@@@###"), "f3");
        assert_eq!(fork_child_path_segment(4, "   "), "f4");
    }
}
