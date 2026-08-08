//! WS-A seam: the runtime driver spine — the outer `render`/`loop`
//! alternation (01/02-runtime.md) over the OUTER Harness-monad resident
//! session, servicing each `runLLMTurn` hole by driving a NESTED
//! [`crate::harness::Harness`] (an ordinary Agent node, reusing
//! `run_to_hole_or_done` — recon: `harness.rs:634-676,975-1016`) to a
//! `finalize` (WS-B) and `run_child`-ing the result back in-heap
//! (`resident.rs:407-465`) to resume `loop`.
//!
//! ANTI-PATTERNS this seam is shaped to avoid (07-impl-orchestration.md
//! WS-A): don't reuse `tidepool-repl`'s parked-thread mechanism (dead end
//! once `fork` lands — the outer session is `Threadless`, same mechanism
//! `Harness`'s own nodes use); don't reimplement the turn loop —
//! [`SelfHarnessDriver::service_runllm_hole`] reshapes
//! `Harness::run_to_hole_or_done`, it does not duplicate it; don't copy
//! heaps — the nested Agent's `finalize` value crosses via
//! [`Harness::take_finalized_value`] (deep-forced out of its own heap, never
//! JSON) and is fed straight into [`ResidentSession::resume`] to resume the
//! OUTER session's parked `runLLMTurn` continuation.
//!
//! # Sync surface, async underneath
//!
//! [`SelfHarnessDriver::run_loop`]/[`SelfHarnessDriver::run_one_cycle`] are
//! synchronous (the frozen S3 contract), but servicing a `runLLMTurn` hole
//! drives the nested [`Harness`]'s `async` turn loop
//! ([`service_runllm_hole`](SelfHarnessDriver::service_runllm_hole)) — so
//! every entry point here must be called from a thread with an ACTIVE tokio
//! runtime (`#[tokio::main]`/`#[tokio::test(flavor = "multi_thread")]`); the
//! bridge is `tokio::task::block_in_place` + `Handle::current().block_on`,
//! which requires the multi-thread runtime flavor.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value as Json;
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::ResidentOutcome;

use crate::compile::{self, CompiledTurn};
use crate::engine::{self, EngineConfig, HoleRouting, TurnOutcome};
use crate::harness::{Harness, HarnessError};
use crate::log::Actor;
use crate::selfharness::harness_source::HarnessSource;
use crate::selfharness::lifecycle::SelfHarnessState;
use crate::selfharness::observer::{Event, Observer};
use crate::selfharness::operator::{FormSpec, OperatorGate, StdinGate};
use crate::selfharness::persistence::{self, PersistenceError};
use crate::selfharness::state_cross;
use crate::tree::NodeId;

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("self-harness driver: {0}")]
    Session(String),
    #[error(transparent)]
    Agent(#[from] HarnessError),
    /// The prior loop's `State` JSON failed the author's `FromJSON State`
    /// instance when re-spliced (J1) — a distinct, actionable failure (the
    /// author's `ToJSON`/`FromJSON State` are not inverse) rather than an
    /// opaque "loop run failed". Detected via
    /// [`state_cross::STATE_DECODE_SENTINEL`]. Carries the Aeson decode error.
    #[error("self-harness State decode failed (ToJSON/FromJSON State not inverse): {0}")]
    StateDecode(String),
    /// W3: `State` json save/restore failed (disk full, permissions, or a
    /// malformed persisted file) — distinct from [`DriverError::StateDecode`],
    /// which is the author's `FromJSON State` instance rejecting otherwise
    /// well-formed JSON.
    #[error("self-harness persistence: {0}")]
    Persistence(#[from] PersistenceError),
}

/// Map an outer-session run error string to a typed [`DriverError`]: a message
/// carrying [`state_cross::STATE_DECODE_SENTINEL`] becomes
/// [`DriverError::StateDecode`] (J1), everything else a generic
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
    /// `render(state, lastCompaction)`'s text BEFORE this cycle's `loop` ran
    /// — the prompt the loop's `runLLMTurn` answerer(s) implicitly worked
    /// under.
    pub prompt_before: String,
    /// `loop`'s returned `State`, serialized ([`state_cross::state_out`]).
    pub state_json: Json,
    /// `render(state, lastCompaction)`'s text AFTER this cycle's `loop`
    /// completed — proves the new `State` reaches the next render.
    pub prompt_after: String,
    /// The runtime-owned emergency compaction turn's `Text` (W2), if this
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
struct OuterSession {
    session: crate::harness::Session,
    cfg: EngineConfig,
    module_name: String,
}

/// The outer Harness-monad's OWN decl list — `Eff '[RunLLMTurn, AskUser]`,
/// distinct from the nested Agent's full stack (`crate::engine`'s private
/// `agent_decls`). `RunLLMTurn` is the loop's model-spawning verb (02-runtime.md
/// LOCKED: no BASE effects for v1); `AskUser` (self-iterating-harness Wave 2) is
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

/// The nested answerer Agent's SCOPED decl list (W1 effect-scoping;
/// 08-wave1-correctness.md: "The Agent's effects = gui + finalize, NOT
/// runLLMTurn — no recursive model-spawning"). Mirrors [`outer_decls`] — a
/// narrow, purpose-built stack rather than the full `EngineConfig::standard`
/// Agent row: it DROPS all NINE base effects (`Console`/`KV`/`Fs`/`Lsp`/
/// `Http`/`Exec`/`Git`/`Time`/`Meta`), so the answerer structurally CANNOT
/// `run` a shell command, read files, or hit the network — its turn compiles
/// against a `Tidepool.Effects` that never defines those verbs. The row is
/// `[AskUser, Finalize]`: `AskUser` (self-iterating-harness Wave 2) is
/// answerer-only — a brand new effect alongside (not a replacement for) the
/// general-Agent-stack `Ask`, deliberately absent from `standard_decls()` —
/// presenting a typed form to a HUMAN OPERATOR (`askUser`/`askUserRaw`) and
/// blocking until submit; `Finalize` (`finalize`) is the answer path.
///
/// # `RunLLMTurn` is STRUCTURALLY excluded — a compile error, not a prompt rule
///
/// "No recursive model-spawning" used to be enforced only BEHAVIORALLY (the
/// narrow answerer FRAMING, [`ANSWERER_FRAMING_SUFFIX`], simply never
/// advertised `runLLMTurn`) because `RunLLMTurn` had to stay in this row for
/// an unrelated reason: the frozen authored-harness contract
/// (`examples/harness/Harness.hs`) used to define its answer types
/// (`Decision`, …) in the SAME module as `loop` (which calls `runLLMTurn`) —
/// so the answerer, which imports that module to construct a typed
/// `finalize @Decision (...)` reply, would only compile if `runLLMTurn`
/// typechecked there too, which needed `RunLLMTurn` in this decl list
/// regardless of whether the verb was monomorphic or `Member`-polymorphic
/// (GHC compiles a module as a whole, not just the names an importer uses).
///
/// The harness contract now splits that dependency: `examples/harness/
/// HarnessTypes.hs` holds `State`/`Mode`/`Decision`/`Confidence`/
/// `initialState`/`render` with NO reference to `Tidepool.Harness`/
/// `runLLMTurn`, and `examples/harness/Harness.hs` (which still defines
/// `loop`) re-exports them. The answerer imports `HarnessTypes` directly, so
/// its compile never pulls in `loop` — `RunLLMTurn` can now be dropped from
/// this row entirely. `RunLLMTurn`'s GADT/verbs are then simply UNDECLARED
/// in the answerer's generated `Tidepool.Effects` (see
/// `tidepool_mcp::effects_module_source`, which only emits an effect's GADT
/// and helpers for decls actually passed in) — a stray `runLLMTurn` call in
/// an answerer turn is a GHC "not in scope" error, the Member-polymorphic
/// verbs (siteid-plugin) making that possible without extract/JIT changes:
/// `runLLMTurn`/`finalize` no longer need to be compiled against one hardcoded
/// closed `M`, so a narrower per-compile decl list is all that's needed to
/// shrink the row. This is the harness/agent structural split (see
/// `Tidepool.Agent` in the stdlib for the answerer's own named boundary,
/// mirroring `Tidepool.Harness`'s `HarnessEff`).
///
/// `AskUser` comes first: [`EngineConfig::from_decls`] takes the FIRST
/// interposed effect (`Ask`|`AskUser`|`RunLLMTurn`|`Finalize`) as the suspend
/// threshold — with `RunLLMTurn` gone from this row, `AskUser` is simply
/// first among the two that remain.
pub fn answerer_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![tidepool_mcp::askuser_decl(), tidepool_mcp::finalize_decl()]
}

