//! The runtime driver spine — the outer `render`/`loop` alternation over the
//! OUTER Harness-monad resident session, servicing each `runLLMTurn` hole by
//! driving a NESTED
//! [`crate::harness::Harness`] (an ordinary Agent node, reusing
//! `run_to_hole_or_done`) to a `finalize` and `run_child`-ing the result
//! back in-heap to resume `loop`.
//!
//! The outer session is an ordinary resident session on the one threadless
//! (stow-as-data) suspension engine, the same mechanism `Harness`'s own nodes
//! use. Heaps are never
//! copied — the nested Agent's `finalize` value crosses via
//! [`Harness::take_finalized_value`] (deep-forced out of its own heap, never
//! JSON) and is fed straight into [`ResidentSession::resume`] to resume the
//! OUTER session's parked `runLLMTurn` continuation.
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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value as Json;
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::ResidentOutcome;

use crate::compile::{self, CompiledTurn};
use crate::engine::{self, ClassifiedHole, EngineConfig, HoleRouting, TurnOutcome};
use crate::harness::{AnswerContract, Harness, HarnessError};
use crate::log::Actor;
use crate::selfharness::harness_source::HarnessSource;
use crate::selfharness::lifecycle::SelfHarnessState;
use crate::selfharness::observer::{Event, FormSource, Observer};
use crate::selfharness::operator::{FormShape, OperatorGate, StdinGate};
use crate::selfharness::persistence::{self, PersistenceError};
use crate::selfharness::state_cross;
use crate::timing;
use crate::tree::NodeId;

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("self-harness driver: {0}")]
    Session(String),
    #[error(transparent)]
    Agent(#[from] HarnessError),
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
}