fn not_bootstrapped() -> DriverError {
    DriverError::Session("outer session not bootstrapped (call run_loop/run_one_cycle)".into())
}

/// Default emergency-compaction threshold — 80% of the CONTEXT-WINDOW budget
/// ([`EngineConfig::context_window_tokens`], NOT `max_tokens`, which is the
/// 2048 per-turn *output* cap; 02-runtime.md LOCKED: "~80% of a real
/// context-window budget"). Overridable per driver via
/// [`SelfHarnessDriver::set_compaction_threshold_percent`] (e.g. a test
/// driving a low threshold to trip compaction deterministically off a
/// single small turn's usage).
const DEFAULT_COMPACTION_THRESHOLD_PERCENT: u64 = 80;

/// The forced compaction turn's target summary length, expressed as a
/// fraction of the context-window budget (a quarter — big enough to carry
/// real context, small enough to be a genuine compaction rather than a
/// re-summarized transcript).
const COMPACTION_TARGET_DIVISOR: u32 = 4;

/// Per-hole SOFT cap (W1 runaway cap 1): after this many model rounds on a
/// single `runLLMTurn` hole that did NOT finalize, nudge the answerer once
/// ("approaching max tool calls, finalize now with `@T`") and keep driving.
/// 08-wave1-correctness.md LOCKED: "up to 16 tool-call rounds ... at 16 the
/// runtime nudges".
const ANSWERER_NUDGE_ROUNDS: u32 = 16;

/// Per-hole HARD cap (W1 runaway cap 1): after this many non-finalize model
/// rounds on one hole, hard-fail the `runLLMTurn` effect with a
/// [`DriverError`]. 08-wave1-correctness.md LOCKED: "at 32 it hard-fails the
/// runLLMTurn effect".
const ANSWERER_MAX_ROUNDS: u32 = 32;

/// Cap on CONSECUTIVE `askUser` re-presentations within the servicing of ONE
/// hole (self-iterating-harness Wave 2 fold-in, post-review): `askUser`
/// re-prompts by RECURSION on a decode failure — no `Either`, per spec — and
/// the frozen headless `StdinGate::present_form` returns an EMPTY
/// `Submission` on EOF rather than erroring, so a non-interactive gate with
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

/// Per-LOOP hard cap on TOTAL model inference calls across every hole + round
/// (W1 runaway cap 2; 08-wave1-correctness.md LOCKED: "Per-loop total
/// inference-call cap = 1024 — hard-stop the loop"). Keeps a misbehaving
/// harness from running away regardless of per-hole budgets or compaction.
const LOOP_INFERENCE_CALL_CAP: u32 = 1024;

/// The narrow answerer instruction appended after `render`'s output to form
/// the per-loop answerer session's system message (W1/C1). Deliberately
/// scoped to the answerer's INTENDED surface — gui (`askUser`) plus
/// `finalize` — NOT the full eval surface [`crate::engine::SYSTEM_FRAMING`]
/// advertises (`runLLMTurn`/`run`/git/http/…). This is now belt-and-braces
/// rather than the enforcement mechanism: every verb `SYSTEM_FRAMING`
/// advertises but this framing omits genuinely does NOT compile against the
/// answerer's scoped stack ([`answerer_decls`]) — `RunLLMTurn` included, now
/// that it's excluded from the row (see [`answerer_decls`]'s doc). This is
/// the first real slice of the per-context-effect-set goal (§03).
const ANSWERER_FRAMING_SUFFIX: &str = "\
---\n\
You are the answering agent for a self-iterating harness loop. The system \
context above is your working brief (it is re-rendered from the loop's durable \
State each loop). Each request below asks you for ONE typed value.\n\
\n\
Your ONLY runnable output is a single fenced ```haskell block containing one \
expression of type `M a`. To gather operator input across turns, evaluate a \
typed form: `askUser :: Form a -> M a` (`import Tidepool.Form`), built \
applicatively from `enumField`/`intField`/`textField`/`boolField` — it BLOCKS \
for a human operator and returns the decoded typed value directly (a bad \
submission re-prompts internally; there is no `Either` to unwrap). A value you \
bind with `x <- …` persists into your NEXT turn like GHCi, so you can branch \
on it.\n\
\n\
When you have the answer, COMMIT it by evaluating `finalize @T (value :: T)` \
— this ends your turn and hands the typed value back to the loop. `T` is the \
type named in the request. Do not call any other effect to answer; `finalize` \
is how you resolve the request.";

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
    /// The nested multi-node orchestrator that answers a `runLLMTurn` hole
    /// by driving an Agent turn loop (`run_to_hole_or_done`) to a
    /// `finalize`. Shared, not owned exclusively, so a future GUI/inspector
    /// can observe the same node tree.
    agent: Arc<Harness>,
    lifecycle: SelfHarnessState,
    observer: Arc<dyn Observer>,
    /// The LATEST emergency-compaction `Text` (W2), fed as the NEXT
    /// [`Self::run_one_cycle`] call's `lastCompaction` — driver-owned state
    /// rather than a threaded parameter, since 02-runtime.md LOCKS the
    /// *runtime* (not the caller) as the owner of the compaction lifecycle.
    /// Updated MID-LOOP by [`Self::maybe_compact_answerer`] the moment a
    /// compaction fires (the loop then CONTINUES under the summary). `None`
    /// until the first compaction fires.
    last_compaction: Option<String>,
    /// The compaction `Text` produced DURING the current cycle's loop, if one
    /// fired (W2, in-place mid-loop relief) — distinct from
    /// [`Self::last_compaction`], which also carries a PRIOR cycle's summary
    /// forward. Reset (`take`n) into [`CycleOutcome::compaction`] at the end of
    /// [`Self::run_one_cycle`], so a test asserts on THIS cycle's compaction,
    /// not a stale carried-forward one. Set by [`Self::maybe_compact_answerer`].
    cycle_compaction: Option<String>,
    /// The emergency-compaction threshold, as a percentage of the CONTEXT-
    /// WINDOW budget ([`EngineConfig::context_window_tokens`], 02-runtime.md:
    /// "~80%", [`DEFAULT_COMPACTION_THRESHOLD_PERCENT`]). Configurable via
    /// [`Self::set_compaction_threshold_percent`]. Checked MID-LOOP against the
    /// answerer session's real accumulated context ([`Harness::node_usage`]),
    /// not against `max_tokens` after the loop (the old, wrong-budget path;
    /// review C3/D2).
    compaction_threshold_percent: u64,
    /// The CURRENT loop's answerer system framing: `render`'s pre-loop output
    /// followed by [`ANSWERER_FRAMING_SUFFIX`] (W1/C1). Set in
    /// [`Self::run_one_cycle`] right after the pre-loop `render`, read when the
    /// answerer session is created. `None` before the first loop's render.
    answerer_framing: Option<String>,
    /// The CURRENT loop's single render-seeded answerer node (W1/C2): created
    /// ONCE per loop in [`Self::run_loop_fragment`], reused for every
    /// `runLLMTurn` hole so hole #2's answerer sees hole #1's exchange (the
    /// accumulating context window — the fused hylo intermediate). Retired
    /// (dropped) at loop end so the next loop gets a fresh render-seeded
    /// session. `None` between loops.
    answerer: Option<NodeId>,
    /// Total model inference calls across the CURRENT loop's holes + rounds
    /// (W1 runaway cap 2): reset in [`Self::run_loop_fragment`], incremented
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
    /// Per-loop total inference-call cap (W1 runaway cap 2), default
    /// [`LOOP_INFERENCE_CALL_CAP`] (1024). Configurable via
    /// [`Self::set_loop_inference_call_cap`] so a test can prove a specific
    /// model call — e.g. the compaction summarize turn (review C-2) — counts
    /// against it with a small cap instead of scripting 1024 real turns.
    loop_inference_call_cap: u32,
    /// W3: the `State` json persistence path — [`Self::run_loop`] restores
    /// from this file on start (falling back to `initialState` if absent,
    /// same as the very first cycle ever) and saves the returned `State`
    /// here after every cycle, so a killed-and-restarted process resumes
    /// where it left off. Default [`persistence::default_state_path`];
    /// override via [`Self::set_state_path`] (mainly for tests, which point
    /// it at a scratch dir rather than the real cache dir).
    state_path: PathBuf,
    /// The compaction-summary sidecar path (review C-3): the LATEST
    /// `self.last_compaction` is persisted here every time a compaction fires
    /// ([`Self::set_last_compaction`]) and reloaded on start by
    /// [`Self::run_loop`], so a crash-and-restart preserves the summary the
    /// next render depends on rather than rolling `last_compaction` back to
    /// `None`. Default [`persistence::default_compaction_path`]; override via
    /// [`Self::set_compaction_path`] (mainly for tests).
    compaction_path: PathBuf,
    /// The operator-input seam (self-iterating-harness Wave 2): the driver
    /// blocks on this for `askUser` form presentation
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
            state_path: persistence::default_state_path(),
            compaction_path: persistence::default_compaction_path(),
            gate: Arc::new(StdinGate),
        }
    }

    /// The driver's current lifecycle state.
    pub fn lifecycle(&self) -> &SelfHarnessState {
        &self.lifecycle
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
    /// budget per 02-runtime.md). Mainly for tests: a low percentage trips
    /// compaction deterministically off a single small scripted turn's usage
    /// instead of needing a long scripted reply sequence to organically cross
    /// 80% of [`EngineConfig::context_window_tokens`].
    pub fn set_compaction_threshold_percent(&mut self, percent: u64) {
        self.compaction_threshold_percent = percent;
    }

    /// Override the per-hole answerer round caps (default
    /// [`ANSWERER_NUDGE_ROUNDS`]/[`ANSWERER_MAX_ROUNDS`], 16/32 per
    /// 08-wave1-correctness.md). Mainly for tests: small caps (e.g. 3/6) trip
    /// the nudge + hard-fail with a few scripted turns instead of 16/32 real
    /// GHC compiles. `nudge` is clamped below `max`.
    pub fn set_answerer_round_caps(&mut self, nudge: u32, max: u32) {
        self.answerer_nudge_rounds = nudge.min(max);
        self.answerer_max_rounds = max;
    }

    /// Override the per-loop total inference-call cap (default
    /// [`LOOP_INFERENCE_CALL_CAP`], 1024). Mainly for tests: a small cap proves
    /// a specific model call — e.g. the compaction summarize turn (review
    /// C-2) — is counted against it without scripting 1024 real turns.
    pub fn set_loop_inference_call_cap(&mut self, cap: u32) {
        self.loop_inference_call_cap = cap;
    }

    /// Override the `State` json persistence path (W3; default
    /// [`persistence::default_state_path`]). Mainly for tests: point it at a
    /// scratch dir so a test's persisted `State` never touches the real
    /// cache dir, and so a "simulated restart" (a second, fresh driver
    /// pointed at the same path) can restore what the first one wrote.
    pub fn set_state_path(&mut self, path: PathBuf) {
        self.state_path = path;
    }

    /// The current `State` json persistence path (W3).
    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    /// Override the compaction-summary sidecar path (review C-3; default
    /// [`persistence::default_compaction_path`]). Mainly for tests: point it at
    /// a scratch dir so a "simulated restart" (a second driver pointed at the
    /// same path) restores the summary the first one persisted.
    pub fn set_compaction_path(&mut self, path: PathBuf) {
        self.compaction_path = path;
    }

    /// The current compaction-summary sidecar path (review C-3).
    pub fn compaction_path(&self) -> &Path {
        &self.compaction_path
    }

    /// The latest compaction summary the driver holds (`self.last_compaction`)
    /// — what the next render receives as `lastCompaction`. Reflects a
    /// reload from [`Self::compaction_path`] after [`Self::run_loop`] starts,
    /// or the most recent mid-loop compaction. `None` before any has fired.
    pub fn last_compaction(&self) -> Option<&str> {
        self.last_compaction.as_deref()
    }

    /// Bootstrap the outer `PersistentSession<Threadless>` (via
    /// [`crate::harness::Session`]) and splice `source`'s whole module body
    /// ([`HarnessSource`]) as a plain `--include`d module (NOT the session
    /// decl plane — see [`HarnessSource`]'s module doc for why: a static
    /// on-disk harness needs one stable defining module BOTH this compile
    /// and a nested Agent's answerer turn resolve identically, so
    /// author-defined types crossing between them get the same DataConId),
    /// compiled against [`outer_decls`]/[`tidepool_mcp::runllmturn_decl`] —
    /// so `Harness = M` resolves to the literal `Eff '[RunLLMTurn]` row
    /// (02-runtime.md LOCKED). No-op if already bootstrapped.
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

        // A trivial effectful seed carrying the RunLLMTurn-only stack's
        // ConTags (mirrors `Harness::new`'s own boot seed).
        let boot_src = engine::template_turn_for(
            &outer_decls(),
            &outer_cfg,
            "pure (toJSON (0 :: Int))",
            "",
            "",
        );
        let boot = compile::compile_turn(
            &outer_cfg.extract_bin,
            &boot_src,
            "result",
            &outer_cfg.include,
        )
        .map_err(|e| DriverError::Session(format!("outer bootstrap compile: {e}")))?;

        let handler_cfg = tidepool_handlers::HandlerConfig {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            kv_path: tidepool_runtime::paths::cache_dir().join("selfharness-kv.json"),
            llm_model: std::env::var("TIDEPOOL_LLM_MODEL")
                .unwrap_or_else(|_| "gpt-4o-mini".to_string()),
        };
        // Never actually dispatched to: `outer_cfg.suspend_tag == 0` means the
        // ONE declared effect (RunLLMTurn) always suspends before reaching a
        // handler. Reused verbatim from `Harness::build_stack` for the same
        // well-tested concrete stack type.
        let stack: crate::harness::BoxedStack =
            Box::new(tidepool_handlers::build_base_stack(&handler_cfg));

        let session = crate::harness::Session::bootstrap(
            &boot.expr,
            boot.table,
            stack,
            outer_cfg.suspend_tag,
            outer_cfg.effect_names.clone(),
            tidepool_mcp::CapturedOutput::new(),
            outer_cfg.include.clone(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            None,
        )
        .map_err(|e| DriverError::Session(format!("outer bootstrap: {e}")))?;

        self.outer = Some(OuterSession {
            session,
            cfg: outer_cfg,
            module_name: source.module_name.clone(),
        });
        Ok(())
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
    fn compile_outer(&mut self, code: &str, helpers: &str) -> Result<CompiledTurn, DriverError> {
        let outer = self.outer.as_mut().ok_or_else(not_bootstrapped)?;
        let imports = format!(
            "qualified {} as {}",
            outer.module_name,
            state_cross::LOADED_QUALIFIER
        );
        let src = engine::template_turn_for(&outer_decls(), &outer.cfg, code, &imports, helpers);
        compile::compile_turn(&outer.cfg.extract_bin, &src, "result", &outer.cfg.include)
            .map_err(|e| DriverError::Session(format!("outer compile failed: {e}")))
    }

    /// Run ONE `render` → `loop` → (service each `runLLMTurn` hole) →
    /// `render` cycle: bootstrap the outer session if needed, render the
    /// pre-loop prompt, run `loop state` as a suspendable fragment
    /// (servicing every `runLLMTurn` hole via
    /// [`Self::service_runllm_hole`]), serialize the returned `State`
    /// ([`state_cross::state_out`]), and render the post-loop prompt.
    /// `prior_state` is `None` only for the very first cycle (mirrors
    /// `render`'s `Maybe Text` compaction argument being `Nothing`
    /// pre-history). The `lastCompaction` fed to `render` is NOT a
    /// parameter — it is `self.last_compaction`, the latest
    /// emergency-compaction `Text` (W2) if one has fired, carried forward
    /// automatically across repeated calls (by [`Self::run_loop`], or by a
    /// caller driving cycles by hand — see `acceptance_selfharness.rs`),
    /// since 02-runtime.md locks the *runtime*, not the caller, as the
    /// compaction lifecycle's owner. This cycle's OWN compaction (if
    /// [`Self::maybe_compact_answerer`] fires one MID-LOOP) updates
    /// `self.last_compaction` before `prompt_after` is rendered, so
    /// `prompt_after` already reflects it — proving the summary reaches the
    /// very next render.
    pub fn run_one_cycle(
        &mut self,
        source: &HarnessSource,
        prior_state: Option<&Json>,
    ) -> Result<CycleOutcome, DriverError> {
        self.bootstrap(source)?;
        self.emit(Event::LoopBoundary);

        // D4: render the pre-loop prompt directly against `prior_state` — `None`
        // (the very first cycle) splices `Loaded.initialState` in the `render`
        // helpers (`state_cross::state_in(None)`), so no redundant
        // `pure initialState` compile + round-trip through JSON is needed.
        let prior_compaction = self.last_compaction.clone();
        let prompt_before = self.render_framing(prior_state, prior_compaction.as_deref())?;

        // W1/C1: the pre-loop render IS the answerer session's system message.
        // Compose it with the narrow answerer instruction and stash it for
        // `run_loop_fragment` to seed the per-loop answerer node (C2).
        self.answerer_framing = Some(format!("{prompt_before}\n\n{ANSWERER_FRAMING_SUFFIX}"));

        self.lifecycle = SelfHarnessState::RunningLoop;
        // F4 (external-review finding 4): the driver must return to `Idle` on ANY
        // exit from the loop body — an error mid-loop (a runaway-cap hard-fail, a
        // failed resume, a compaction error) must not strand the lifecycle in
        // `RunningLoop`/`Compacting`. Run the fallible body, then reset the
        // lifecycle unconditionally before propagating success or error.
        let result = (|| {
            let (value, table) = self.run_loop_fragment(prior_state)?;
            let state_json = state_cross::state_out(&value, &table);

            // W2: any MID-LOOP compaction that fired during this loop has already
            // set `self.cycle_compaction` (and `self.last_compaction`) IN PLACE —
            // the loop CONTINUED under the summary rather than aborting. `None` if
            // the context window never crossed threshold this loop.
            let compaction = self.cycle_compaction.take();
            // `self.last_compaction` carries the LATEST compaction summary forward
            // to the next render regardless of which cycle produced it: this
            // cycle's if one fired, else the prior cycle's (unchanged). Render
            // `prompt_after` against it so the summary reaches the very next render
            // (02-runtime.md: `render`'s `Maybe Text`).
            let next_compaction = self.last_compaction.clone();
            let prompt_after =
                self.render_framing(Some(&state_json), next_compaction.as_deref())?;

            Ok(CycleOutcome {
                prompt_before,
                state_json,
                prompt_after,
                compaction,
            })
        })();
        self.lifecycle = SelfHarnessState::Idle;
        result
    }

    /// Bootstrap the outer session over `source`, restore the last
    /// persisted `State` from [`Self::state_path`] if a file is there yet
    /// (W3 restart-reload — falls back to `initialState`, exactly the
    /// in-process very-first-cycle case, when no file has been written
    /// yet), then run [`Self::run_one_cycle`] FOREVER, persisting each
    /// cycle's returned `State` back to [`Self::state_path`] and threading
    /// it into the next cycle. Production entry point — see the module doc
    /// for why this (and everything it calls) must run on a thread with an
    /// active multi-thread tokio runtime.
    ///
    /// Between-loops human gate (W1 runaway cap 3): before each new cycle
    /// AFTER the first, unless `auto` is set, print "press Enter to continue"
    /// and block on a line from stdin — a human checkpoint that keeps a
    /// misbehaving harness from running away across loops. `auto` (the
    /// binary's `--yes`/`--auto` flag) skips the gate for CI/replay. The
    /// acceptance path drives [`Self::run_one_cycle`] directly and has NO gate.
    /// Reload persisted durable state from [`Self::state_path`] and
    /// [`Self::compaction_path`] (review C-3), returning the restored `State`
    /// JSON (or `None` for a first-ever run). Restores `self.last_compaction`
    /// as a side effect, so the first render after a restart feeds the same
    /// `lastCompaction` the prior process distilled rather than starting from
    /// `None` (which would silently drop the summary). Called by
    /// [`Self::run_loop`] at start; exposed so a restart-durability test can
    /// drive the same reload path without entering the forever-loop.
    pub fn restore(&mut self) -> Result<Option<Json>, DriverError> {
        let state_json = persistence::load_state(&self.state_path)?;
        if let Some(summary) = persistence::load_compaction(&self.compaction_path)? {
            self.last_compaction = Some(summary);
        }
        Ok(state_json)
    }

    pub fn run_loop(&mut self, source: &HarnessSource, auto: bool) -> Result<(), DriverError> {
        let mut state_json: Option<Json> = self.restore()?;
        let mut first = true;
        loop {
            if !first && !auto {
                self.between_loops_gate()?;
            }
            first = false;
            let outcome = self.run_one_cycle(source, state_json.as_ref())?;
            persistence::save_state(&self.state_path, &outcome.state_json)?;
            state_json = Some(outcome.state_json);
        }
    }

    /// The between-loops human checkpoint: block on [`OperatorGate::await_continue`]
    /// — the human-clicks-continue gate (self-iterating-harness Wave 2) that
    /// replaces the old stdin-Enter read. The default [`StdinGate`] keeps the
    /// original headless behavior (block on a stdin line); a web/GUI gate
    /// parks on a button click instead.
    fn between_loops_gate(&mut self) -> Result<(), DriverError> {
        // The gate is SYNC-blocking (frozen contract); run the park under
        // `block_in_place` so a web gate's channel-wait yields the tokio worker
        // to other tasks instead of stalling it.
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
    /// against — `resume` never recompiles, mirroring
    /// `Harness::resume_parent`'s snapshot-the-table discipline).
    ///
    /// W2: emergency compaction no longer happens here at loop end — it fires
    /// MID-LOOP via [`Self::maybe_compact_answerer`] (checked between the
    /// answerer's holes/rounds against its real accumulated context), setting
    /// `self.cycle_compaction`/`self.last_compaction` in place while the loop
    /// continues under the summary.
    fn run_loop_fragment(
        &mut self,
        prior_state: Option<&Json>,
    ) -> Result<(Value, DataConTable), DriverError> {
        self.loop_inference_calls = 0;
        self.cycle_compaction = None;

        // W1/C2: create the ONE render-seeded answerer session for this whole
        // loop, up front — every `runLLMTurn` hole pushes onto it, so hole #2
        // sees hole #1's exchange (the accumulating context window). Retired
        // in `retire_answerer` once the loop completes (or errors out).
        let answerer =
            self.agent
                .create_root_framed("loop answerer", "", self.answerer_framing.clone())?;
        self.agent.force(answerer, Actor::Operator)?;
        self.answerer = Some(answerer);

        let result = self.run_loop_fragment_inner(prior_state);
        self.retire_answerer();
        result
    }

    /// Drop the current loop's answerer session (W1/C2), so the next loop
    /// starts from a fresh render-seeded one. Idempotent — a no-op if no
    /// answerer is live.
    fn retire_answerer(&mut self) {
        if let Some(node) = self.answerer.take() {
            self.agent.drop_session(node);
        }
    }

    /// The body of [`Self::run_loop_fragment`] — run `loop`, service each
    /// `runLLMTurn` hole against the pre-created per-loop answerer
    /// (`self.answerer`), and return the completed `State` + table. Split out
    /// so [`Self::run_loop_fragment`] can retire the answerer whether this
    /// succeeds or errors.
    ///
    /// W2 (mid-loop, in-place compaction): AFTER each hole is serviced — a
    /// natural boundary between the answerer's holes — [`Self::maybe_compact_answerer`]
    /// checks the answerer's real accumulated context against the threshold and,
    /// if past it, summarizes + replaces the answerer's context IN PLACE so the
    /// loop's REMAINING holes continue under a smaller window (no abort).
    fn run_loop_fragment_inner(
        &mut self,
        prior_state: Option<&Json>,
    ) -> Result<(Value, DataConTable), DriverError> {
        let helpers = state_cross::state_in(prior_state);
        let code = format!("{}.loop __selfHarnessState", state_cross::LOADED_QUALIFIER);
        let compiled = self.compile_outer(&code, &helpers)?;

        let mut outcome = {
            let outer = self.outer.as_mut().ok_or_else(not_bootstrapped)?;
            outer
                .session
                .run("loop", &compiled.expr, &compiled.table)
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
                            let answer =
                                self.service_runllm_hole(*site, ty.as_deref(), &classified.prompt)?;
                            // W2: between holes — if the answerer's accumulated
                            // context has crossed threshold, compact + replace its
                            // context IN PLACE now, so the NEXT hole drives under
                            // the smaller window.
                            //
                            // D-2: this runs only AFTER `service_runllm_hole` has
                            // already finalized THIS hole's answer
                            // (`take_finalized_value_keep_open` consumed the finalize
                            // continuation and returned the session to idle —
                            // harness.rs `take_finalized_value_keep_open`). So the
                            // summarize turn `maybe_compact_answerer` drives sees the
                            // last answer already IN the transcript and cannot drop
                            // it: a future refactor that moves this call BEFORE the
                            // answer is taken would compact a mid-finalize session —
                            // do not.
                            self.maybe_compact_answerer()?;
                            let outer = self.outer.as_mut().ok_or_else(not_bootstrapped)?;
                            outcome = outer.session.resume(&hole, answer).map_err(|e| {
                                DriverError::Session(format!("loop resume failed: {e}"))
                            })?;
                        }
                        // The AUTHORED loop itself evaluated `askUser` (`Tidepool.Form`,
                        // auto-imported because `AskUser` is in `outer_decls`) — a form
                        // presented DIRECTLY by the loop, distinct from an answerer's
                        // form (`service_askuser_hole`). Present it via the same
                        // operator gate and resume the OUTER session; the helper loops
                        // over `askUser`'s Haskell-side decode-retry (a bad submission
                        // re-suspends on a fresh `AskUserWith`) and returns the first
                        // outcome that ISN'T another operator form — a `runLLMTurn`
                        // suspension the main loop then services, or a completion.
                        HoleRouting::AskUser { spec } => {
                            outcome = self.service_outer_askuser_hole(
                                hole.clone(),
                                spec.clone(),
                                &compiled,
                            )?;
                        }
                        other => {
                            return Err(DriverError::Session(format!(
                                "outer loop suspended on an unserviceable hole ({other:?}) — \
                                 the Harness monad exposes runLLMTurn and askUser only"
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
    /// answerer session (`self.answerer`, W1/C2): push the hole card as a User
    /// turn onto that persistent node — so hole #2 sees hole #1's exchange
    /// (the accumulating context window) — then drive it as a bounded
    /// multi-turn interaction to `finalize` (WS-B's effect — terminates the
    /// Agent turn loop rather than resuming it, per 03-agent-surface.md). The
    /// finalized value feeds straight into the OUTER session's `resume` to
    /// answer `loop`'s parked continuation.
    ///
    /// Bounded (W1 runaway caps): each non-finalize model round counts against
    /// a per-hole budget — at [`ANSWERER_NUDGE_ROUNDS`] the answerer is nudged
    /// to finalize, at [`ANSWERER_MAX_ROUNDS`] the hole hard-fails — and
    /// against the per-loop [`LOOP_INFERENCE_CALL_CAP`] total.
    pub fn service_runllm_hole(
        &mut self,
        site: u32,
        ty: Option<&str>,
        prompt: &str,
    ) -> Result<Value, DriverError> {
        self.lifecycle = SelfHarnessState::SuspendedOnHole;
        self.emit(Event::RunLLMTurnHole {
            site,
            ty: ty.map(String::from),
        });

        let node = self.answerer.ok_or_else(|| {
            DriverError::Session(
                "service_runllm_hole called with no per-loop answerer (run_loop_fragment \
                 must create it first)"
                    .into(),
            )
        })?;

        // Push the hole card onto the EXISTING answerer node, accumulating
        // context rather than spawning a fresh one. The SCOPED answerer card
        // (`[AskUser, Finalize]`) names `finalize @T`, NOT the generic
        // `resume expr` (which does not compile against this stack — finding 1).
        let child_prompt = engine::answerer_hole_card(prompt, ty);
        self.agent.push_user_turn(node, &child_prompt)?;
        self.emit(Event::TurnStart { node });

        let outcome = self.drive_answerer_to_finalize(node, ty)?;
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

        // W1/C2: take the finalized value AND keep the node live (consume the
        // finalize hole, Suspended→Running) so the NEXT hole can push onto the
        // same accumulating session — not `take_finalized_value`, which cancels.
        let value = self.agent.take_finalized_value_keep_open(node)?;
        self.emit(Event::Finalize { node });
        // W2: the answerer node is REUSED across the loop's holes (C2), so
        // `node_usage` returns the node's CUMULATIVE context size. The
        // MID-LOOP compaction check (`maybe_compact_answerer`) reads it BETWEEN
        // holes, once per hole, right after this returns — never summed per-hole
        // (that would double-count the reused node's running total).
        self.lifecycle = SelfHarnessState::RunningLoop;
        Ok(value)
    }

    /// Drive `node` (the per-loop answerer, already seeded with this hole's
    /// card) turn-by-turn until it suspends on `finalize`, applying the W1
    /// runaway caps: count each non-finalize model round; at
    /// [`ANSWERER_NUDGE_ROUNDS`] push a one-time "finalize now" nudge; at
    /// [`ANSWERER_MAX_ROUNDS`] hard-fail the hole; and abort the whole loop if
    /// the per-loop [`LOOP_INFERENCE_CALL_CAP`] is hit. A `Completed`
    /// (non-finalize) or `NoBlock` turn is treated as a wasted round —
    /// re-prompted toward `finalize` — rather than accepted, since the
    /// answerer's contract is to resolve the hole via `finalize`, not return a
    /// plain value.
    fn drive_answerer_to_finalize(
        &mut self,
        node: NodeId,
        ty: Option<&str>,
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
            let outcome = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(self.agent.drive_turn(node))
            });
            match outcome {
                Ok(out @ TurnOutcome::Suspended { .. }) => {
                    // A Finalize suspension is the answer. An AskUser suspension
                    // (the scoped answerer's gui path) is SERVICED here — present
                    // the form via the operator gate, resume, and repeat while the
                    // answerer keeps re-suspending on another AskUser (askUser's
                    // Haskell-side decode-failure re-prompt) — see
                    // `service_askuser_hole`. Any OTHER suspension is a hard error:
                    // the scoped answerer stack (`[AskUser, Finalize]`) cannot
                    // reach anything else.
                    let TurnOutcome::Suspended { classified, .. } = &out else {
                        unreachable!("matched TurnOutcome::Suspended above");
                    };
                    if matches!(classified.routing, HoleRouting::Finalize { .. }) {
                        return Ok(out);
                    }
                    if let HoleRouting::AskUser { spec } = &classified.routing {
                        match self.service_askuser_hole(node, spec)? {
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
                                         evaluating `finalize @{ty_label} (value :: \
                                         {ty_label})`."
                                    ),
                                )?;
                                continue;
                            }
                        }
                    }
                    // A non-finalize, non-askUser suspension: the answerer is
                    // mid-interaction and has parked awaiting input this driver
                    // cannot service. Treat it as a hard error rather than
                    // silently hanging.
                    return Err(DriverError::Session(format!(
                        "runLLMTurn answerer suspended on a non-finalize, non-askUser \
                         hole ({:?}) — the self-harness driver has no operator to \
                         answer it",
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
                             `finalize @{ty_label} (value :: {ty_label})`."
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
                Err(HarnessError::Compile(msg)) => {
                    self.agent.push_user_turn(
                        node,
                        &format!(
                            "That Haskell did not compile. Fix it and reply with a corrected \
                             single ```haskell block that evaluates `finalize @{ty_label} \
                             (value :: {ty_label})`.\n\nGHC error:\n{msg}"
                        ),
                    )?;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Service a contiguous run of `askUser` suspensions on `node`, starting
    /// from the just-classified `spec` (self-iterating-harness Wave 2): block
    /// on the operator gate for a submission ([`OperatorGate::present_form`]),
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
    fn service_askuser_hole(
        &mut self,
        node: NodeId,
        spec: &FormSpec,
    ) -> Result<Option<TurnOutcome>, DriverError> {
        let mut spec = spec.clone();
        let mut reprompts: u32 = 0;
        loop {
            if reprompts >= ASKUSER_MAX_REPROMPTS {
                return Err(DriverError::Session(format!(
                    "operator form re-presented {reprompts} times without a decodable \
                     submission (a non-interactive gate at EOF, or a form whose \
                     submission never decodes) — hard-failing the hole"
                )));
            }
            reprompts += 1;

            // The gate is SYNC-blocking (frozen contract): park it under
            // `block_in_place` so a web gate's channel-wait yields the tokio
            // worker rather than stalling it.
            let gate = Arc::clone(&self.gate);
            let form = spec.clone();
            let submission = tokio::task::block_in_place(move || gate.present_form(&form));
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(self.agent.answer_dialog(node, Json::Object(submission)))
            })?;

            let Some((hole, classified, table)) = self.agent.pending_hole_full(node) else {
                // The resume completed the node with no further suspension.
                return Ok(None);
            };
            if matches!(classified.routing, HoleRouting::Finalize { .. }) {
                return Ok(Some(TurnOutcome::Suspended {
                    hole: hole.0,
                    classified,
                    table,
                }));
            }
            if let HoleRouting::AskUser { spec: next_spec } = classified.routing {
                spec = next_spec;
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
    /// raised (self-iterating-harness Wave 2, outer-form path — distinct from
    /// [`Self::service_askuser_hole`], which handles a nested ANSWERER's form).
    /// Present `spec` via the operator gate ([`OperatorGate::present_form`]),
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
    fn service_outer_askuser_hole(
        &mut self,
        hole: String,
        spec: FormSpec,
        compiled: &CompiledTurn,
    ) -> Result<ResidentOutcome, DriverError> {
        let mut hole = hole;
        let mut spec = spec;
        let mut reprompts: u32 = 0;
        loop {
            if reprompts >= ASKUSER_MAX_REPROMPTS {
                return Err(DriverError::Session(format!(
                    "outer-loop operator form re-presented {reprompts} times without a \
                     decodable submission (a non-interactive gate at EOF, or a form \
                     whose submission never decodes) — hard-failing the loop"
                )));
            }
            reprompts += 1;

            // Sync-blocking gate under `block_in_place` (see the frozen contract):
            // a web gate parks a channel here; yield the worker while it waits.
            let gate = Arc::clone(&self.gate);
            let form = spec.clone();
            let submission = tokio::task::block_in_place(move || gate.present_form(&form));
            let answer = engine::json_answer_to_value(&Json::Object(submission), &compiled.table)
                .map_err(|e| {
                DriverError::Session(format!("outer askUser submission decode: {e}"))
            })?;
            let outcome = {
                let outer = self.outer.as_mut().ok_or_else(not_bootstrapped)?;
                outer.session.resume(&hole, answer).map_err(|e| {
                    DriverError::Session(format!("outer askUser resume failed: {e}"))
                })?
            };

            match &outcome {
                ResidentOutcome::Suspended {
                    hole: next_hole,
                    request,
                    ..
                } => {
                    let classified =
                        engine::classify_hole(request, &compiled.table, &compiled.asks);
                    if let HoleRouting::AskUser { spec: next_spec } = classified.routing {
                        // askUser's Haskell-side decode-retry re-suspended on a
                        // fresh form: re-present it (does NOT count as progress).
                        hole = next_hole.clone();
                        spec = next_spec;
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

    /// Evaluate `render(state, lastCompaction)` against the outer session
    /// and return its `Text` result — the next loop's system prompt.
    /// Runtime-invoked at loop boundaries ONLY (02-runtime.md LOCKED).
    /// `state_json` is `None` only for the very first cycle — then the render
    /// splice references `Loaded.initialState` directly (no JSON to decode),
    /// per [`state_cross::state_in`] (D4).
    pub fn render_framing(
        &mut self,
        state_json: Option<&Json>,
        last_compaction: Option<&str>,
    ) -> Result<String, DriverError> {
        let state_decl = state_cross::state_in(state_json);
        let compaction_decl = match last_compaction {
            None => "__selfHarnessCompaction :: Maybe Text\n__selfHarnessCompaction = Nothing"
                .to_string(),
            Some(s) => format!(
                "__selfHarnessCompaction :: Maybe Text\n__selfHarnessCompaction = Just {}",
                state_cross::haskell_string_literal(s)
            ),
        };
        let helpers = format!("{state_decl}\n{compaction_decl}\n");
        let code = format!(
            "pure ({q}.render __selfHarnessState __selfHarnessCompaction)",
            q = state_cross::LOADED_QUALIFIER
        );
        let compiled = self.compile_outer(&code, &helpers)?;
        let outer = self.outer.as_mut().ok_or_else(not_bootstrapped)?;
        let outcome = outer
            .session
            .run("render", &compiled.expr, &compiled.table)
            .map_err(|e| map_run_error("render run failed", e.to_string()))?;
        match outcome {
            ResidentOutcome::Completed { result, .. } => match result.to_json() {
                Json::String(s) => Ok(s),
                other => Err(DriverError::Session(format!(
                    "render did not yield Text, got {other:?}"
                ))),
            },
            ResidentOutcome::Suspended { .. } => Err(DriverError::Session(
                "render suspended unexpectedly — render must be a pure function".into(),
            )),
        }
    }

    /// Runtime-owned MID-LOOP emergency compaction with IN-PLACE relief (W2 /
    /// 02-runtime.md LOCKED: the *runtime* owns this trigger, never the loop;
    /// "replace its context with the summary so the loop CONTINUES", NO
    /// loop-abort). Called between the answerer's holes ([`Self::run_loop_fragment_inner`]).
    ///
    /// Watches the CURRENT loop's answerer session's REAL context size —
    /// [`Harness::node_last_input_tokens`], the LAST turn's `input_tokens`
    /// high-water mark (review C-1: NOT [`Harness::node_usage`]'s summed
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
    ///    per-loop [`LOOP_INFERENCE_CALL_CAP`] (review C-2).
    /// 2. Replaces the answerer's context with `[system + summary]` IN PLACE
    ///    ([`Harness::replace_transcript_with_summary`]) — the loop's remaining
    ///    holes continue under the smaller window.
    /// 3. Records the summary as `self.cycle_compaction` (this cycle's, for
    ///    [`CycleOutcome::compaction`]) and `self.last_compaction` (carried to
    ///    the NEXT [`Self::render_framing`]'s `Maybe Text`, and persisted for
    ///    restart durability — review C-3).
    /// 4. Emits [`Event::CompactionTrigger`] with its payload (summary, pre/post
    ///    context size, node — review C-4).
    fn maybe_compact_answerer(&mut self) -> Result<(), DriverError> {
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

        // C-2: the summarize turn is a real model call — count it against the
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

        // J-2: the target is a fraction of the budget, but clamp it against the
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
        let (summary, post_usage) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.agent.summarize_turn(answerer, &prompt))
        })?;
        let summary = summary.trim().to_string();

        // In-place relief: the answerer's context becomes `[system + summary]`,
        // so its remaining holes drive under the smaller window (loop CONTINUES).
        self.agent
            .replace_transcript_with_summary(answerer, &summary)?;

        // C-4: the trigger event now carries what compaction produced — the
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

    /// Record `summary` as the latest compaction (`self.last_compaction`, fed to
    /// the next render's `Maybe Text`) AND persist it to
    /// [`Self::compaction_path`] so a crash-and-restart preserves the summary
    /// (review C-3): the in-memory field alone rolls back to `None` on restart,
    /// losing the summary the next render depends on. [`Self::run_loop`] reloads
    /// it on start alongside the persisted `State`.
    fn set_last_compaction(&mut self, summary: String) -> Result<(), DriverError> {
        persistence::save_compaction(&self.compaction_path, &summary)?;
        self.last_compaction = Some(summary);
        Ok(())
    }

    /// Emit `event` to the configured [`Observer`] — the ONE place the
    /// driver touches the observer, so no call site hardwires logging or a
    /// future GUI push directly (WS-H anti-pattern guard).
    fn emit(&self, event: Event) {
        self.observer.on_event(&event);
    }
}

#[cfg(test)]
mod tests {
    use super::answerer_decls;

    /// The answerer's generated `Tidepool.Effects` module (self-iterating-
    /// harness Wave 2) declares the NEW `AskUser` GADT + `askUserRaw` helper
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
}