/// Map an outer-session run error string to a typed [`DriverError`]: a message
/// carrying [`state_cross::STATE_DECODE_SENTINEL`] becomes
/// [`DriverError::StateDecode`], everything else a generic
/// [`DriverError::Session`] with `ctx` for locus.
fn map_run_error(ctx: &str, msg: String) -> DriverError {
    if let Some(idx) = msg.find(state_cross::STATE_DECODE_SENTINEL) {
        let detail = &msg[idx + state_cross::STATE_DECODE_SENTINEL.len()..];
        DriverError::StateDecode(detail.trim().to_string())
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
enum FinalAnswer {
    Value(Value),
    Handle(tidepool_codegen::jit_machine::ValueHandle),
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
fn outer_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![
        tidepool_mcp::runllmturn_decl(),
        tidepool_mcp::askuser_decl(),
    ]
}

/// The nested answerer Agent's scoped decl row: `[AskUser, Fork, Finalize]`.
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
        tidepool_mcp::finalize_decl(),
    ]
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
         Your ONLY runnable output is a single fenced ```haskell block containing one \
         expression of type `M a`. A value you bind with `x <- …` persists into your \
         NEXT turn like GHCi, so you can branch on it.\n\
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
        engine::available_effects_section(&answerer_decls())
    )
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
pub struct SelfHarnessDriver {
    /// The Harness-monad resident session. `None` before bootstrap.
    outer: Option<OuterSession>,
    /// Monotonic per-loop realm counter: each loop's answerer node gets its
    /// own realm on the SHARED machine (structured-concurrency scope; closed
    /// at retirement). Distinct from `iteration` (which restarts restore).
    iteration_realm: u64,
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
    /// session. `None` between loops.
    answerer: Option<NodeId>,
    /// Total model inference calls across the CURRENT loop's holes + rounds:
    /// reset in [`Self::run_loop_fragment`], incremented
    /// per answerer `drive_turn`. The loop hard-stops with a [`DriverError`]
    /// if it reaches [`LOOP_INFERENCE_CALL_CAP`].
    loop_inference_calls: u32,
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
    /// restored — `0` before either has happened. A commit writes
    /// `checkpoint_generation + 1` and then adopts it, so generation
    /// increases by exactly one per committed cycle and stays monotonic
    /// across a restart (restore adopts the reloaded generation first).
    checkpoint_generation: u64,
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
            iteration_realm: 0,
            agent,
            lifecycle: SelfHarnessState::Idle,
            observer,
            last_compaction: None,
            cycle_compaction: None,
            compaction_threshold_percent: DEFAULT_COMPACTION_THRESHOLD_PERCENT,
            answerer_framing: None,
            answerer: None,
            loop_inference_calls: 0,
            answerer_nudge_rounds: ANSWERER_NUDGE_ROUNDS,
            answerer_max_rounds: ANSWERER_MAX_ROUNDS,
            loop_inference_call_cap: LOOP_INFERENCE_CALL_CAP,
            checkpoint_path: persistence::default_checkpoint_path(),
            checkpoint_generation: 0,
            iteration: 0,
            gate: Arc::new(StdinGate),
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
        self.loop_inference_calls = 0;
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
    /// stdin.
    pub fn set_gate(&mut self, gate: Arc<dyn OperatorGate>) {
        self.gate = gate;
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

        let handler_cfg = tidepool_handlers::HandlerConfig {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            kv_path: tidepool_runtime::paths::cache_dir().join("selfharness-kv.json"),
            llm_model: std::env::var("TIDEPOOL_LLM_MODEL")
                .unwrap_or_else(|_| "gpt-4o-mini".to_string()),
        };
        // Never actually dispatched to: `outer_cfg.suspend_tag == 0` means the
        // ONE declared effect (RunLLMTurn) always suspends before reaching a
        // handler.
        let stack: crate::harness::BoxedStack =
            Box::new(tidepool_handlers::build_base_stack(&handler_cfg));

        let session = crate::harness::Session::unbootstrapped(
            stack,
            outer_cfg.suspend_tag,
            outer_cfg.effect_names.clone(),
            tidepool_mcp::CapturedOutput::new(),
            outer_cfg.include.clone(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            None,
        );

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
        let src = engine::template_turn_for(&outer_decls(), &stack, code, &imports, helpers);
        self.emit(Event::OuterCompile {
            label: label.to_string(),
            source: src.clone(),
        });
        compile::compile_turn(
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

    /// Compile the PRE-loop `render` and this cycle's `loop` fragment as TWO
    /// entries of ONE module, in a SINGLE `tidepool-extract` spawn
    /// ([`compile::compile_turns`]) — the pre-model boot-path fusion this
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
    /// targets share ONE `meta.cbor` (`compile::compile_turns`'s whole point),
    /// so the render entry's [`CompiledTurn::table`] already carries the loop
    /// entry's constructors — including the `RunLLMTurn` ConTags the machine
    /// needs once `loop` starts suspending on holes.
    ///
    /// Returns `(render_turn, loop_turn)`. Does NOT run either — that stays
    /// [`Self::render_framing`]/[`Self::run_loop_fragment_inner`]'s job, so a
    /// caller can compile once and run each entry through its own existing
    /// path.
    fn compile_cycle_entry(
        &mut self,
        prior_state: Option<&Json>,
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

        let helpers = state_cross::state_in(prior_state);
        let render_code = format!(
            "pure ({q}.render __selfHarnessState)",
            q = state_cross::LOADED_QUALIFIER
        );
        let loop_code = format!("{}.loop __selfHarnessState", state_cross::LOADED_QUALIFIER);
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
        let mut turns = compile::compile_turns(
            &extract_bin,
            &src,
            &["result", Self::LOOP_ENTRY_TARGET],
            &include,
            timing::NO_NODE,
            timing::NO_ROUND,
        )
        .map_err(|e| DriverError::Session(format!("fused outer compile failed: {e}")))?;
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
        self.emit(Event::LoopBoundary);

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
        let (prompt_before, loop_turn) = match self.compile_cycle_entry(prior_state) {
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
        let mut first = true;
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
        self.checkpoint_generation = checkpoint.generation;
        self.iteration = checkpoint.iteration;
        if checkpoint.harness_source != source.fingerprint {
            tracing::info!(
                restored_fingerprint = %checkpoint.harness_source,
                current_fingerprint = %source.fingerprint,
                "checkpoint harness_source disagrees with the current source fingerprint — \
                 discarding the persisted state and starting fresh from initialState"
            );
            self.emit(Event::HarnessSourceChanged {
                restored_fingerprint: checkpoint.harness_source,
                current_fingerprint: source.fingerprint.clone(),
            });
            self.last_compaction = None;
            // The iteration count is the discarded harness's loop history —
            // same reasoning as the compaction summary above. GENERATION
            // deliberately still carries (adopted before this branch): it is
            // storage lineage for the checkpoint FILE, not loop state, and
            // the next commit must supersede the discarded row, not restart
            // a parallel numbering at 1.
            self.iteration = 0;
            return Ok(None);
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
        let generation = self.checkpoint_generation + 1;
        let checkpoint = persistence::Checkpoint {
            generation,
            state: state.clone(),
            compaction: self.last_compaction.clone(),
            harness_source: source.fingerprint.clone(),
            iteration: self.iteration,
        };
        persistence::save_checkpoint(&self.checkpoint_path, &checkpoint)?;
        self.checkpoint_generation = generation;
        Ok(())
    }

    /// The between-loops human checkpoint: block on [`OperatorGate::await_continue`]
    /// — the human-clicks-continue gate. The default [`StdinGate`] keeps the
    /// original headless behavior (block on a stdin line); a web/GUI gate
    /// parks on a button click instead.
    fn between_loops_gate(&mut self) -> Result<(), DriverError> {
        // `OperatorGate::await_continue` is SYNC-BLOCKING by frozen contract
        // (`selfharness/operator.rs`) — a web gate parks a channel. Run the
        // park under `block_in_place` so that blocking wait yields the tokio
        // worker to other tasks instead of stalling it.
        let gate = Arc::clone(&self.gate);
        tokio::task::block_in_place(move || gate.await_continue());
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
        self.loop_inference_calls = 0;
        self.cycle_compaction = None;

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
        self.iteration_realm = self.iteration_realm.wrapping_add(1);
        self.agent.set_node_realm(
            answerer,
            tidepool_codegen::jit_machine::RealmId(self.iteration_realm),
        );
        self.answerer = Some(answerer);

        let result = self.run_loop_fragment_inner(prior_state, precompiled).await;
        self.retire_answerer();
        result
    }

    /// Retire the current loop's answerer node (terminalize it and drop its
    /// session), so the next loop starts from a fresh render-seeded one.
    /// Idempotent — a no-op if no answerer is live.
    fn retire_answerer(&mut self) {
        if let Some(node) = self.answerer.take() {
            let _ = self.agent.terminate_node(node, "loop answerer retired");
        }
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
                let helpers = state_cross::state_in(prior_state);
                let code = format!("{}.loop __selfHarnessState", state_cross::LOADED_QUALIFIER);
                self.compile_outer(&code, &helpers, "loop")?
            }
        };

        let mut outcome = {
            let sid = self.outer_sid()?;
            self.agent
                .with_session(sid, |s| s.run("loop", &compiled.expr, &compiled.table))
                .map_err(|e| DriverError::Session(e.to_string()))?
                .map_err(|e| map_run_error("loop run failed", e.to_string()))?
        };
        loop {
            match outcome {
                ResidentOutcome::Completed { result, .. } => {
                    return Ok((result.into_value(), compiled.table));
                }
                ResidentOutcome::Suspended { hole, request, .. } => {
                    let classified =
                        engine::classify_hole(&request, &compiled.table, &compiled.asks);
                    match &classified.routing {
                        HoleRouting::RunLLMTurn { site, ty } => {
                            let answer = self
                                .service_runllm_hole(
                                    *site,
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
                            outcome = self
                                .agent
                                .with_session(sid, |s| match answer {
                                    FinalAnswer::Value(v) => s.resume(&hole, v),
                                    // Pillar B: the closure payload is
                                    // DELIVERED by handle — same heap, no
                                    // bridge, no sentinel.
                                    FinalAnswer::Handle(h) => s.resume_handle(&hole, h),
                                })
                                .map_err(|e| DriverError::Session(e.to_string()))?
                                .map_err(|e| {
                                    DriverError::Session(format!("loop resume failed: {e}"))
                                })?;
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
                            outcome = self
                                .service_outer_askuser_hole(
                                    hole.clone(),
                                    classified.routing.clone(),
                                    &compiled,
                                )
                                .await?;
                        }
                        other => {
                            return Err(DriverError::Session(format!(
                                "outer loop suspended on an unserviceable hole ({other:?}) — \
                                 the Harness monad exposes runLLMTurn, askUser, and note only"
                            )))
                        }
                    }
                }
            }
        }
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

        let node = self.answerer.ok_or_else(|| {
            DriverError::Session(
                "service_runllm_hole called with no per-loop answerer (run_loop_fragment \
                 must create it first)"
                    .into(),
            )
        })?;

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
        let child_prompt =
            engine::answerer_hole_card(prompt, ty, self.answerer_imports(), Some(table));
        self.agent.push_user_turn(node, &child_prompt)?;
        self.emit(Event::TurnStart { node });

        let outcome = self.drive_answerer_to_finalize(node, ty, site).await?;
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
    async fn drive_answerer_to_finalize(
        &mut self,
        node: NodeId,
        ty: Option<&str>,
        site: u32,
    ) -> Result<TurnOutcome, DriverError> {
        let ty_label = ty.unwrap_or("A");
        let max_rounds = self.answerer_max_rounds;
        let nudge_rounds = self.answerer_nudge_rounds;
        let mut rounds: u32 = 0;
        let mut nudged = false;
        loop {
            let cap = self.loop_inference_call_cap;
            if self.loop_inference_calls >= cap {
                return Err(DriverError::Session(format!(
                    "per-loop inference-call cap ({cap}) reached — \
                     hard-stopping the loop (a runaway harness)"
                )));
            }
            if rounds >= max_rounds {
                return Err(DriverError::Session(format!(
                    "runLLMTurn answerer exceeded {max_rounds} rounds without \
                     finalizing — hard-failing the hole"
                )));
            }
            if rounds == nudge_rounds && !nudged {
                self.agent.push_user_turn(
                    node,
                    &format!(
                        "You are approaching the maximum number of tool calls for this \
                         request. Finalize now: evaluate `finalize @{ty_label} (value :: \
                         {ty_label})` with your best answer."
                    ),
                )?;
                nudged = true;
            }

            self.loop_inference_calls += 1;
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
                    // default-no-op on headless gates).
                    if let Some(src) = self.agent.last_turn_source(node) {
                        self.gate.post_turn_source(&src);
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
                    // answerer stack (`[AskUser, Fork, Finalize]`) can reach
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
                        return Ok(TurnOutcome::Suspended { hole, classified });
                    }
                    if let HoleRouting::AskUser { shape } = &classified.routing {
                        match self.service_askuser_hole(node, shape).await? {
                            Some(finalize_outcome) => return Ok(finalize_outcome),
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
                            return Ok(out);
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
                    self.agent.reopen_node(node)?;
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "That did not resolve the request. Answer by evaluating \
                             `(finalize @{ty_label} value :: M {ty_label})` — the whole \
                             expression must carry the type annotation, not just the \
                             argument."
                        ),
                    )?;
                }
                // An empty turn (no haskell block): the node is still `Running`
                // (no block ran), so no reopen — just re-prompt.
                Ok(TurnOutcome::NoBlock { .. }) => {
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "Reply with a single ```haskell block that evaluates \
                             `finalize @{ty_label} (value :: {ty_label})`."
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
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "That Haskell did not compile. Fix it and reply with a corrected \
                             single ```haskell block that evaluates `finalize @{ty_label} \
                             (value :: {ty_label})`.\n\nGHC error:\n{msg}{hint}"
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
        &mut self,
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
    async fn service_outer_askuser_hole(
        &mut self,
        hole: String,
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
                        .with_session(sid, |s| s.resume(&hole, answer))
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
                        .with_session(sid, |s| s.resume(&hole, answer))
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
                        engine::classify_hole(request, &compiled.table, &compiled.asks);
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

    /// Post `text` to the operator gate and emit [`Event::NotePosted`] — the
    /// shared, non-blocking half of servicing a `note` hole. `source`
    /// distinguishes a nested answerer's own note from one the AUTHORED
    /// OUTER loop raised directly, same as [`FormSource`] does for a form.
    /// Unlike [`Self::present_askuser_form`], there is nothing to wait for:
    /// the caller resumes immediately after this returns.
    fn announce_note(&self, source: FormSource, text: &str) {
        self.emit(Event::NotePosted {
            source,
            text: text.to_string(),
        });
        let gate = Arc::clone(&self.gate);
        let posted = text.to_string();
        tokio::task::block_in_place(move || gate.post_note(&posted));
    }

    /// Post `text` to the operator gate and resume `node`'s `note` hole
    /// immediately with `()` via [`Harness::answer_note`] — no operator
    /// interaction, no model round. Unlike `askUser`'s reprompt cap, this has
    /// no bound of its own: a `note` resume always makes progress (the next
    /// pending hole, or none at all), so nothing here can spin.
    async fn service_note_hole(&mut self, node: NodeId, text: &str) -> Result<(), DriverError> {
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
        &mut self,
        node: NodeId,
        mut hole: String,
        mut classified: ClassifiedHole,
    ) -> Result<Option<(String, ClassifiedHole)>, DriverError> {
        while let HoleRouting::Note { text } = classified.routing.clone() {
            self.service_note_hole(node, &text).await?;
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
    /// side raised the form in the transcript, not a hidden behavior fork.
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

        self.emit(Event::FormPresented {
            source: source.clone(),
            shape: shape.clone(),
        });
        // `OperatorGate::present_form` is SYNC-BLOCKING by frozen contract
        // (`selfharness/operator.rs`) — a web gate parks a channel. Run it
        // under `block_in_place` so that blocking wait yields the tokio
        // worker rather than stalling it.
        let gate = Arc::clone(&self.gate);
        let form = shape.clone();
        let submission = tokio::task::block_in_place(move || gate.present_form(&form));
        self.emit(Event::FormSubmitted {
            source,
            submission: submission.clone(),
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
        &mut self,
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
        let Some(answerer) = self.answerer else {
            return Ok(());
        };
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
        if self.loop_inference_calls >= cap {
            return Err(DriverError::Session(format!(
                "per-loop inference-call cap ({cap}) reached during \
                 compaction — hard-stopping the loop (a runaway harness)"
            )));
        }
        self.loop_inference_calls += 1;

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
    use super::answerer_decls;

    /// The answerer's generated `Tidepool.Effects` module declares the
    /// `AskUser` GADT + `askUserRaw` helper
    /// — and does NOT declare `Ask`/`ask`/`dialogAsk` (a DIFFERENT effect,
    /// deliberately absent from `answerer_decls()`, and `dialogAsk` is
    /// deleted outright). Pure string-level check, no GHC needed.
    #[test]
    fn answerer_effects_module_declares_askuser_not_ask() {
        let src = tidepool_mcp::effects_module_source(&answerer_decls());
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
        assert!(!outer_section.contains("**Fork**"));
        assert!(answerer_section.contains("**Fork**"));
        assert_ne!(outer_section, answerer_section);
    }
}
