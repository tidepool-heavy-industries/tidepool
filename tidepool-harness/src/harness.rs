//! `Harness` — the orchestrator that owns the node tree, the resident
//! sessions, the transcript store, and the provider-driven turn loop. This is
//! the object the web protocol server drives.
//!
//! # Ownership map
//!
//! - [`NodeTree`] (`crate::forcing`) owns the tree structure, per-node
//!   [`NodeState`], and the durable event log. Every state transition and
//!   every event goes through it.
//! - The Harness owns the resident sessions directly (one
//!   [`ResidentSession`] per forced node), keyed by [`NodeId`], behind a
//!   mutex. A node's session is created at force time and dropped when the
//!   node terminates.
//! - The transcript store is reconstructed by folding `TurnDelta`/`TurnForked`
//!   events; the live in-memory copy per node lives here too.
//!
//! # Scheduler (thin, TARGET §4)
//!
//! One parent + one live child. A fork PARKS the parent (it is suspended);
//! the child answerer runs its own turn loop, and its final answer eval runs
//! via [`ResidentSession::run_child`] against the parent's suspended machine
//! (same heap, GC-rooted), then resumes the parent. Child evals run only while
//! the parent is parked — which it is, by construction (a fork suspends). No
//! preemption, no work-stealing: inference-bound fan sizes make starvation a
//! non-issue for R0.
//!
//! # Sync core, async driver
//!
//! The resident-session calls (`run`/`run_child`/`resume`) are synchronous and
//! block (they spawn their own eval thread internally). The provider calls are
//! async. The turn-driving methods here are `async`; they `spawn_blocking` the
//! resident-session steps so the tokio reactor is never blocked.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value as Json;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_mcp::CapturedOutput;
use tidepool_repr::{DataConTable, Generation, SessionId};
use tidepool_runtime::session::{
    run_turn, BoundBinder, CompiledTurn, ModuleEnv, ResidentError, ResidentOutcome,
    ResidentSession, SessionLib, TemplateSelector, TurnRequest, TurnResult, TurnTemplate,
    DECL_TEMPLATE_SOURCE,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tokio::sync::{mpsc, oneshot};

use crate::compile::{self, AsksSidecar};
use crate::effect_trace::{EffectRecord, EffectTrace, TracingDispatcher};
use crate::engine::{
    self, ClassifiedHole, EngineConfig, EngineError, HoleRouting, TurnOutcome, RESUME_HELPER,
};
use crate::forcing::{ForkShape, NodeTree, TreeError};
use crate::log::{Actor, AnswerOutcome, LogWriter};
use crate::provider::{DynModelProvider, Message, Role, StreamDelta, Usage};
use crate::registry::{Checkout, CheckoutError};
use crate::timing;
use crate::tree::{FanBadge, HoleId, NodeId};

/// The boxed handler stack — one concrete machine type so the Harness (and the
/// web server over it) is not generic. `build_base_stack` returns an opaque
/// `impl DispatchEffect<CapturedOutput>`; boxing it here erases that so the
/// resident-session type is nameable.
pub type BoxedStack = Box<dyn DispatchEffect<CapturedOutput> + Send>;

/// The concrete resident session type the Harness stores per forced node.
pub type Session = ResidentSession<BoxedStack, CapturedOutput>;

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error(transparent)]
    Tree(#[from] TreeError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error("compile failed:\n{0}")]
    Compile(String),
    #[error("resident session error: {0}")]
    Resident(String),
    #[error("node {0:?} has no live session (not forced, or already terminal)")]
    NoSession(NodeId),
    #[error("node {0:?} is not suspended on a hole")]
    NotSuspended(NodeId),
    #[error("node {node:?}: answer routed to {routing} but hole is {actual}")]
    RoutingMismatch {
        node: NodeId,
        routing: &'static str,
        actual: String,
    },
    #[error("node {node:?} aborted: {reason}")]
    Aborted { node: NodeId, reason: String },
    #[error("node {0:?} has no pending operator escalation to resolve")]
    NoPendingEscalation(NodeId),
    #[error("node {0:?} already has a turn in flight")]
    TurnInFlight(NodeId),
    /// A registry checkout landed on a state mismatch that is neither "no
    /// session" nor "busy" — a resume/child checkout aimed at the wrong hole,
    /// or a new top-level turn attempted on an already-suspended session (see
    /// [`CheckoutError::Suspended`]/[`CheckoutError::NotSuspended`]/
    /// [`CheckoutError::WrongHole`]). Kept distinct from both so a caller
    /// never mistakes a hole/state mismatch for "never forced" or "busy,
    /// retry".
    #[error("node {node:?}: {detail}")]
    SessionMismatch { node: NodeId, detail: String },
}

impl HarnessError {
    /// The ONE place a registry [`CheckoutError`] becomes a node-scoped
    /// [`HarnessError`] — every checkout call site routes through this
    /// rather than choosing its own collapse. `CheckoutError` only knows the
    /// `SessionId`, not the `NodeId`, so the node comes from the call site
    /// (which always has it in hand at the point it checks a session out).
    fn from_checkout(node: NodeId, err: CheckoutError) -> Self {
        match err {
            CheckoutError::Unknown(_) => HarnessError::NoSession(node),
            CheckoutError::Running(_) | CheckoutError::RunningChild { .. } => {
                HarnessError::TurnInFlight(node)
            }
            other @ (CheckoutError::Suspended { .. }
            | CheckoutError::NotSuspended(_)
            | CheckoutError::WrongHole { .. }) => HarnessError::SessionMismatch {
                node,
                detail: other.to_string(),
            },
        }
    }
}

/// What a node must produce to resolve the hole it is answering, and what its
/// turns need in scope to produce it.
///
/// Both halves are required for the "GHC validates the answer against `T`"
/// guarantee to hold for `finalize`. `ty` pins `finalize` to the hole's answer
/// type — [`Harness::run_block`] resolves it into a `Finalize T` ROW entry via
/// [`crate::engine::EngineConfig::turn_target`] (`Member (Finalize T) effs` is
/// the whole pin), so a wrong-typed answer is a compile error naming the row
/// instead of a value that crosses in-heap into a `T`-typed continuation.
/// `imports` puts `T` itself in scope: `T` is an author type (the wizard's
/// `Contribution`, defined in the harness source module), and a turn that cannot
/// NAME `T` cannot construct one — the model tries `@T`, gets "not in scope",
/// and settles for whatever does compile. Importing the module that defines `T`
/// also fixes WHICH `T` the turn means: the same defining module the parent
/// resolved, hence the same `DataConId` at the crossing.
#[derive(Debug, Clone)]
pub struct AnswerContract {
    /// The hole's rendered answer type, as it appears in `asks.json`.
    pub ty: String,
    /// Import lines (without the leading `import`) prepended to every turn on
    /// this node — the module(s) defining `ty`.
    pub imports: Vec<String>,
}

/// Per-node conversation state the Harness keeps live (also derivable from the
/// log by folding). `transcript` is the message list the turn engine assembles
/// prompts from; `turn_seq` is the monotonic per-node turn index logged with
/// each `TurnDelta`. `pending` is the classified hole when suspended.
struct NodeConvo {
    transcript: Vec<Message>,
    turn_seq: u64,
    /// Shared buffer the session's [`TracingDispatcher`] appends each effect to;
    /// drained per turn by [`Harness::flush_effects`] into `Event::Effect`.
    effect_trace: EffectTrace,
    /// Monotonic per-node effect sequence number for the logged `Event::Effect`s.
    effect_seq: u64,
    pending: Option<PendingHole>,
    /// The typed hole this node is currently answering, when it answers by
    /// `finalize` (the self-iterating harness's answerer). Set per hole by
    /// [`Harness::set_answer_contract`]; read by [`Harness::run_block`] to pin
    /// `finalize` to the hole's type and to put that type in scope. `None` for
    /// every node that isn't driving toward a `finalize`.
    answer_contract: Option<AnswerContract>,
    /// Compile artifacts of the turn that suspended — the table is needed to
    /// bridge an answer Value against the same constructor set.
    suspend_table: Option<DataConTable>,
    suspend_asks: AsksSidecar,
    /// When the SUSPENDED turn is a value-plane bind (`x <- fork …`), the binder
    /// metadata + generation to materialize once the bind completes on resume.
    /// `resume_parent` drives `resume_bind` (not `resume`) while this is `Some`,
    /// and clears it when the bind finally lands (a completion, not a re-suspend).
    pending_bind: Option<(BoundBinder, Generation)>,
    /// Running sum of every assistant turn's [`Usage`] on this node — the
    /// self-iterating-harness driver's emergency-compaction trigger,
    /// [`Harness::node_usage`], sums this across every `runLLMTurn`
    /// answerer / compaction node it drives per loop.
    usage: Usage,
    /// The MOST RECENT turn's `input_tokens` (overwritten every turn, not
    /// summed) — the provider's per-round input token count already includes
    /// the whole re-sent transcript, so the latest value IS the node's real
    /// current context size (a high-water mark). The self-iterating-harness
    /// driver's compaction threshold reads THIS rather than the
    /// running [`Self::usage`] sum, which super-linearly over-counts across a
    /// multi-round hole (each round's input re-counts every prior round's
    /// transcript). `0` before the node's first turn.
    last_input_tokens: u64,
    /// This node's OWN system message, overriding the default
    /// [`engine::SYSTEM_FRAMING`] when set — the self-iterating harness's
    /// per-loop answerer session's framing is `render`'s output, wired to
    /// the model here rather than left observational. `None` for an
    /// ordinary Agent node (the default full-surface framing).
    framing: Option<String>,
    /// Set while a turn-owning operation (`drive_turn`/`summarize_turn`/an
    /// `answer_*` method) holds this node's [`TurnLease`] — guards the
    /// snapshot → provider await → log append → resident run → outcome
    /// publish span against a second concurrent turn on the SAME node.
    /// Cleared by `TurnLease::drop`, so every exit path (success, `?`, panic
    /// unwind) releases it.
    turn_lease: bool,
}

/// An RAII hold on [`NodeConvo::turn_lease`], returned by
/// [`Harness::acquire_turn_lease`]. `Drop` clears the flag under the
/// `convos` lock, so success, an early `?` return, and a panic unwind all
/// release it — a node is never left permanently unleasable by a failed turn.
struct TurnLease<'a> {
    harness: &'a Harness,
    node: NodeId,
}

impl Drop for TurnLease<'_> {
    fn drop(&mut self) {
        if let Some(convo) = self.harness.convos.lock().get_mut(&self.node) {
            convo.turn_lease = false;
        }
    }
}

#[derive(Clone)]
struct PendingHole {
    hole: HoleId,
    classified: ClassifiedHole,
    /// The raw suspended request `Value`, kept alongside `classified` (which
    /// is JSON-shaped, lossy for a `Finalize` hole — its carried value may be
    /// non-serializable, e.g. a closure). `Harness::take_finalized_value`
    /// reads the finalize payload straight out of this, never through JSON.
    raw_request: Value,
}

/// A node's live heap/GC snapshot (observatory heap pane) — plain numbers off
/// its resident `JitEffectMachine`, straight from
/// [`tidepool_codegen::jit_machine::HeapStats`]: no new GC/rooting
/// instrumentation, this is a read-only view of counters that already exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapSummary {
    pub nursery_bytes: usize,
    pub live_bytes: usize,
    pub gc_count: u64,
}

/// The escalation-ladder's rung-2 state (operator-in-the-loop): a child
/// answerer exhausted its auto-retry and is parked awaiting an operator
/// decision. IN-PROCESS ONLY — this lives in [`Harness`]'s memory, not the
/// durable event log; a process restart mid-escalation loses it (the
/// operator re-triggers by re-forcing, same as any other in-flight turn —
/// durable mid-fan suspension is explicitly out of R0 scope).
#[derive(Debug, Clone)]
pub struct Escalation {
    /// Human-facing summary of why this node escalated (e.g. cap-exhausted
    /// after N attempts).
    pub reason: String,
    /// A short tail of the answerer's own transcript, for the popup's
    /// "what has it been trying" preview. Model-authored — render it through
    /// the same HTML-neutralizing path any other model text uses.
    pub transcript_preview: String,
}

/// The operator's rung-2 decision, delivered through the oneshot channel
/// [`Harness::resolve_escalation`] fires. `AllocateMore` grants a fresh turn
/// budget (replacing, not adding to, what remained) and optionally injects
/// `steer` as the answerer's next corrective user turn before it retries.
#[derive(Debug, Clone)]
pub enum OperatorDecision {
    AllocateMore { turns: u32, steer: Option<String> },
    Abort,
}

/// The outcome of [`Harness::handle_cap_exhaustion`]'s ladder step: either
/// the caller's loop keeps going with a (possibly larger) turn budget, or the
/// answerer is being torn down.
enum CapDecision {
    Retry { turn_budget: u32 },
    Abort { reason: String },
}

/// A just-created node's staged opening context, held between node creation
/// (`create_root_framed`/`register_fork_child`) and `force` (a thunk node has
/// no live [`NodeConvo`] to hold it yet) — the two node-creation paths differ
/// only in WHAT seeds the transcript, so they share one staging slot per node
/// rather than two maps a reader has to know are mutually exclusive by
/// construction.
enum NodeSeed {
    /// A plain root's opening prompt, from `create_root_framed`.
    Root {
        prompt: String,
        framing: Option<String>,
    },
    /// A fork/fanout child's inherited context, from `register_fork_child`:
    /// the cloned parent transcript prefix (through the fork checkpoint) plus
    /// the hole card, and the parent's framing (its system message), so the
    /// child's request prefix is byte-identical to the parent's through the
    /// checkpoint.
    Forked {
        transcript: Vec<Message>,
        framing: Option<String>,
    },
}

/// A fork child's compile row: the parent row minus the fork-spawning effects
/// (`Fork`/`RunLLMTurn`). A child keeps everything else it needs to compute its
/// answer (base effects, `AskUser`, `Finalize`) but literally cannot name
/// `fork`/`forkAll`/`runLLMTurn` — depth-one is structural, not a runtime
/// guard. For the answerer (`[AskUser, Fork, Finalize]`) this yields the leaf
/// `[AskUser, Finalize]`.
fn fork_child_decls(parent: &[tidepool_mcp::EffectDecl]) -> Vec<tidepool_mcp::EffectDecl> {
    parent
        .iter()
        .filter(|d| !matches!(d.type_name, "Fork" | "RunLLMTurn"))
        .copied()
        .collect()
}

/// Cap a (possibly huge) GHC/extract compile error before feeding it back to
/// the model as a corrective turn — the head carries the structured diagnostics
/// and first errors, which is what the model needs to fix its Haskell. UTF-8
/// safe (truncates on a char boundary).
fn truncate_ghc_error(msg: &str) -> String {
    tracing::warn!("compile error (fed back to model as corrective turn):\n{msg}");
    const CAP: usize = 3000;
    if msg.chars().count() <= CAP {
        msg.to_string()
    } else {
        let head: String = msg.chars().take(CAP).collect();
        format!("{head}\n… (truncated)")
    }
}

/// The `Expr.hs` anchor + display label [`render_compile_error`] renders a
/// remapped `run_turn` diagnostic under — mirrors `tidepool-mcp`'s eval-path
/// `anchor: "Expr.hs"` (the module every `run_turn` template declares, via
/// `tidepool_mcp::build_preamble`), with a harness-specific display label.
const TURN_ANCHOR: &str = "Expr.hs";
const TURN_LABEL: &str = "<turn>";

/// The EXPR template's user-code marker — byte-identical to
/// `tidepool-mcp/src/eval_prep.rs`'s `format_error_with_source::MARKER`,
/// since `engine::expr_turn_template` is built through the SAME
/// `tidepool_mcp::template_haskell`/`template_haskell_anchored` that eval
/// uses (`engine::template_turn_for`).
const EXPR_MARKER: &str = "__user = let {\n __b =\n";

/// The BIND/BINDDISCARD templates' (`engine::session_bind_template`)
/// user-code marker: the turn statement is spliced immediately after
/// `__result = do {`, with no `[user-lines]` annotation of its own (that
/// marker is `template_haskell`-specific — `session_bind_template` never
/// calls it) — found empirically by reading the builder, per this item's
/// spec.
const BIND_MARKER: &str = "__result = do {\n";

/// Compute a candidate template's own user-code line window: `(line_offset,
/// (start, end))`, `line_offset` = newline count up to and including
/// `marker`'s end, `(start, end)` = the 1-based inclusive range `content`
/// (the turn text `run_block` embedded — same `block` for every candidate)
/// occupies immediately after it. `None` when `marker` isn't in `source` (a
/// candidate that was never built, or a builder that changed shape).
fn candidate_window(
    source: &str,
    marker: &str,
    content_lines: usize,
) -> Option<(usize, (usize, usize))> {
    let pos = source.find(marker)?;
    let offset = source[..pos + marker.len()].matches('\n').count();
    Some((offset, (offset + 1, offset + content_lines)))
}

/// Render a turn-compile failure as text a MODEL can act on, with GHC's
/// coordinates remapped from TEMPLATE space to the model's own turn text
/// (`block`) — see `plans/post-restart/dev/error-coordinates.md`.
///
/// `CompileError::Diagnostics`' own `Display` reports only how many
/// diagnostics there were, not what they said — fine for a log line, useless
/// as the corrective user turn [`Harness::run_to_hole_or_done`] feeds back,
/// which is the whole mechanism by which a model fixes its own Haskell.
/// `9c2b14ff` restored the content (severity/span/message per entry); this
/// restores the COORDINATES, reusing `tidepool_runtime::diag`'s remapper (the
/// same one `tidepool-mcp`'s eval path uses) rather than a second one.
///
/// `run_turn` builds up to TWO full module sources before it knows which
/// verdict GHC will pick (`expr_source` always; `bind_source` — representing
/// both `Bind`/`BindDiscard`, which share its exact preamble/offset — only
/// when the node has a value-plane bind context worth trying), and a compile
/// FAILURE carries no verdict tag: `run_turn` returns before ever decoding
/// which template applied. So a diagnostic is remapped against a candidate
/// ONLY when its raw line falls inside THAT candidate's own (deterministically
/// computed, from `block`) user-code window — tried EXPR first, then BIND.
/// A diagnostic outside every candidate's window (or when there is no
/// candidate at all) keeps its raw template-space span, exactly as before
/// this fix: picking the wrong candidate would silently shift every line
/// number by a wrong constant, which is worse than not remapping.
fn render_compile_error(
    e: &tidepool_runtime::CompileError,
    block: &str,
    expr_source: &str,
    bind_source: &str,
) -> String {
    let tidepool_runtime::CompileError::Diagnostics(diags) = e else {
        return e.to_string();
    };
    if let Some(opts) = pick_render_opts(diags, block, expr_source, bind_source) {
        let mut out = format!("GHC error ({} diagnostic(s)):\n", diags.len());
        out.push_str(&tidepool_runtime::diag::render_diagnostics(diags, &opts));
        return out;
    }
    let mut out = format!("GHC error ({} diagnostic(s)):", diags.len());
    for d in diags {
        out.push('\n');
        match &d.span {
            Some(s) => out.push_str(&format!(
                "{}:{}:{}: {}: {}",
                s.file, s.start_line, s.start_col, d.severity, d.message
            )),
            None => out.push_str(&format!("{}: {}", d.severity, d.message)),
        }
    }
    out
}

/// Pick which candidate template's [`tidepool_runtime::diag::RenderOpts`]
/// (if any) a batch of diagnostics should be remapped against — see
/// [`render_compile_error`]'s doc for why this exists and why the choice is
/// derived, never guessed. All diagnostics in one `CompileError::Diagnostics`
/// batch come from the SAME compile, so one representative (anchor-file)
/// diagnostic's raw line decides for the whole batch.
fn pick_render_opts<'a>(
    diags: &[tidepool_runtime::diag::ExtractDiag],
    block: &str,
    expr_source: &'a str,
    bind_source: &'a str,
) -> Option<tidepool_runtime::diag::RenderOpts<'a>> {
    let representative_line = diags.iter().find_map(|d| {
        let span = d.span.as_ref()?;
        span.file
            .ends_with(TURN_ANCHOR)
            .then_some(span.start_line as usize)
    })?;
    let content_lines = engine::content_line_count(block);
    for (source, marker) in [(expr_source, EXPR_MARKER), (bind_source, BIND_MARKER)] {
        let Some((offset, (start, end))) = candidate_window(source, marker, content_lines) else {
            continue;
        };
        if representative_line >= start && representative_line <= end {
            return Some(tidepool_runtime::diag::RenderOpts {
                anchor: TURN_ANCHOR,
                label: TURN_LABEL,
                user_lines: Some((start, end)),
                line_offset: offset,
                col_indent: 0,
                drop_foreign_gen_warnings_except: None,
                source,
            });
        }
    }
    None
}

/// The orchestrator. Cloneable-cheap? No — it owns the tree + sessions, so it
/// is shared behind an `Arc`.
pub struct Harness {
    tree: NodeTree<Session>,
    cfg: EngineConfig,
    /// Unique per-construction run identity, scoping this
    /// instance's node decl-plane directories to
    /// `harness-sessions/<run_id>/node-<id>` so a second, concurrent Harness
    /// sharing the same cache root (a different process, or a second
    /// Harness in this one) can never construct the same node dir and
    /// `remove_dir_all` the other's live declarations out from under it.
    /// See [`generate_run_id`] and [`node_session_dir`].
    run_id: String,
    /// The row a FORK CHILD's answer block compiles against: the node's own
    /// row minus the fork-spawning effects (`Fork`/`RunLLMTurn`), so a child
    /// structurally cannot fork — a `forkAll` in a child block is a GHC
    /// "not in scope" error, not a runtime `ChildSuspended`. For the answerer
    /// (`[AskUser, Fork, Finalize]`) this is the leaf `[AskUser, Finalize]`.
    /// The child's answer still runs via `run_child` against the PARENT's
    /// session (a pure `resume expr` value crossing), so the leaf row only
    /// scopes what the child can NAME, not where its value lands.
    child_cfg: EngineConfig,
    provider: Arc<dyn DynModelProvider>,
    convos: Mutex<HashMap<NodeId, NodeConvo>>,
    /// A just-created node's staged [`NodeSeed`] — a root's opening prompt or
    /// a fork child's inherited transcript, either way paired with its
    /// framing — between node creation and `force` (a thunk node has no live
    /// `NodeConvo` to hold it yet). Removed once consumed at force time.
    pending: Mutex<HashMap<NodeId, NodeSeed>>,
    /// Rung-2 escalation state (operator popup), keyed by the answerer node
    /// that is parked awaiting a decision: the [`Escalation`] the web layer
    /// renders the stuck-node popup from, paired with the oneshot sender half
    /// that delivers the operator's decision back to
    /// [`Self::escalate_to_operator`]'s awaiting receiver (which lives on its
    /// own async stack, in-process only — see that method's doc for the
    /// durability caveat). Set by [`Self::escalate_to_operator`] just before
    /// the await; [`Self::resolve_escalation`] (driven by the web resolve
    /// endpoint, or fired directly in a test) removes the whole entry to take
    /// the sender and fires it — the pair is inserted together and removed
    /// together, never independently.
    escalations: Mutex<HashMap<NodeId, (Escalation, oneshot::Sender<OperatorDecision>)>>,
}

impl Harness {
    /// Build a harness over `writer` (a fresh log past its header), the engine
    /// config, and a signed-in provider.
    pub fn new(
        writer: LogWriter,
        cfg: EngineConfig,
        provider: Arc<dyn DynModelProvider>,
    ) -> Result<Self, HarnessError> {
        let run_id = generate_run_id();
        // Best-effort: sweep run dirs left behind by processes that are
        // provably dead (see `sweep_stale_run_dirs`). Never blocks
        // construction — a failed/skipped sweep just leaves stale dirs on
        // disk a little longer.
        sweep_stale_run_dirs();
        // The fork-child compile config: this node's row minus the
        // fork-spawning effects, so a child cannot fork (see `child_cfg`).
        let child_cfg = EngineConfig::from_decls(
            fork_child_decls(&cfg.decls),
            cfg.prelude_dir.clone(),
            cfg.project_lib.clone(),
        )
        .map_err(|e| HarnessError::Compile(format!("fork-child engine config: {e}")))?;
        Ok(Harness {
            tree: NodeTree::new(writer),
            cfg,
            run_id,
            child_cfg,
            provider,
            convos: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            escalations: Mutex::new(HashMap::new()),
        })
    }

    /// Drive one model turn via the provider over its `StreamSink` (see
    /// `provider::StreamDelta`/`StreamSink`) — the sink is still wired so the
    /// provider's own streaming path runs; nothing currently reads the
    /// deltas past draining the channel. Shared by the root turn loop and the
    /// fork/fanout answerer loops.
    async fn stream_turn(
        &self,
        transcript: &[Message],
        framing: Option<&str>,
    ) -> Result<engine::DrivenTurn, HarnessError> {
        let (tx, mut rx) = mpsc::unbounded_channel::<StreamDelta>();
        let provider = self.provider.as_ref();
        let drive_fut =
            engine::drive_model_turn(provider, transcript, self.cfg.max_tokens, framing, Some(tx));
        tokio::pin!(drive_fut);
        let result = loop {
            tokio::select! {
                res = &mut drive_fut => break res,
                Some(_delta) = rx.recv() => {}
            }
        };
        result.map_err(Into::into)
    }

    /// Drain `node`'s effect-trace buffer and write one `Event::Effect` per
    /// captured effect (mapping the stack tag to its effect name). Called after
    /// a turn's block runs, while the node is still `Running`.
    ///
    /// The durable log is the audit contract: on the first append failure,
    /// this stops, restores the failed record and every record after it
    /// (in their original order, ahead of anything a concurrent path has
    /// pushed into `effect_trace` since the drain) back into the node's
    /// trace, and returns the error — `effect_seq` only ever advances past
    /// the records that actually landed in the log. A caller propagates the
    /// error rather than treating the turn as complete.
    fn flush_effects(&self, node: NodeId) -> Result<(), HarnessError> {
        let (records, seq0) = {
            let mut convos = self.convos.lock();
            let Some(convo) = convos.get_mut(&node) else {
                return Ok(());
            };
            let records: Vec<EffectRecord> = convo
                .effect_trace
                .lock()
                .map(|mut t| std::mem::take(&mut *t))
                .unwrap_or_default();
            (records, convo.effect_seq)
        };
        if records.is_empty() {
            return Ok(());
        }

        let mut seq = seq0;
        let mut iter = records.into_iter();
        let mut failure: Option<HarnessError> = None;
        while let Some(rec) = iter.next() {
            let tag = self
                .cfg
                .effect_names
                .get(rec.tag as usize)
                .cloned()
                .unwrap_or_else(|| format!("tag{}", rec.tag));
            match self
                .tree
                .effect(node, seq, tag, rec.req.clone(), rec.resp.clone())
            {
                Ok(()) => seq += 1,
                Err(e) => {
                    let mut unwritten = vec![rec];
                    unwritten.extend(iter);
                    if let Some(convo) = self.convos.lock().get_mut(&node) {
                        if let Ok(mut trace) = convo.effect_trace.lock() {
                            unwritten.append(&mut trace);
                            *trace = unwritten;
                        }
                    }
                    failure = Some(e.into());
                    break;
                }
            }
        }

        if let Some(convo) = self.convos.lock().get_mut(&node) {
            convo.effect_seq = seq;
        }
        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Acquire `node`'s per-node turn lease, serializing the snapshot →
    /// provider await → log append → resident run → outcome publish span of
    /// one turn-owning operation against a second concurrent one on the SAME
    /// node. Errors with [`HarnessError::TurnInFlight`] if another turn
    /// already holds it. Acquire at exactly one layer per turn — a leaf
    /// orchestration entry point that owns a whole turn (`drive_turn`,
    /// `summarize_turn`, an `answer_*` method) — never inside a loop over one
    /// of those, and never twice on the same node within one call chain: a
    /// nested acquire on a still-held lease deadlocks the node against
    /// itself, since this fails fast rather than blocking.
    fn acquire_turn_lease(&self, node: NodeId) -> Result<TurnLease<'_>, HarnessError> {
        let mut convos = self.convos.lock();
        let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
        if convo.turn_lease {
            return Err(HarnessError::TurnInFlight(node));
        }
        convo.turn_lease = true;
        Ok(TurnLease {
            harness: self,
            node,
        })
    }

    /// Read-only handle to the node tree (state/children/parent queries for the
    /// protocol server's tree pane).
    pub fn tree(&self) -> &NodeTree<Session> {
        &self.tree
    }

    /// This harness's engine config — the self-iterating harness driver
    /// reads `prelude_dir`/`project_lib` off it to build the OUTER
    /// session's own (narrower) `EngineConfig`, so the outer `Eff
    /// '[RunLLMTurn]` compile and this nested Agent's compile resolve
    /// author-defined types (e.g. a harness's own `Decision`) from the SAME
    /// module — required for a value to cross between them via `resume`.
    pub fn cfg(&self) -> &EngineConfig {
        &self.cfg
    }

    /// `node`'s live heap/GC snapshot, straight off its resident
    /// `JitEffectMachine` — what the observatory heap pane renders. `None`
    /// when `node` has no live session (never forced, terminal) or during the
    /// transient mid-turn gap while its session runs on the blocking pool.
    pub fn heap_stats(&self, node: NodeId) -> Option<HeapSummary> {
        let sid = self.tree.session_of(node)?;
        let stats = self.tree.registry().peek(sid, Session::heap_stats)??;
        Some(HeapSummary {
            nursery_bytes: stats.nursery_bytes,
            live_bytes: stats.live_bytes,
            gc_count: stats.gc_count,
        })
    }

    /// Create a ROOT node as a thunk with the DEFAULT system framing
    /// ([`engine::SYSTEM_FRAMING`]). `title` seeds the teaser + first user
    /// turn. Equivalent to [`Self::create_root_framed`] with `framing: None`.
    pub fn create_root(&self, title: &str, prompt: &str) -> Result<NodeId, HarnessError> {
        self.create_root_framed(title, prompt, None)
    }

    /// Create a ROOT node as a thunk with an EXPLICIT per-node system message
    /// `framing` (overriding the default [`engine::SYSTEM_FRAMING`] once the
    /// node is forced). `title` seeds the teaser + first user turn.
    /// The self-iterating harness's per-loop answerer session uses this to
    /// install `render`'s output as the answerer's system prompt.
    pub fn create_root_framed(
        &self,
        title: &str,
        prompt: &str,
        framing: Option<String>,
    ) -> Result<NodeId, HarnessError> {
        let node = self.tree.create_node(
            None,
            title,
            self.cfg.effect_names.clone(),
            ForkShape::Exact(0),
            false,
        )?;
        // Seed the (not-yet-live) transcript with the operator's opening prompt
        // and the node's framing. The convo entry is created lazily at force
        // time; stash both in the pending seed map until then.
        self.pending.lock().insert(
            node,
            NodeSeed::Root {
                prompt: prompt.to_string(),
                framing,
            },
        );
        Ok(node)
    }

    /// Force a thunk node: emit `Forced`, register its (as-yet machine-less)
    /// resident session, seed its transcript with the opening prompt. Returns
    /// the node's session id. The node's session machine comes up lazily, on
    /// its first REAL turn (`ResidentSession::unbootstrapped` — see that
    /// constructor's doc) — `force` itself pays no GHC extract compile.
    pub fn force(&self, node: NodeId, actor: Actor) -> Result<(), HarnessError> {
        // Register a fresh resident session for this node, keeping a handle to
        // its effect-trace buffer so per-turn effects can be logged.
        let (stack, effect_trace) = self.build_stack();
        // Give the node its OWN decl plane so declarations accumulate across its
        // turns (a value bound in turn N is a live binding in turn N+1). Each
        // node's plane is rooted in its own directory, so a fork parent's
        // declarations survive independently of any child's — the child forces a
        // separate node with a separate plane. Degrades to no accumulation
        // (`None`) if the session root cannot be created.
        let lib = self.node_decl_plane(node);
        let session = ResidentSession::unbootstrapped(
            stack,
            self.cfg.suspend_tag,
            self.cfg.effect_names.clone(),
            CapturedOutput::new(),
            self.cfg.include.clone(),
            DEFAULT_NURSERY_SIZE,
            lib,
        );

        // Register with the tree AND the session registry it owns (emits
        // Forced before the session is visible, then mints the SessionId and
        // inserts the machine as Idle — `NodeTree::force`'s one job).
        self.tree.force(node, actor, session)?;

        // Seed the transcript: a fork/fanout answerer inherits its parent's
        // transcript (staged by `register_fork_child`); a plain root gets its
        // opening prompt (staged by `create_root_framed`). Mutually exclusive
        // by construction — a node id is seeded exactly once, by whichever
        // path created it.
        let mut convos = self.convos.lock();
        let seed = self.pending.lock().remove(&node);
        let (transcript, framing) = match seed {
            // A fork/fanout answerer inherits its parent's transcript (turns
            // already in the log — nothing to re-log) AND the parent's framing,
            // so the child's request prefix is byte-identical to the parent's
            // through the fork checkpoint (exact-context fork).
            Some(NodeSeed::Forked {
                transcript,
                framing,
            }) => (transcript, framing),
            // A plain root: log its opening prompt as a User turn so the
            // transcript shows what was asked, not just the model's reply
            // (symmetric with the assistant `turn_delta` in `drive_turn`).
            Some(NodeSeed::Root { prompt, framing }) => {
                // An empty seed (the self-iterating harness's framing-only
                // answerer, `create_root_framed(_, "", _)`) has no opening
                // user turn to log or carry — its context comes from
                // `framing` alone. Logging/transcribing an empty turn would
                // be a false record.
                let transcript = if prompt.is_empty() {
                    Vec::new()
                } else {
                    self.tree
                        .turn_delta(node, 0, Role::User, prompt.clone(), None)?;
                    vec![Message {
                        role: Role::User,
                        content: prompt,
                        reasoning_items: Vec::new(),
                    }]
                };
                (transcript, framing)
            }
            None => {
                self.tree
                    .turn_delta(node, 0, Role::User, "Begin.".to_string(), None)?;
                (
                    vec![Message {
                        role: Role::User,
                        content: "Begin.".to_string(),
                        reasoning_items: Vec::new(),
                    }],
                    None,
                )
            }
        };
        convos.insert(
            node,
            NodeConvo {
                transcript,
                turn_seq: 0,
                effect_trace,
                effect_seq: 0,
                pending: None,
                answer_contract: None,
                suspend_table: None,
                suspend_asks: AsksSidecar::default(),
                pending_bind: None,
                usage: Usage::default(),
                last_input_tokens: 0,
                framing,
                turn_lease: false,
            },
        );
        Ok(())
    }

    /// Build the concrete handler stack (rooted at the process CWD sandbox),
    /// wrapped in a [`TracingDispatcher`] that records every effect into the
    /// returned [`EffectTrace`] — the harness drains it per turn to write
    /// `Event::Effect` (the observatory's trace pane).
    fn build_stack(&self) -> (BoxedStack, EffectTrace) {
        let cfg = tidepool_handlers::HandlerConfig {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            kv_path: tidepool_runtime::paths::cache_dir().join("harness-kv.json"),
            llm_model: std::env::var("TIDEPOOL_LLM_MODEL")
                .unwrap_or_else(|_| "gpt-4o-mini".to_string()),
        };
        let trace: EffectTrace = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stack =
            TracingDispatcher::new(tidepool_handlers::build_base_stack(&cfg), trace.clone());
        (Box::new(stack), trace)
    }

    /// Create a fresh, per-node decl plane rooted in its OWN directory, so a
    /// declaration turn accumulates for that node alone (independent of every
    /// other node's plane). Returns `None` — degrading to no cross-turn
    /// accumulation — if the root can't be prepared. The validation include is
    /// the node's compile include, so a decl can resolve the same imports a turn
    /// does (stdlib, the generated effects module).
    fn node_decl_plane(&self, node: NodeId) -> Option<SessionLib> {
        let root = node_session_dir(&self.run_id, node);
        // Fresh: clear any stale gen modules left by a prior run of THIS
        // instance at this node id. Scoped under `run_id`, so this can never
        // reach into another live Harness's node dir.
        let _ = std::fs::remove_dir_all(&root);
        SessionLib::open(SessionId(node.0), &root, ModuleEnv::standalone_default())
            .map(|lib| lib.with_validation_include(self.cfg.include.clone()))
            .ok()
    }
}

/// The one place the node decl-plane path shape is constructed:
/// `<cache>/harness-sessions/<run_id>/node-<node-id>`. `run_id` scopes the
/// whole subtree to ONE `Harness` instance (see [`generate_run_id`]), so two
/// Harnesses sharing a cache root — two processes, or two instances in one
/// process — never resolve to the same node directory: without this scoping,
/// both number nodes from 0 and `node_decl_plane`'s `remove_dir_all` on
/// node-0 creation would delete the survivor's live declarations.
fn node_session_dir(run_id: &str, node: NodeId) -> PathBuf {
    tidepool_runtime::paths::cache_dir()
        .join("harness-sessions")
        .join(run_id)
        .join(format!("node-{}", node.0))
}

/// Process-lifetime counter backing [`generate_run_id`] — process id alone is
/// not a unique run identity, since one process can hold more than one
/// `Harness` (e.g. a parent + fork-child harness, or two in one test binary).
static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A run identity unique to this `Harness::new` call: `<pid>-<nanos>-<seq>`.
/// `seq` (a per-process monotonic counter) alone already guarantees
/// in-process uniqueness; `pid` distinguishes concurrent processes; the
/// wall-clock component is extra defense against pid reuse across a long
/// uptime (relevant only to [`sweep_stale_run_dirs`], which reads `pid` back
/// out of the directory name).
fn generate_run_id() -> String {
    let pid = std::process::id();
    let seq = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{pid}-{nanos}-{seq}")
}

/// Best-effort cleanup of run-scoped session dirs left behind by processes
/// that exited without ever tearing down (killed, crashed, `kill -9`'d).
/// NOT a daemon: runs synchronously, once, inline in [`Harness::new`].
///
/// Deletion is gated on PID LIVENESS, not age: a dir is removed only when
/// `/proc/<pid>` (parsed back out of the `<pid>-<nanos>-<seq>` dir name — see
/// [`generate_run_id`]) does not exist, i.e. the owning process is
/// *provably* gone. This is what makes the sweep safe to run unconditionally
/// at every construction: it can never touch a run dir whose process is
/// still alive, however old the dir looks, so it cannot race a live Harness
/// (long-idle or otherwise) the way an age-only sweep could. Linux-only
/// (`/proc`); on any other platform (or if `/proc` is unreadable) this is a
/// silent no-op — stale dirs just accumulate, which is the documented
/// trade-off for not building a daemon.
fn sweep_stale_run_dirs() {
    let proc_dir = Path::new("/proc");
    if !proc_dir.is_dir() {
        return;
    }
    let root = tidepool_runtime::paths::cache_dir().join("harness-sessions");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(pid) = name.split('-').next() else {
            continue;
        };
        if !proc_dir.join(pid).exists() {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

impl Harness {
    /// Drive ONE model turn on `node`: assemble its transcript, call the
    /// provider, extract + compile + run the block against the node's session.
    /// Logs the assistant turn and the resident outcome. Returns the turn
    /// outcome (Completed / Suspended / NoBlock).
    ///
    /// This is the inner step of the turn loop; [`Self::run_to_hole_or_done`]
    /// loops it until the node suspends, completes, or hits the turn cap.
    pub async fn drive_turn(&self, node: NodeId) -> Result<engine::TurnOutcome, HarnessError> {
        // Hold the node's turn lease for this whole turn — snapshot,
        // provider await, log append, resident run, and outcome publish are
        // all below, and a second concurrent turn on the same node must fail
        // fast rather than race the snapshot-then-release-then-await window.
        let _lease = self.acquire_turn_lease(node)?;

        // Snapshot the transcript AND the node's framing under the lock, then
        // release before the await.
        let (transcript, turn_seq, framing) = {
            let convos = self.convos.lock();
            let convo = convos.get(&node).ok_or(HarnessError::NoSession(node))?;
            (
                convo.transcript.clone(),
                convo.turn_seq,
                convo.framing.clone(),
            )
        };

        let provider_started = std::time::Instant::now();
        let driven = self.stream_turn(&transcript, framing.as_deref()).await?;
        timing::record_stage(
            node.0,
            timing::NO_ROUND,
            timing::STAGE_PROVIDER_CALL,
            provider_started.elapsed(),
            0,
        );
        self.tree.turn_delta_reasoned(
            node,
            turn_seq,
            Role::Assistant,
            driven.reply.clone(),
            Some(driven.usage),
            driven.reasoning.clone(),
        )?;
        {
            let mut convos = self.convos.lock();
            let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
            convo.transcript.push(Message {
                role: Role::Assistant,
                content: driven.reply.clone(),
                reasoning_items: driven.reasoning_items.clone(),
            });
            convo.turn_seq += 1;
            convo.usage.input_tokens += driven.usage.input_tokens;
            convo.usage.output_tokens += driven.usage.output_tokens;
            // The latest turn's input_tokens IS the node's real context
            // size (the provider re-sends the whole transcript each round, so
            // its input count already includes every prior turn). Overwrite,
            // don't accumulate — this is the high-water the compaction
            // threshold reads.
            convo.last_input_tokens = driven.usage.input_tokens;
        }

        let Some(block) = driven.block else {
            // A prose-only turn ran no Haskell, but
            // still record a `TurnStart` whose `source` is the reply text — so a
            // node's durable log always shows one `TurnStart` per model turn
            // (`tail`ing it never has a silent gap), and consent integrity's
            // "no Turn/Effect before Forced" holds (this is well after Forced).
            self.tree.turn_start(node, driven.reply.clone(), None)?;
            return Ok(engine::TurnOutcome::NoBlock {
                reply: driven.reply,
            });
        };

        // Record the EXTRACTED executed Haskell as this turn's
        // `TurnStart.source` — so `tail -f <log>` shows the exact block the
        // turn ran, not a coarse "model" provenance tag. Emitted before the
        // block runs, so it precedes this turn's Effect / HolePublished
        // events in the durable log.
        self.tree.turn_start(node, block.clone(), None)?;
        // INFO, not DEBUG: a person watching the console must see the exact
        // source every compile ran (dogfood-observability deliverable 1) —
        // full text, never truncated (a pathologically large source is
        // itself signal worth seeing).
        tracing::info!(node = node.0, source = %block, "compiled turn source");

        // Compile + run the block synchronously (spawn_blocking off the reactor).
        let (imports, body) = engine::split_imports(&block);
        self.run_block(node, &body, &imports, "").await
    }

    /// Drive ONE plain model turn on `node`: push `prompt` as a User message,
    /// call the provider, log the assistant reply, and return its RAW TEXT plus
    /// that single turn's [`Usage`] — WITHOUT compiling or running any Haskell
    /// block. Unlike [`Self::drive_turn`], the model's answer is captured as
    /// prose, not executed; the node's session is untouched (still idle), so it
    /// can keep driving afterward.
    ///
    /// This is the self-iterating harness's simplified compaction primitive:
    /// compaction is ONE ordinary turn on the answerer
    /// session that ALREADY holds the full context — "summarize everything
    /// above" — no second node, no `finalize`, no serializing the transcript
    /// into a prompt (the model has it in context). The caller then resets the
    /// session's context to the returned summary via
    /// [`Self::replace_transcript_with_summary`].
    pub async fn summarize_turn(
        &self,
        node: NodeId,
        prompt: &str,
    ) -> Result<(String, Usage), HarnessError> {
        // Same whole-turn lease discipline as `drive_turn`.
        let _lease = self.acquire_turn_lease(node)?;

        // Push the summarize request, then snapshot the transcript + framing.
        self.push_user_turn(node, prompt)?;
        let (transcript, turn_seq, framing) = {
            let convos = self.convos.lock();
            let convo = convos.get(&node).ok_or(HarnessError::NoSession(node))?;
            (
                convo.transcript.clone(),
                convo.turn_seq,
                convo.framing.clone(),
            )
        };

        self.tree.turn_start(node, "model".to_string(), None)?;
        let driven = self.stream_turn(&transcript, framing.as_deref()).await?;
        self.tree.turn_delta_reasoned(
            node,
            turn_seq,
            Role::Assistant,
            driven.reply.clone(),
            Some(driven.usage),
            driven.reasoning.clone(),
        )?;
        {
            let mut convos = self.convos.lock();
            let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
            convo.transcript.push(Message {
                role: Role::Assistant,
                content: driven.reply.clone(),
                reasoning_items: driven.reasoning_items.clone(),
            });
            convo.turn_seq += 1;
            convo.usage.input_tokens += driven.usage.input_tokens;
            convo.usage.output_tokens += driven.usage.output_tokens;
            convo.last_input_tokens = driven.usage.input_tokens;
        }
        Ok((driven.reply, driven.usage))
    }

    /// Compile a `block` (with optional imports/helpers) and run it against
    /// `node`'s resident session as a TOP-LEVEL turn. Classifies a suspension.
    ///
    /// ONE `run_turn` spawn classifies and compiles together — the verdict
    /// (decl/bind/expr) is not known until it returns, so every template it
    /// might select is built up front, from the SAME session/contract context
    /// a compile would have needed anyway.
    async fn run_block(
        &self,
        node: NodeId,
        block: &str,
        imports: &str,
        helpers: &str,
    ) -> Result<engine::TurnOutcome, HarnessError> {
        // Session/contract context, peeked under the lock WITHOUT checking the
        // session out, so a compile failure below never leaks it (the session
        // is taken only once a compiled fragment is in hand).
        let (session_module, session_include) = self.session_decl_context(node);
        let bind_ctx = self.session_bind_context(node);

        // The EXPR template's imports + row: the node's answer contract (when
        // driving toward a `finalize`) contributes both halves — its `imports`
        // put the answer type in scope, and its `ty` instantiates the ROW
        // (`Finalize <ty>`) this turn compiles against — ONE computation
        // (`turn_target`) resolves both the include dir and the stack string
        // from the SAME row, so they cannot disagree.
        let contract = self.answer_contract(node);
        let mut expr_import_lines: Vec<String> = contract
            .iter()
            .flat_map(|c| c.imports.iter().cloned())
            .collect();
        if !imports.is_empty() {
            expr_import_lines.push(imports.to_string());
        }
        expr_import_lines.extend(session_module.clone());
        let expr_imports = expr_import_lines.join("\n");
        let target = self.cfg.turn_target(
            contract
                .as_ref()
                .map(|c| (c.ty.as_str(), c.imports.as_slice())),
        )?;

        // The BIND/BINDDISCARD templates' imports: user imports + the decl
        // module + current Val modules — just the user imports when the node
        // has no decl plane.
        let bind_imports = match &bind_ctx {
            Some((session_imports, ..)) => match (imports.is_empty(), session_imports.is_empty()) {
                (_, true) => imports.to_string(),
                (true, false) => session_imports.clone(),
                (false, false) => format!("{imports}\n{session_imports}"),
            },
            None => imports.to_string(),
        };

        // Build every template `run_turn` might select — the verdict, and so
        // which one applies, isn't known until it returns. Resolved BEFORE
        // this window (contract/session context above): for a new answer type
        // that materializes an effects module (a filesystem write), which is
        // not templating cost and would inflate this stage's attribution.
        let template_started = std::time::Instant::now();
        let expr_source =
            engine::expr_turn_template(&self.cfg, &target.stack, block, &expr_imports, helpers);
        let bind_source =
            engine::session_bind_template(&self.cfg, "{{BINDERS}}", &bind_imports, helpers);
        // BindDiscard: the same bind shape, yielding `pure ()` and splicing no
        // binder (a literal `"()"`, not a `{{BINDERS}}` placeholder).
        let binddiscard_source =
            engine::session_bind_template(&self.cfg, "()", &bind_imports, helpers);
        let templates = vec![
            TurnTemplate {
                kind: TemplateSelector::Decl,
                source: DECL_TEMPLATE_SOURCE.to_string(),
            },
            TurnTemplate {
                kind: TemplateSelector::Bind,
                // Kept alongside (see below) — a compile failure carries no
                // verdict tag, so a corrective error needs both candidate
                // sources to remap against.
                source: bind_source.clone(),
            },
            TurnTemplate {
                kind: TemplateSelector::BindDiscard,
                source: binddiscard_source,
            },
            TurnTemplate {
                kind: TemplateSelector::Expr,
                source: expr_source.clone(),
            },
        ];
        timing::record_stage(
            node.0,
            timing::NO_ROUND,
            timing::STAGE_TEMPLATE,
            template_started.elapsed(),
            0,
        );

        // include: the contract-aware target include, plus the decl-plane dir
        // (if any) — the same path `bind_ctx`'s session root resolves to.
        let mut include = target.include;
        if let Some(dir) = session_include {
            include.push(dir);
        }

        // session-root/inject/gen: from the bind context when the node has a
        // decl plane; a scratch dir otherwise. `run_turn` carries these
        // unconditionally (the decl/expr verdicts don't use them, but the wire
        // is unconditional), so a node with no decl plane at all (a rare
        // degrade — see `force`'s `node_decl_plane`) still needs SOME writable
        // directory to hand the extract; a real Bind verdict on such a node is
        // rejected below (PRESERVE step) regardless of what the extract did
        // with this scratch root.
        let scratch_root;
        let (session_root, inject_modules, gen) = match &bind_ctx {
            Some((_, inject, root, gen)) => (root.clone(), inject.clone(), gen.0),
            None => {
                scratch_root = tempfile::TempDir::new()
                    .map_err(|e| HarnessError::Resident(format!("scratch session root: {e}")))?;
                (scratch_root.path().to_path_buf(), Vec::new(), 0)
            }
        };

        let block_owned = block.to_string();
        let req_block = block_owned.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let include_refs: Vec<&Path> = include.iter().map(PathBuf::as_path).collect();
            let req = TurnRequest {
                turn_text: &req_block,
                templates: &templates,
                include: &include_refs,
                session_root: &session_root,
                inject_modules: &inject_modules,
                gen,
                verdict: None,
                target: None,
            };
            run_turn(req)
        })
        .await
        .map_err(|e| HarnessError::Resident(format!("turn compile task join: {e}")))?
        .map_err(|e| {
            HarnessError::Compile(render_compile_error(&e, block, &expr_source, &bind_source))
        })?;

        match outcome {
            TurnResult::Decl { .. } => {
                let checkout = self.checkout_run(node)?;
                let res = self
                    .run_checked_out(node, checkout, move |mut session| {
                        let r = session.define_scoped(&[&block_owned]);
                        (session, r)
                    })
                    .await?;
                self.flush_effects(node)?;
                match res {
                    Ok(gen) => {
                        let rendered = format!("declared (gen {})", gen.0);
                        self.tree.node_done(node, rendered.clone())?;
                        Ok(engine::TurnOutcome::Completed { rendered })
                    }
                    Err(e) => {
                        let msg = format!("The declaration failed: {e}");
                        self.push_user_turn(node, &msg)?;
                        Err(HarnessError::Resident(e.to_string()))
                    }
                }
            }
            // A value-plane BIND turn (`x <- e`) materializes its result into
            // the node's value plane so a later turn can reference it. ONE
            // name only: a multi-binder pattern is rejected below rather than
            // partially materialized.
            TurnResult::Bind {
                binders,
                bound,
                compiled,
                ..
            } if !binders.is_empty() => {
                let Some((.., gen)) = bind_ctx else {
                    return Err(HarnessError::Resident(
                        "value-plane bind requires a node decl plane".into(),
                    ));
                };
                // A multi-binder pattern (`(a, b) <- e`) compiles fine and the
                // extract writes a thin iface naming EVERY bound name, but only
                // one binding is registered on this node's value plane below.
                // Taking the first and dropping the rest would leave the type
                // plane promising names the value plane cannot resolve — a
                // later turn referencing `b` would typecheck and then fail at
                // run time. Rejecting is the loud form of the same limit.
                if binders.len() > 1 {
                    let names = binders.join(", ");
                    let msg = format!(
                        "This node binds one name per turn; `{names}` binds {}. \
                         Bind them one at a time.",
                        binders.len()
                    );
                    self.push_user_turn(node, &msg)?;
                    return Err(HarnessError::Resident(msg));
                }
                let binder = bound.into_iter().next().ok_or_else(|| {
                    HarnessError::Resident("session-bind emitted no binder metadata".into())
                })?;
                self.run_bind_turn(node, binder, compiled, gen).await
            }
            // A discarding bind (`_ <- e`) or a bare expression: run for
            // effect/value, no binding materializes on the value plane.
            TurnResult::Bind { compiled, .. } | TurnResult::Expr { compiled, .. } => {
                self.log_turn_extracted(node, &compiled.asks, None)?;
                let asks = AsksSidecar::from_pairs(compiled.asks);
                let table = compiled.table;
                let expr = compiled.expr;

                // Run the compiled fragment against the session (move it onto
                // the blocking pool and back — the resident session is `Send`).
                let checkout = self.checkout_run(node)?;
                let run_table = table.clone();
                let run_outcome = self
                    .run_checked_out(node, checkout, move |mut session| {
                        let out = session.run("turn", &expr, &run_table);
                        (session, out)
                    })
                    .await?;

                self.finish_run(node, run_outcome, table, asks, None)
            }
        }
    }

    /// Shared turn epilogue: restore the session, flush effects, and turn a
    /// [`ResidentOutcome`] into a [`engine::TurnOutcome`] — `node_done` on
    /// completion, publish + `set_pending` on suspension. `pending_bind` is
    /// `Some` for a value-plane BIND turn that suspended (`x <- fork …`): it is
    /// stashed so `resume_parent` drives `resume_bind` and the binding
    /// materializes when the fork answers. A completion needs no `pending_bind`
    /// handling — `run_bind` already materialized it.
    fn finish_run(
        &self,
        node: NodeId,
        outcome: Result<ResidentOutcome, ResidentError>,
        table: DataConTable,
        asks: AsksSidecar,
        pending_bind: Option<(BoundBinder, Generation)>,
    ) -> Result<engine::TurnOutcome, HarnessError> {
        {
            let mut convos = self.convos.lock();
            if let Some(convo) = convos.get_mut(&node) {
                convo.suspend_table = Some(table.clone());
                convo.suspend_asks = asks.clone();
            }
        }
        self.flush_effects(node)?;

        match outcome {
            Ok(ResidentOutcome::Completed { result, .. }) => {
                let rendered = result.to_string_pretty();
                self.tree.node_done(node, rendered.clone())?;
                Ok(engine::TurnOutcome::Completed { rendered })
            }
            Ok(ResidentOutcome::Suspended { hole, request, .. }) => {
                let classified = engine::classify_hole(&request, &table, &asks);
                let fork = matches!(classified.routing, HoleRouting::Fork { .. });
                let ty = match &classified.routing {
                    HoleRouting::Fork { ty, .. }
                    | HoleRouting::RunLLMTurn { ty, .. }
                    | HoleRouting::Finalize { ty, .. } => ty.clone(),
                    _ => None,
                };
                let site = match &classified.routing {
                    HoleRouting::Fork { site, .. }
                    | HoleRouting::RunLLMTurn { site, .. }
                    | HoleRouting::Finalize { site, .. } => Some(crate::tree::SiteId(*site)),
                    _ => None,
                };
                self.tree.hole_published(
                    node,
                    HoleId(hole.clone()),
                    site,
                    ty,
                    classified.prompt.clone(),
                    fork,
                )?;
                self.set_pending(
                    node,
                    PendingHole {
                        hole: HoleId(hole.clone()),
                        classified: classified.clone(),
                        raw_request: request,
                    },
                );
                // A suspended bind: remember the binder+gen so the resume path
                // materializes it (via `resume_bind`) when the fork answers.
                if let Some(pb) = pending_bind {
                    let mut convos = self.convos.lock();
                    if let Some(convo) = convos.get_mut(&node) {
                        convo.pending_bind = Some(pb);
                    }
                }
                Ok(engine::TurnOutcome::Suspended { hole, classified })
            }
            Err(e) => {
                let msg = format!("The eval failed at runtime: {e}");
                self.push_user_turn(node, &msg)?;
                Err(HarnessError::Resident(e.to_string()))
            }
        }
    }

    /// Peek a node's value-plane compile context WITHOUT checking the session
    /// out (so a compile failure never leaks it): `(decl module import, live
    /// `Val.G` inject modules, session root, the next value generation to mint)`.
    /// `None` when the node has no session or no decl plane.
    fn session_bind_context(
        &self,
        node: NodeId,
    ) -> Option<(String, Vec<String>, PathBuf, Generation)> {
        let sid = self.tree.session_of(node)?;
        self.tree.registry().peek(sid, |s| {
            let root = s.lib_include_dir()?;
            // Imports: the decl `Lib.G<g>` module + the CURRENT `Val.G<g>` module
            // of each live name (newest gen only — shadowed gens are injected,
            // not imported, to avoid an ambiguous occurrence). Injection
            // (`--inject-val`) uses ALL live gens.
            let mut import_lines: Vec<String> = Vec::new();
            if let Some(m) = s.session_import_module() {
                import_lines.push(m);
            }
            import_lines.extend(s.current_val_modules());
            Some((
                import_lines.join("\n"),
                s.inject_val_modules(),
                root,
                s.val_gen().next(),
            ))
        })?
    }

    /// Materialize an ALREADY-COMPILED value-plane BIND turn (`x <- e`): run it
    /// against the resident session (`session.run_bind`), then hand off to the
    /// shared epilogue. Called from [`Self::run_block`] once `run_turn` has
    /// returned a `TurnResult::Bind` with a non-empty binder list and
    /// `session_bind_context` confirmed the node has a decl plane — `gen` is
    /// the SAME generation that compile stamped into `binder.module`. A fork
    /// bind suspends here and its value is materialized on resume
    /// (`finish_run` stashes the binder).
    async fn run_bind_turn(
        &self,
        node: NodeId,
        binder: BoundBinder,
        compiled: CompiledTurn,
        gen: Generation,
    ) -> Result<engine::TurnOutcome, HarnessError> {
        self.log_turn_extracted(
            node,
            &compiled.asks,
            Some((&binder.name, &binder.type_display)),
        )?;
        let asks = AsksSidecar::from_pairs(compiled.asks);
        let table = compiled.table;
        let expr = compiled.expr;

        let checkout = self.checkout_run(node)?;
        let binder_for_run = binder.clone();
        let run_table = table.clone();
        let outcome = self
            .run_checked_out(node, checkout, move |mut session| {
                let out = session.run_bind("bind", &expr, &run_table, &binder_for_run, gen);
                (session, out)
            })
            .await?;

        self.finish_run(node, outcome, table, asks, Some((binder, gen)))
    }

    /// Loop [`Self::drive_turn`] until the node SUSPENDS at a hole, COMPLETES,
    /// or hits the per-node turn cap. A pure-prose turn (NoBlock) feeds a nudge
    /// and loops; a turn whose block DOESN'T COMPILE feeds the GHC error back so
    /// the model can self-correct (the same type-retry the fork answerer gets —
    /// a common, fixable model mistake like wrong verb arity shouldn't cancel
    /// the whole node). Returns the terminal turn outcome; only exhausting the
    /// turn cap surfaces an error to the caller.
    pub async fn run_to_hole_or_done(
        &self,
        node: NodeId,
    ) -> Result<engine::TurnOutcome, HarnessError> {
        let mut turns = 0;
        loop {
            if turns >= self.cfg.max_turns {
                return Err(EngineError::NoBlock { turns }.into());
            }
            turns += 1;
            match self.drive_turn(node).await {
                Ok(
                    out @ (engine::TurnOutcome::Completed { .. }
                    | engine::TurnOutcome::Suspended { .. }),
                ) => return Ok(out),
                Ok(engine::TurnOutcome::NoBlock { .. }) => {
                    self.push_user_turn(
                        node,
                        "Reply with a single ```haskell block to run (or to answer the \
                         hole with `resume expr`).",
                    )?;
                }
                // The model's Haskell didn't compile — feed the GHC error back
                // verbatim (capped) as a corrective user turn and retry, rather
                // than cancelling the node. `turns` is this hole's corrective-
                // retry round index — a burned round must be visible while it
                // is happening, not just reconstructable afterwards.
                Err(HarnessError::Compile(msg)) => {
                    tracing::warn!(
                        node = node.0,
                        round = turns,
                        "compile attempt failed (round {turns} of {}, corrective retry)",
                        self.cfg.max_turns
                    );
                    let ghc = truncate_ghc_error(&msg);
                    self.push_user_turn(
                        node,
                        &format!(
                            "That Haskell did not compile. Fix it and reply with a \
                             corrected single ```haskell block. Common causes: a verb \
                             needs more arguments (e.g. `grepGlob pat path`), or you \
                             passed `Text` where a different type is expected.\n\n\
                             GHC error:\n{ghc}"
                        ),
                    )?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Continue a COMPLETED node's conversation with a new operator message —
    /// the multi-turn REPL follow-up. Reopens the `Done` node
    /// ([`NodeTree::reopen`]), appends `message` as a user turn, and drives to
    /// the next hole/done. The node's resident session persisted past its
    /// previous completion, so bindings and heap carry over — this continues
    /// the session rather than restarting it. Errors if the node isn't `Done`
    /// or its session is gone (e.g. it was cancelled).
    pub async fn follow_up(
        &self,
        node: NodeId,
        message: &str,
    ) -> Result<engine::TurnOutcome, HarnessError> {
        if !self.convos.lock().contains_key(&node) {
            return Err(HarnessError::NoSession(node));
        }
        self.tree.reopen(node)?;
        self.push_user_turn(node, message)?;
        match self.run_to_hole_or_done(node).await {
            Ok(out) => Ok(out),
            Err(e) => {
                // The follow-up turn failed: return the node to `Done` so the
                // prior conversation is preserved and still continuable, rather
                // than stranded `Running`. The failure is recorded as the turn's
                // result so it shows in the transcript.
                let _ = self.tree.node_done(node, format!("follow-up failed: {e}"));
                Err(e)
            }
        }
    }
}

// -- answering holes: fork, in-context return, operator dialog ---------------

impl Harness {
    /// The classified pending hole on `node`, if it is suspended.
    pub fn pending_hole(&self, node: NodeId) -> Option<ClassifiedHole> {
        self.convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.as_ref())
            .map(|p| p.classified.clone())
    }

    /// Like [`Self::pending_hole`], but also returns the hole id and the
    /// compile table the pending suspension's constructor ids resolve
    /// against — what a caller needs to build a
    /// [`engine::TurnOutcome::Suspended`] out of a `pending_hole` read (the
    /// self-iterating-harness driver's `AskUser` servicing loop, which reads
    /// the pending hole again after a resume rather than threading the
    /// original `drive_turn` outcome through). `None` if `node` isn't
    /// suspended or has no compile table recorded.
    pub(crate) fn pending_hole_full(
        &self,
        node: NodeId,
    ) -> Option<(HoleId, ClassifiedHole, DataConTable)> {
        let convos = self.convos.lock();
        let convo = convos.get(&node)?;
        let pending = convo.pending.as_ref()?;
        let table = convo.suspend_table.clone()?;
        Some((pending.hole.clone(), pending.classified.clone(), table))
    }

    /// Reconstruct a [`TurnOutcome::Suspended`] from `node`'s CURRENT pending
    /// hole (self-iterating-harness fork widen: after [`Self::answer_fanout`]/
    /// [`Self::answer_fork`] resumes a parent answerer, the driver needs the
    /// parent's freshly re-published hole in the same shape
    /// [`Self::drive_turn`] returns, without re-running a model turn).
    /// `None` if `node` isn't currently suspended.
    pub fn pending_turn_outcome(&self, node: NodeId) -> Option<TurnOutcome> {
        let convos = self.convos.lock();
        let convo = convos.get(&node)?;
        let pending = convo.pending.as_ref()?;
        Some(TurnOutcome::Suspended {
            hole: pending.hole.0.clone(),
            classified: pending.classified.clone(),
        })
    }

    /// The harness-level primitive `service_runllm_hole`
    /// (`selfharness/driver.rs`) calls once a nested Agent node
    /// suspends on `finalize @T x`: read the
    /// finalized value straight out of the suspended request `Value` (NEVER
    /// through JSON — it may carry a closure or other non-serializable
    /// value, per `finalize`'s relaxed function-arrow rule) and terminate
    /// the node.
    ///
    /// `finalize` does NOT resume the Agent (unlike answering a
    /// `RunLLMTurn`/`Fork` hole via [`Self::drive_answerer_to_value`]) — it
    /// TERMINATES the node's turn loop and hands the value UP, so this
    /// retires the node via [`Self::terminate_node`] with a `"finalized"`
    /// reason — a SUCCESSFUL termination, not a failure, even though the
    /// tree's own state name (`Cancelled`; a `Suspended` node has no
    /// `node_done` transition — that one is reserved for a turn that ran to
    /// completion from `Running`) reads that way. The caller is expected to
    /// `run_child` the returned `Value` into the OUTER (Harness-monad)
    /// session to resolve the parent `runLLMTurn` hole, zero-copy.
    ///
    /// Errors if `node` has no live session, isn't suspended, or its pending
    /// hole isn't `Finalize`-routed.
    pub fn take_finalized_value(&self, node: NodeId) -> Result<Value, HarnessError> {
        self.take_finalized_value_with_table(node).map(|(v, _)| v)
    }

    /// Extract the finalized value + its compile table out of `node`'s pending
    /// `Finalize` hole, clearing the pending hole but NOT terminating the node.
    /// The caller decides the node's next state (cancel via
    /// [`Self::take_finalized_value_with_table`], or keep it reopenable via
    /// [`Self::take_finalized_value_keep_open`]).
    fn take_finalized_value_core(
        &self,
        node: NodeId,
    ) -> Result<(Value, DataConTable), HarnessError> {
        let mut convos = self.convos.lock();
        let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
        let pending = convo
            .pending
            .as_ref()
            .ok_or(HarnessError::NotSuspended(node))?;
        if !matches!(pending.classified.routing, HoleRouting::Finalize { .. }) {
            return Err(HarnessError::RoutingMismatch {
                node,
                routing: "Finalize",
                actual: format!("{:?}", pending.classified.routing),
            });
        }
        let Value::Con(_, fields) = &pending.raw_request else {
            return Err(HarnessError::Resident(
                "finalize request was not a Con".into(),
            ));
        };
        let value = fields
            .get(1)
            .cloned()
            .ok_or_else(|| HarnessError::Resident("FinalizeWith missing its value field".into()))?;
        // A finalize suspension without its compile table is an
        // inconsistency — fail LOUD rather than defaulting to an empty
        // table, which would silently misrender the finalized value's
        // constructor ids.
        let table = convo.suspend_table.clone().ok_or_else(|| {
            HarnessError::Resident(
                "finalize suspension has no compile table (cannot resolve the value's \
                 constructor ids)"
                    .into(),
            )
        })?;
        convo.pending = None;
        Ok((value, table))
    }

    /// Like [`Self::take_finalized_value`], but also returns the
    /// [`DataConTable`] the finalized value's constructor ids resolve
    /// against — needed by a caller that renders the raw value itself
    /// (the self-iterating harness's forced compaction turn finalizes a
    /// `Text`, then reads it out via [`tidepool_runtime::value_to_json`],
    /// which needs the SAME table the value was compiled with) rather than
    /// just feeding it opaquely into another suspended continuation.
    /// TERMINATES the node via [`Self::terminate_node`] (`Cancelled`,
    /// session + convo retired) — the finalized node is done.
    pub fn take_finalized_value_with_table(
        &self,
        node: NodeId,
    ) -> Result<(Value, DataConTable), HarnessError> {
        let (value, table) = self.take_finalized_value_core(node)?;
        self.terminate_node(node, "finalized")?;
        Ok((value, table))
    }

    /// Like [`Self::take_finalized_value`], but keeps the node + its resident
    /// session LIVE and reusable instead of cancelling — the self-iterating
    /// harness's per-loop answerer reuses ONE node across the loop's
    /// holes so the model's transcript (the accumulating context window)
    /// persists, and hole #2 sees hole #1's exchange.
    ///
    /// Two things must happen for the reuse to work: (1) the TREE state goes
    /// `Suspended` → `Running` (`hole_consumed`) so a new turn is representable;
    /// (2) the RESIDENT SESSION's parked finalize continuation is ABORTED so it
    /// returns to idle — a suspended session REJECTS a new top-level turn
    /// (`ResidentError::Suspended`), and `finalize`'s continuation is terminal
    /// (nothing meaningful runs after it), so discarding it is exactly right.
    /// Without (2) the next hole's `drive_turn` cannot run a fresh block on the
    /// same session. Returns the finalized value (already read out of the
    /// suspended request by `take_finalized_value_core`, so aborting the
    /// continuation does not lose it) alongside its rendered JSON text — what
    /// the answer actually WAS, for the driver's `Finalize` narration event
    /// (dogfood-observability deliverable 4).
    pub(crate) fn take_finalized_value_keep_open(
        &self,
        node: NodeId,
    ) -> Result<(Value, String), HarnessError> {
        // Snapshot the pending finalize hole/continuation id BEFORE clearing it.
        let hole = {
            let convos = self.convos.lock();
            let convo = convos.get(&node).ok_or(HarnessError::NoSession(node))?;
            convo
                .pending
                .as_ref()
                .ok_or(HarnessError::NotSuspended(node))?
                .hole
                .clone()
        };
        let (value, table) = self.take_finalized_value_core(node)?;
        let rendered = tidepool_runtime::value_to_json(&value, &table, 0).to_string();

        // Abort the resident session's parked finalize continuation so the
        // session returns to idle and can run the NEXT hole's turn. `abort`
        // consumes the stowed continuation (clearing `pending` up front) and
        // then surfaces the abort as a terminal error outcome — that Err IS the
        // expected "continuation discarded" signal, not a failure, so it is
        // deliberately ignored. What matters is the session is now idle.
        let mut co = self.checkout_resume(node, &hole)?;
        let _ = co
            .machine()
            .abort(&hole.0, "finalize consumed (answerer reused)".to_string());
        debug_assert!(
            co.machine().is_idle(),
            "session must be idle after aborting the finalize continuation"
        );
        co.restore_idle();

        // Tree state: Suspended → Running, so the reused node accepts a new turn.
        self.tree.hole_consumed(node, hole)?;
        Ok((value, rendered))
    }

    /// Whether `node`'s pending finalize hole carries a CLOSURE value: the
    /// tolerant suspend bridge substituted a
    /// `CLOSURE_SENTINEL` placeholder for field 1, so the finalized value is a
    /// live closure kept in-heap (applied by reference), not data. `false` for a
    /// plain-data finalize, or when `node` isn't suspended on a finalize hole.
    pub fn finalize_is_closure(&self, node: NodeId) -> bool {
        let convos = self.convos.lock();
        let Some(convo) = convos.get(&node) else {
            return false;
        };
        let Some(pending) = convo.pending.as_ref() else {
            return false;
        };
        if !matches!(pending.classified.routing, HoleRouting::Finalize { .. }) {
            return false;
        }
        matches!(
            &pending.raw_request,
            Value::Con(_, fields)
                if fields.get(1).is_some_and(|f| matches!(
                    f,
                    Value::Con(id, cf)
                        if id.0 == u64::MAX && cf.is_empty()
                ))
        )
    }

    /// Apply a `finalize`d CLOSURE by reference:
    /// `node` must be suspended on a `finalize @(Int -> Int) f` hole whose value
    /// was kept LIVE in the shared heap (never deep-forced). This runs `f arg`
    /// in place against that same suspended heap — the "code as a value"
    /// round-trip — and returns the (data) result `Value`. The node stays
    /// suspended on its finalize hole afterward (the apply is a non-consuming
    /// child run against the suspended machine); the caller terminates it via
    /// [`Self::take_finalized_value`] when done.
    pub async fn apply_finalized_closure(
        &self,
        node: NodeId,
        arg: i64,
    ) -> Result<Value, HarnessError> {
        // Require the node to be suspended on a Finalize hole — the resident
        // session's parked continuation is what makes the child run (the apply)
        // legal, and the finalized closure's root was stashed on suspend.
        let is_finalize = matches!(
            self.pending_hole(node).map(|c| c.routing),
            Some(HoleRouting::Finalize { .. })
        );
        if !is_finalize {
            return Err(HarnessError::RoutingMismatch {
                node,
                routing: "Finalize",
                actual: format!("{:?}", self.pending_hole(node).map(|c| c.routing)),
            });
        }
        // The suspend turn's table, passed through so the apply fragment's
        // compile merges the closure's own defining constructors into the
        // accumulated session table (see `ResidentSession::apply_finalized`'s
        // doc for why no `I#`-id matching is needed here).
        let suspend_table = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.suspend_table.clone());
        let checkout = self.checkout_child(node)?;
        let out = self
            .run_checked_out(node, checkout, move |mut session| {
                let out = session.apply_finalized(arg, suspend_table.as_ref());
                (session, out)
            })
            .await?;
        self.flush_effects(node)?;
        out.map(|r| r.into_value())
            .map_err(|e| HarnessError::Resident(e.to_string()))
    }

    /// Reopen a `Done` answerer node (`Done` → `Running`) for another turn —
    /// the self-iterating harness's bounded answerer drive reuses ONE
    /// per-loop node, and a `Completed` (non-finalize) turn leaves it `Done`,
    /// so a corrective re-prompt must reopen it first. Mirrors
    /// [`Self::follow_up`]'s reopen step. No-op-safe only from `Done`
    /// ([`crate::forcing::NodeTree::reopen`] refuses other states).
    pub(crate) fn reopen_node(&self, node: NodeId) -> Result<(), HarnessError> {
        self.tree.reopen(node)?;
        Ok(())
    }

    /// The running sum of every assistant turn's [`Usage`] logged on `node`
    /// so far — what the self-iterating harness driver's emergency
    /// compaction trigger accumulates across the `runLLMTurn` answerer nodes
    /// it drives per loop. `None` if `node` has no live session.
    pub fn node_usage(&self, node: NodeId) -> Option<Usage> {
        self.convos.lock().get(&node).map(|c| c.usage)
    }

    /// The MOST RECENT turn's `input_tokens` on `node` — the node's real
    /// current context size (the provider re-sends the whole transcript each
    /// round, so its per-round input count already includes every prior turn).
    /// This is a HIGH-WATER mark, not a running sum: the self-iterating
    /// harness's compaction threshold reads THIS, never
    /// [`Self::node_usage`]'s summed `input_tokens`, which super-linearly
    /// over-counts across a multi-round hole. `Some(0)` before the node's
    /// first turn; `None` if `node` has no live session.
    pub fn node_last_input_tokens(&self, node: NodeId) -> Option<u64> {
        self.convos.lock().get(&node).map(|c| c.last_input_tokens)
    }

    /// `node`'s pending rung-2 escalation, if it is currently parked awaiting
    /// an operator decision — what the stuck-node popup renders.
    pub fn escalation_of(&self, node: NodeId) -> Option<Escalation> {
        self.escalations.lock().get(&node).map(|(e, _)| e.clone())
    }

    /// The first node currently parked on a rung-2 escalation, if any — what
    /// the inspector's stuck-node popup focuses by default (checked BEFORE
    /// the plain operator-hole focus, since an escalated answerer blocks fan
    /// progress and has no pending hole of its own to otherwise surface it).
    pub fn first_escalated_node(&self) -> Option<NodeId> {
        self.escalations.lock().keys().min().copied()
    }

    /// Resolve `node`'s pending rung-2 escalation with the operator's
    /// `decision` (driven by the web `/steer/:node` endpoint, or fired
    /// directly in a test to simulate the popup). Errors with
    /// [`HarnessError::NoPendingEscalation`] if `node` has no escalation
    /// parked (already resolved, or never escalated).
    pub fn resolve_escalation(
        &self,
        node: NodeId,
        decision: OperatorDecision,
    ) -> Result<(), HarnessError> {
        let (_, tx) = self
            .escalations
            .lock()
            .remove(&node)
            .ok_or(HarnessError::NoPendingEscalation(node))?;
        tx.send(decision).map_err(|_| {
            HarnessError::Resident(format!(
                "node {node:?}: operator decision could not be delivered (its wait was already abandoned)"
            ))
        })
    }

    /// Cancel + retire a fork/fanout CHILD that failed mid-drive, so no error
    /// path leaves it `Running` with a live resident session: every fallible
    /// step after forcing the child (a provider/join/log fault inside
    /// `drive_answerer_to_value`, or a `resume_parent` failure after) must
    /// route through this explicit cleanup instead of propagating via `?`
    /// and orphaning the child. Scoped to the fork/fanout callers
    /// deliberately — NOT baked into `drive_answerer_to_value` itself, which
    /// is also called in-context with `answerer == the main node`, where
    /// cancelling "the child" would kill the live agent. `terminate_node` is
    /// idempotent, so this is safe to repeat even against the internal abort
    /// paths that already retired the child (cap-exhaustion /
    /// `ChildSuspended`).
    fn cleanup_failed_child(&self, child: NodeId) {
        let _ = self.terminate_node(child, "fork child failed");
    }

    /// Force + drive a FORK answerer for `node`'s pending single-fork hole
    /// (`fork @T` / `runLLMTurnFork @T`, `fan: None` — a fanout hole routes to
    /// [`Self::answer_fanout`] instead). Registers a child node (transcript
    /// forked at the checkpoint, framing inherited), forces it, drives
    /// its turn loop until it produces an answering block, runs that block
    /// via `run_child` against the parent, and resumes the parent with the
    /// resulting typed Value. The ONE deliberate ill-typed attempt in the golden
    /// path exercises the GHC-verbatim retry here (a compile failure feeds back
    /// as the child's next user turn; the parent's continuation is untouched).
    pub async fn answer_fork(&self, node: NodeId, actor: Actor) -> Result<NodeId, HarnessError> {
        // `node` (the parent) owns this whole answer — force+drive the
        // child, then `resume_parent` — as one turn-owning operation.
        let _lease = self.acquire_turn_lease(node)?;
        let pending = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.clone())
            .ok_or(HarnessError::NotSuspended(node))?;
        let (site_ty, prompt) = match &pending.classified.routing {
            HoleRouting::Fork { ty, fan: None, .. } => {
                (ty.clone(), pending.classified.prompt.clone())
            }
            other => {
                return Err(HarnessError::RoutingMismatch {
                    node,
                    routing: "fork",
                    actual: format!("{other:?}"),
                })
            }
        };

        // Register the fork child: inherit the parent transcript up to the
        // parent's current turn, append the hole card.
        let child = self.register_fork_child(node, "fork answerer", &prompt, site_ty.as_deref())?;
        if let Err(e) = self.force(child, actor) {
            self.cleanup_failed_child(child);
            return Err(e);
        }

        // Drive the child's turn loop until it emits an answering block, then run
        // that block via run_child against the SUSPENDED PARENT (not the child's
        // own session) to produce a Value in the parent's heap. On ANY failure
        // (provider/join/log fault, or cap-exhaustion abort) clean up the child
        // before propagating — no orphaned Running+resident node.
        let answer_value = match self
            .drive_answerer_to_value(
                child,
                node,
                site_ty.as_deref(),
                self.cfg.max_turns,
                &self.child_cfg,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                self.cleanup_failed_child(child);
                return Err(e);
            }
        };

        // Resume the parent with the child's typed answer.
        if let Err(e) = self.resume_parent(node, &pending.hole, answer_value).await {
            self.cleanup_failed_child(child);
            return Err(e);
        }
        // The child answerer node is done once it has produced the answer.
        let _ = self.tree.node_done(child, "answer delivered".to_string());
        let _ = self.terminate_node(child, "answer delivered");
        Ok(child)
    }

    /// Force + drive a FANOUT answerer set for `node`'s pending fanout hole
    /// (`forkAll @T` / `runLLMTurnFanout @T`, `HoleRouting::Fork` with
    /// `fan: Some(_)`). One park, N thunk children — each registered under
    /// `node` (transcript forked at the checkpoint, same discipline as
    /// [`Self::answer_fork`]), forced, and driven to an answering value IN
    /// DECLARATION ORDER: children serialize against the parked parent's single
    /// heap (`run_child` only ever touches one machine at a time), so this needs
    /// no new `Slot` state beyond what a plain fork already uses. Each child gets
    /// its own turn-cap budget (`cfg.max_child_turns`) rather than the whole-node
    /// cap. The N raw per-child `T` values are assembled into a genuine `[T]`
    /// `Value` (the same raw-representation discipline a single fork's
    /// `unsafeCoerce` relies on) and resume the parent exactly once.
    ///
    /// A child that exhausts its `max_child_turns` budget does NOT hard-fail
    /// straight out of this loop anymore: [`Self::drive_answerer_to_value`]
    /// runs the escalation ladder internally (auto corrective-retry, then an
    /// operator popup) before ever returning an error. If a child is still
    /// stuck after the operator aborts, that child is CANCELLED inside
    /// `drive_answerer_to_value` (never left `Running`) before the error
    /// propagates here via `?` — this function does nothing further: earlier
    /// children in the loop are already terminal (`Done`), later ones were
    /// never created, and `node` (the parent) is simply never resumed, so it
    /// stays `Suspended` on its original fanout hole, re-answerable.
    pub async fn answer_fanout(
        &self,
        node: NodeId,
        actor: Actor,
    ) -> Result<Vec<NodeId>, HarnessError> {
        // `node` (the fanout parent) owns this whole answer — force+drive
        // every child in turn, then `resume_parent` once — as one
        // turn-owning operation. Each child gets its own fresh `NodeConvo`
        // (no lease to acquire on it here); only `node`'s lease is held.
        let _lease = self.acquire_turn_lease(node)?;
        let pending = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.clone())
            .ok_or(HarnessError::NotSuspended(node))?;
        let (list_ty, fan, prompts) = match &pending.classified.routing {
            HoleRouting::Fork {
                ty,
                fan: Some(fan),
                prompts,
                ..
            } => (ty.clone(), *fan, prompts.clone()),
            other => {
                return Err(HarnessError::RoutingMismatch {
                    node,
                    routing: "fanout",
                    actual: format!("{other:?}"),
                })
            }
        };
        let element_ty = list_ty.as_deref().and_then(engine::strip_list_type);

        // Cardinality integrity: the Haskell side's `fan` is the ONE
        // authoritative child count (it is `length prompts` at the
        // `runLLMTurnFanoutSited` call, before serialization). Every prompt
        // element must have decoded to a `Text` brief — `classify_hole`'s
        // `filter_map(as_str)` SILENTLY DROPS a non-string element, so a
        // shorter `prompts` than `fan` means one was lost. Answering anyway
        // would resume the parent with a `[T]` shorter than its `forkAll`
        // promised (a length the type system already committed to). Fail loud
        // instead of under-answering. (An empty `fan == prompts == 0` is
        // legitimate — `forkAll [] :: M [T]` resumes with `[]` — so it passes.)
        if let FanBadge::Exact { n } = fan {
            if n as usize != prompts.len() {
                return Err(HarnessError::Resident(format!(
                    "fanout cardinality mismatch on {node:?}: fan={n} but {} prompt(s) \
                     decoded — a non-Text prompt element was dropped, or the fan/prompts \
                     wire fields disagree",
                    prompts.len()
                )));
            }
        }

        let mut children = Vec::with_capacity(prompts.len());
        let mut answers = Vec::with_capacity(prompts.len());
        for (idx, prompt) in prompts.iter().enumerate() {
            let child = self.register_fork_child(
                node,
                &format!("fanout answerer {idx}"),
                prompt,
                element_ty,
            )?;
            // Guard every child exit: a force/drive fault must not orphan this
            // child (earlier children are already Done+dropped; later ones are
            // never created — only the in-flight one can leak).
            if let Err(e) = self.force(child, actor) {
                self.cleanup_failed_child(child);
                return Err(e);
            }
            let value = match self
                .drive_answerer_to_value(
                    child,
                    node,
                    element_ty,
                    self.cfg.max_child_turns,
                    &self.child_cfg,
                )
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    self.cleanup_failed_child(child);
                    return Err(e);
                }
            };
            let _ = self.tree.node_done(child, "answer delivered".to_string());
            let _ = self.terminate_node(child, "answer delivered");
            children.push(child);
            answers.push(value);
        }

        let table = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.suspend_table.clone())
            .unwrap_or_default();
        let list_value = engine::build_list_value(answers, &table)?;
        self.resume_parent(node, &pending.hole, list_value).await?;
        Ok(children)
    }

    /// Answer an in-context `runLLMTurn` hole: the SAME node's model writes
    /// `resume expr`, which runs via `run_child` against the (suspended) node's
    /// own session to produce the Value, then resumes it. No child node.
    pub async fn answer_run_llm_turn(&self, node: NodeId) -> Result<(), HarnessError> {
        // `node` answers its OWN hole here (`answerer == target == node` in
        // `drive_answerer_to_value` below) — one lease covers the whole
        // multi-round answer, exactly as `drive_turn`'s covers one round;
        // `drive_answerer_to_value` never acquires on its own, so this is the
        // one and only acquire in this call chain.
        let _lease = self.acquire_turn_lease(node)?;
        let pending = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.clone())
            .ok_or(HarnessError::NotSuspended(node))?;
        let ty = match &pending.classified.routing {
            HoleRouting::RunLLMTurn { ty, .. } => ty.clone(),
            other => {
                return Err(HarnessError::RoutingMismatch {
                    node,
                    routing: "run_llm_turn",
                    actual: format!("{other:?}"),
                })
            }
        };
        // Push the hole card as a user turn, then drive the node's own loop to an
        // answering value against itself. `suspend_table` is the table THIS
        // hole was classified from (set when the node suspended) — exactly
        // the table `ty` was resolved against.
        let table = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.suspend_table.clone());
        self.push_user_turn(
            node,
            &engine::hole_card(&pending.classified.prompt, ty.as_deref(), table.as_ref()),
        )?;
        let value = self
            .drive_answerer_to_value(node, node, ty.as_deref(), self.cfg.max_turns, &self.cfg)
            .await?;
        self.resume_parent(node, &pending.hole, value).await?;
        Ok(())
    }

    /// Answer an operator hole — `askUser` ([`HoleRouting::AskUser`]) or a
    /// plain `ask` ([`HoleRouting::Ask`]) — with the operator's submission.
    /// Both effects return the submitted value DIRECTLY, so the submission
    /// JSON always becomes the resume `Value` with zero model turns; the
    /// program that suspended decides what it means. Typed structure is the
    /// caller's job, via `Tidepool.Form`'s `askUser @T` (which derives its
    /// form from `T`'s own metadata and decodes the reply with `T`'s own
    /// `FromJSON`), never a harness-side interpretation step.
    pub async fn answer_dialog(&self, node: NodeId, submission: Json) -> Result<(), HarnessError> {
        // `resume_parent` below is this method's whole job — one lease for
        // the call.
        let _lease = self.acquire_turn_lease(node)?;
        let pending = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.pending.clone())
            .ok_or(HarnessError::NotSuspended(node))?;
        match &pending.classified.routing {
            HoleRouting::Ask { .. } | HoleRouting::AskUser { .. } => {}
            other => {
                return Err(HarnessError::RoutingMismatch {
                    node,
                    routing: "dialog",
                    actual: format!("{other:?}"),
                })
            }
        }

        // The suspend table is the constructor set the hole suspended with; the
        // submission Value bridges against it. Both `askUser` and `ask` return
        // a Value, so the submission JSON IS the resume answer — always, no
        // interpretation.
        let table = self
            .convos
            .lock()
            .get(&node)
            .and_then(|c| c.suspend_table.clone())
            .unwrap_or_default();
        let value = engine::json_answer_to_value(&submission, &table)?;
        // `resume_parent` logs the Consumed attempt itself, exactly once,
        // only after the resume actually succeeds — the single source of
        // truth for the Consumed record; this call site must not log again.
        self.resume_parent(node, &pending.hole, value).await?;
        Ok(())
    }

    /// Drive `answerer`'s turn loop until it emits an answering block, then run
    /// that block via `run_child` against `target`'s suspended session to
    /// produce a Value. On a compile failure (the GHC-verbatim retry), feed the
    /// error back as the answerer's next user turn and loop (bounded by
    /// `max_turns` — a single answerer's cap for a plain fork/return-control
    /// answer, or one fanout child's per-child cap, `cfg.max_child_turns`, so
    /// no single child of a fan can consume the whole node's turn budget).
    /// `ty` is threaded into the answerer's `resume :: ty -> M ty` helper.
    ///
    /// CAP EXHAUSTION never returns straight out: a hard-failure here would
    /// leak a `Running` answerer and wedge the parent, so it instead runs
    /// the escalation ladder via
    /// [`Self::handle_cap_exhaustion`]: an auto corrective-retry first
    /// (rung 1), then an operator popup (rung 2). Only an operator ABORT (or
    /// an unrelated resident/routing error) unwinds out of this loop; on
    /// abort, `answerer` is cancelled here (never left dangling) and
    /// [`HarnessError::Aborted`] is returned, naming `answerer` and why.
    /// `compile_cfg` is the row the answerer's block compiles against: the
    /// parent's own [`Self::cfg`] for an in-context answer (same node answers
    /// itself), or the fork-child [`Self::child_cfg`] for a forked child — so a
    /// child cannot name `fork`/`forkAll`. The answer VALUE always crosses via
    /// `run_child` against `target`'s session regardless (a pure `resume expr`).
    async fn drive_answerer_to_value(
        &self,
        answerer: NodeId,
        target: NodeId,
        ty: Option<&str>,
        max_turns: u32,
        compile_cfg: &EngineConfig,
    ) -> Result<Value, HarnessError> {
        let mut attempts = 0;
        let mut turn_budget = max_turns;
        let mut auto_retries_used = 0u32;
        loop {
            if attempts >= turn_budget {
                match self
                    .handle_cap_exhaustion(answerer, ty, attempts, &mut auto_retries_used)
                    .await?
                {
                    CapDecision::Retry {
                        turn_budget: new_budget,
                    } => {
                        turn_budget = new_budget;
                        continue;
                    }
                    CapDecision::Abort { reason } => {
                        self.terminate_node(answerer, &reason)?;
                        return Err(HarnessError::Aborted {
                            node: answerer,
                            reason,
                        });
                    }
                }
            }
            attempts += 1;

            // One provider turn on the answerer.
            let (transcript, turn_seq, framing) = {
                let convos = self.convos.lock();
                let convo = convos
                    .get(&answerer)
                    .ok_or(HarnessError::NoSession(answerer))?;
                (
                    convo.transcript.clone(),
                    convo.turn_seq,
                    convo.framing.clone(),
                )
            };
            let driven = self.stream_turn(&transcript, framing.as_deref()).await?;
            self.tree.turn_delta_reasoned(
                answerer,
                turn_seq,
                Role::Assistant,
                driven.reply.clone(),
                Some(driven.usage),
                driven.reasoning.clone(),
            )?;
            {
                let mut convos = self.convos.lock();
                let convo = convos
                    .get_mut(&answerer)
                    .ok_or(HarnessError::NoSession(answerer))?;
                convo.transcript.push(Message {
                    role: Role::Assistant,
                    content: driven.reply.clone(),
                    reasoning_items: driven.reasoning_items.clone(),
                });
                convo.turn_seq += 1;
            }

            let Some(block) = driven.block else {
                self.push_user_turn(
                    answerer,
                    "Reply with a single ```haskell block: `resume expr` where the value \
                     matches the hole type.",
                )?;
                continue;
            };

            // Compile the answering block. When the hole's answer type is
            // known, specialize `resume :: T -> M T` so a mismatched `resume
            // expr` fails at extract with a GHC error naming T (the golden
            // path's deliberate ill-typed attempt lands here) — otherwise fall
            // back to the polymorphic identity.
            let helpers = match ty {
                Some(t) => format!("resume :: {t} -> M {t}\nresume = pure"),
                None => RESUME_HELPER.to_string(),
            };
            let (imports, body) = engine::split_imports(&block);
            let src = engine::template_answer_turn(compile_cfg, &body, &imports, &helpers);
            let cfg_bin = compile_cfg.extract_bin.clone();
            let include = compile_cfg.include.clone();
            let answerer_id = answerer.0;
            let compiled = tokio::task::spawn_blocking(move || {
                compile::compile_turn(
                    &cfg_bin,
                    &src,
                    "result",
                    &include,
                    answerer_id,
                    timing::NO_ROUND,
                )
            })
            .await
            .map_err(|e| HarnessError::Resident(format!("compile join: {e}")))?;

            let compiled = match compiled {
                Ok(c) => c,
                Err(e) => {
                    // GHC-verbatim retry: feed the compile error back to the
                    // answerer. The continuation is NEVER consumed by a bad
                    // attempt.
                    let err = e.to_string();
                    self.log_answer_attempt(
                        target,
                        &self
                            .convos
                            .lock()
                            .get(&target)
                            .and_then(|c| c.pending.as_ref().map(|p| p.hole.clone()))
                            .unwrap_or(HoleId(String::new())),
                        "child",
                        AnswerOutcome::Rejected { error: err.clone() },
                    )?;
                    self.push_user_turn(
                        answerer,
                        &format!(
                            "That did not compile. Fix it and try again — the error is:\n\n\
                             ```\n{err}\n```"
                        ),
                    )?;
                    continue;
                }
            };

            // Run the answering block via run_child against the TARGET's
            // suspended session (same heap → the Value can feed resume).
            let checkout = self.checkout_child(target)?;
            let expr = compiled.expr;
            let ctable = compiled.table.clone();
            let child_out = self
                .run_checked_out(target, checkout, move |mut session| {
                    let out = session.run_child(
                        "answerer",
                        &expr,
                        &ctable,
                        &tidepool_codegen::emit::ExternalEnv::new(),
                    );
                    (session, out)
                })
                .await?;
            self.flush_effects(target)?;

            match child_out {
                Ok(result) => {
                    if std::env::var("HARNESS_DEBUG").is_ok() {
                        eprintln!("[harness] child answer value: {:?}", result.value());
                    }
                    return Ok(result.into_value());
                }
                Err(ResidentError::NotSuspended) => return Err(HarnessError::NotSuspended(target)),
                // v1 limitation: a forked CHILD that itself suspends (nested
                // `fork`/`forkAll`, or an `askUser`/`ask` hole) is unsupported.
                // The GUI/self-harness driver monitors ONE session (the
                // parent's), so there is no operator to answer a hole opened
                // two levels deep — and `run_child` itself only ever holds ONE
                // stowed continuation (R0 sequential-isolated), so the child
                // literally cannot park here. Cancel the child and hard-error
                // rather than falling into the generic retry arm below (which
                // would blind-retry forever: the child's own suspend is not a
                // transient compile/runtime fault it can self-correct from).
                Err(ResidentError::ChildSuspended) => {
                    self.terminate_node(
                        answerer,
                        "nested fork/askUser in a fork child unsupported (v1)",
                    )?;
                    return Err(HarnessError::Aborted {
                        node: answerer,
                        reason: "a forked child answerer suspended on its own effect — nested \
                                 fork or askUser inside a fork child is unsupported in v1"
                            .to_string(),
                    });
                }
                Err(e) => {
                    // A run-time fault in the answerer (e.g. `error "boom"`
                    // forced during its own eval, before the parent's
                    // continuation is ever touched) — logged as a Rejected
                    // attempt, same as the compile-failure branch above, so
                    // the durable audit trail shows every attempt, not just
                    // the one that eventually consumes. The continuation is
                    // NEVER consumed by this attempt; retry with the message.
                    let err = e.to_string();
                    self.log_answer_attempt(
                        target,
                        &self
                            .convos
                            .lock()
                            .get(&target)
                            .and_then(|c| c.pending.as_ref().map(|p| p.hole.clone()))
                            .unwrap_or(HoleId(String::new())),
                        "child",
                        AnswerOutcome::Rejected { error: err.clone() },
                    )?;
                    self.push_user_turn(
                        answerer,
                        &format!("The answer failed at runtime: {err}. Try again."),
                    )?;
                    continue;
                }
            }
        }
    }

    /// The escalation ladder's ONE rung-1 auto-retry: a fixed extra turn
    /// budget granted exactly once per [`Self::drive_answerer_to_value`] call
    /// before further exhaustion escalates to rung 2. Kept small and
    /// singular deliberately — this is meant to unwedge the common case (the
    /// model just needed one more nudge), not to substitute for the
    /// operator.
    const AUTO_RETRY_MAX: u32 = 1;
    /// The turn-budget bump rung 1's single auto-retry grants.
    const AUTO_RETRY_BUMP: u32 = 3;

    /// [`Self::drive_answerer_to_value`]'s ladder step, called when
    /// `answerer` has exhausted its current turn budget (`attempts >=
    /// turn_budget`) without producing a consumed answer — whether from
    /// repeated `NoBlock` replies, repeated ill-typed `resume` attempts, or a
    /// mix (both retry paths consume from the same `attempts` counter, so
    /// either exhausts the same way). RUNG 1 fires at most once per answerer
    /// per call (`auto_retries_used` is the caller's counter, threaded
    /// through so a SECOND exhaustion after an operator-granted budget goes
    /// straight back to rung 2 rather than re-trying rung 1): it injects a
    /// corrective user turn — reusing the same feed-the-error-back-verbatim
    /// idiom the compile-failure retry above uses, just with a different
    /// message — and grants [`Self::AUTO_RETRY_BUMP`] more turns. Once rung 1
    /// is spent, this escalates to [`Self::escalate_to_operator`] (rung 2).
    async fn handle_cap_exhaustion(
        &self,
        answerer: NodeId,
        ty: Option<&str>,
        attempts: u32,
        auto_retries_used: &mut u32,
    ) -> Result<CapDecision, HarnessError> {
        if *auto_retries_used < Self::AUTO_RETRY_MAX {
            *auto_retries_used += 1;
            let ty_clause = ty.map(|t| format!(" of type `{t}`")).unwrap_or_default();
            self.push_user_turn(
                answerer,
                &format!(
                    "You have exhausted your turn budget ({attempts} turns) without \
                     producing a single valid `resume expr`{ty_clause}. You have \
                     {bump} more turns — produce a single valid `resume expr` now.",
                    bump = Self::AUTO_RETRY_BUMP
                ),
            )?;
            return Ok(CapDecision::Retry {
                turn_budget: attempts + Self::AUTO_RETRY_BUMP,
            });
        }
        self.escalate_to_operator(answerer, attempts).await
    }

    /// Rung 2 of the escalation ladder: park `answerer` awaiting an operator
    /// decision. Publishes an [`Escalation`] (what the stuck-node popup
    /// renders) and a oneshot sender (fired by [`Self::resolve_escalation`],
    /// driven by the web `/steer/:node` endpoint or a test simulating the
    /// popup), THEN drops every lock before awaiting the receiver — the
    /// partial fan state a caller further up the stack (e.g.
    /// [`Self::answer_fanout`]'s `answers` vector) is holding lives on the
    /// ASYNC STACK across this await, which is fine and intended: this is an
    /// IN-PROCESS control-plane wait, not durable-across-restart mid-fan
    /// suspension (explicitly out of R0 scope) — a process death here loses
    /// the in-flight fan and the operator re-triggers, same as any other
    /// in-flight turn.
    async fn escalate_to_operator(
        &self,
        answerer: NodeId,
        attempts: u32,
    ) -> Result<CapDecision, HarnessError> {
        let (tx, rx) = oneshot::channel();
        let escalation = Escalation {
            reason: format!("cap-exhausted after {attempts} attempts"),
            transcript_preview: self.transcript_tail(answerer, 6),
        };
        self.escalations.lock().insert(answerer, (escalation, tx));

        // On success, `resolve_escalation` already removed this entry (it
        // takes the sender by removing the whole pair) — nothing left to
        // clean up here. On error (the sender dropped without a decision —
        // e.g. a second escalation on the same node overwrote this entry
        // before it resolved), `resolve_escalation` never ran, so the entry
        // needs cleaning up here instead.
        let decision = rx.await.map_err(|_| {
            self.escalations.lock().remove(&answerer);
            HarnessError::Resident(format!(
                "node {answerer:?}: operator escalation channel dropped without a decision"
            ))
        })?;

        match decision {
            OperatorDecision::AllocateMore { turns, steer } => {
                if let Some(steer) = steer.filter(|s| !s.trim().is_empty()) {
                    self.push_user_turn(answerer, &steer)?;
                }
                Ok(CapDecision::Retry {
                    turn_budget: attempts + turns,
                })
            }
            OperatorDecision::Abort => Ok(CapDecision::Abort {
                reason: format!("cap-exhausted after {attempts} attempts; operator aborted"),
            }),
        }
    }

    /// The last `n` transcript messages on `node`, rendered as a plain-text
    /// preview for the escalation popup. Model-authored content — the caller
    /// renders it through the same HTML-neutralizing path any other
    /// model/operator-visible text uses; this returns bare text, no markup.
    fn transcript_tail(&self, node: NodeId, n: usize) -> String {
        let convos = self.convos.lock();
        let Some(convo) = convos.get(&node) else {
            return String::new();
        };
        let start = convo.transcript.len().saturating_sub(n);
        convo.transcript[start..]
            .iter()
            .map(|m| format!("{:?}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Resume `node`'s parked continuation with `answer` (a Value in the node's
    /// heap). Runs the resume off-reactor; on completion marks the node done, on
    /// a re-suspend re-publishes the new hole.
    async fn resume_parent(
        &self,
        node: NodeId,
        hole: &HoleId,
        answer: Value,
    ) -> Result<(), HarnessError> {
        // `Session::resume` continues the ALREADY-COMPILED fragment the node
        // suspended with (it does not recompile), so any hole reached further
        // down that same continuation — including a second sequential ask —
        // resolves its site-id -> type against this SAME table, exactly like
        // `run_block` resolves the first hole's. Snapshot it now (session is
        // about to be taken out) so the re-suspend arm below can classify the
        // next hole with real site/type instead of publishing `None`/`None`.
        let (table, asks, pending_bind) = {
            let convos = self.convos.lock();
            let c = convos.get(&node);
            (
                c.and_then(|c| c.suspend_table.clone()).unwrap_or_default(),
                c.map(|c| c.suspend_asks.clone()).unwrap_or_default(),
                c.and_then(|c| c.pending_bind.clone()),
            )
        };

        let checkout = self.checkout_resume(node, hole)?;
        let hole_str = hole.0.clone();
        // A suspended value-plane bind resumes via `resume_bind` (which
        // materializes the binding on completion); a plain hole via `resume`.
        let outcome = self
            .run_checked_out(node, checkout, move |mut session| {
                let out = match &pending_bind {
                    Some((binder, gen)) => session.resume_bind(&hole_str, answer, binder, *gen),
                    None => session.resume(&hole_str, answer),
                };
                (session, out)
            })
            .await?;
        // Refresh suspend_table/suspend_asks explicitly on every restore, not
        // just the first suspend — downstream lookups (hole-card synopsis,
        // dialog answers) must read the table the resident session is
        // actually compiled against, not silently-preserved first-suspend
        // state.
        {
            let mut convos = self.convos.lock();
            if let Some(convo) = convos.get_mut(&node) {
                convo.suspend_table = Some(table.clone());
                convo.suspend_asks = asks.clone();
            }
        }
        self.flush_effects(node)?;

        // Only log the hole as Consumed (and clear it from `pending`) once the
        // resume has ACTUALLY SUCCEEDED — a fault here (e.g. `error` forced
        // mid-resume) must not leave a durable Consumed record with no
        // matching NodeDone or re-HolePublished.
        let outcome = outcome.map_err(|e| HarnessError::Resident(e.to_string()))?;

        self.log_answer_attempt(node, hole, "harness", AnswerOutcome::Consumed)?;
        self.tree.hole_consumed(node, hole.clone())?;
        {
            let mut convos = self.convos.lock();
            if let Some(convo) = convos.get_mut(&node) {
                convo.pending = None;
            }
        }

        match outcome {
            ResidentOutcome::Completed { result, .. } => {
                let rendered = result.to_string_pretty();
                self.tree.node_done(node, rendered)?;
                // A suspended value-plane bind materialized on this completion
                // (via `resume_bind`) — clear the stashed binder.
                {
                    let mut convos = self.convos.lock();
                    if let Some(convo) = convos.get_mut(&node) {
                        convo.pending_bind = None;
                    }
                }
                // Keep the session ALIVE past completion (don't `terminate_node`),
                // same as `run_block`: a node that suspended on a hole and then
                // resumed to Done is still a followable conversation — its
                // persisted session lets `follow_up` reopen it with heap intact.
                Ok(())
            }
            ResidentOutcome::Suspended { hole, request, .. } => {
                // The resumed turn hit ANOTHER hole. Re-classify against the
                // table snapshotted above (the compile the still-executing
                // fragment was built with) and re-publish with its REAL
                // site + type, same as a first-suspend `run_block` hole.
                let classified = engine::classify_hole(&request, &table, &asks);
                let fork = matches!(classified.routing, HoleRouting::Fork { .. });
                let ty = match &classified.routing {
                    HoleRouting::Fork { ty, .. }
                    | HoleRouting::RunLLMTurn { ty, .. }
                    | HoleRouting::Finalize { ty, .. } => ty.clone(),
                    _ => None,
                };
                let site = match &classified.routing {
                    HoleRouting::Fork { site, .. }
                    | HoleRouting::RunLLMTurn { site, .. }
                    | HoleRouting::Finalize { site, .. } => Some(crate::tree::SiteId(*site)),
                    _ => None,
                };
                self.tree.hole_published(
                    node,
                    HoleId(hole.clone()),
                    site,
                    ty,
                    classified.prompt.clone(),
                    fork,
                )?;
                self.set_pending(
                    node,
                    PendingHole {
                        hole: HoleId(hole),
                        classified,
                        raw_request: request,
                    },
                );
                Ok(())
            }
        }
    }

    /// Register a fork/fanout child under `parent`, inheriting the parent's
    /// transcript prefix through the fork checkpoint plus the hole card, and
    /// the parent's framing (its system message). Emits `TurnForked`
    /// referencing the checkpoint. The child is a THUNK — the caller forces it.
    /// `title` distinguishes a plain fork's single child ("fork answerer") from
    /// one of a fanout's N children ("fanout answerer <i>").
    ///
    /// The checkpoint is the parent transcript's PREFIX LENGTH at fork time (a
    /// durable transcript position), not the assistant-only `turn_seq` counter
    /// — the child clones exactly that prefix, so the two agree by
    /// construction.
    fn register_fork_child(
        &self,
        parent: NodeId,
        title: &str,
        prompt: &str,
        ty: Option<&str>,
    ) -> Result<NodeId, HarnessError> {
        // `parent`'s `suspend_table` is the table its CURRENT hole (the one
        // this fork answers) was classified from — exactly the table `ty` was
        // resolved against.
        let (parent_transcript, parent_framing, table) = {
            let convos = self.convos.lock();
            let convo = convos.get(&parent).ok_or(HarnessError::NoSession(parent))?;
            (
                convo.transcript.clone(),
                convo.framing.clone(),
                convo.suspend_table.clone(),
            )
        };
        let checkpoint = parent_transcript.len() as u64;
        let child = self.tree.create_node(
            Some(parent),
            title,
            self.cfg.effect_names.clone(),
            ForkShape::Exact(0),
            false,
        )?;
        self.tree.turn_forked(child, parent, checkpoint)?;

        // The child's transcript = parent prefix + the hole card as a fresh user
        // task. The fork IS the calling agent (inherits scope + framing), so the
        // parent conversation is genuine context.
        let mut transcript = parent_transcript;
        transcript.push(Message {
            role: Role::User,
            content: engine::hole_card(prompt, ty, table.as_ref()),
            reasoning_items: Vec::new(),
        });
        self.pending.lock().insert(
            child,
            NodeSeed::Forked {
                transcript,
                framing: parent_framing,
            },
        );
        Ok(child)
    }

    fn log_answer_attempt(
        &self,
        node: NodeId,
        hole: &HoleId,
        source: &str,
        outcome: AnswerOutcome,
    ) -> Result<(), HarnessError> {
        self.tree
            .hole_answer_attempt(node, hole.clone(), source.to_string(), outcome)?;
        Ok(())
    }

    /// Log what extract said the just-compiled turn's holes and binds ARE —
    /// the `asks.json` site → type table plus a value-plane bind's bound
    /// name/type, if either is non-empty — to console INFO and `log.jsonl`
    /// (dogfood-observability deliverable 2). A no-op (no console line, no
    /// event) when the turn has neither: most turns don't.
    fn log_turn_extracted(
        &self,
        node: NodeId,
        asks: &[(u32, String)],
        bound: Option<(&str, &str)>,
    ) -> Result<(), HarnessError> {
        if asks.is_empty() && bound.is_none() {
            return Ok(());
        }
        tracing::info!(
            node = node.0,
            asks = ?asks,
            bound = ?bound,
            "turn extracted types"
        );
        self.tree.turn_extracted(
            node,
            asks.to_vec(),
            bound.map(|(name, ty)| (name.to_string(), ty.to_string())),
        )?;
        Ok(())
    }

    /// Cancel a node (operator stop / teardown) — retires it via
    /// [`Self::terminate_node`].
    pub fn cancel(&self, node: NodeId, reason: &str) -> Result<(), HarnessError> {
        self.terminate_node(node, reason)
    }

    /// The `turn_spliced` verb: interject `content` into `node`'s
    /// OWN transcript, landing at `node`'s CURRENT turn position. Appends
    /// straight to the LIVE `NodeConvo::transcript` (the same list
    /// `drive_turn`/`push_user_turn` read/append), so the very next prompt
    /// assembly on `node` sees it — no separate replay-only path, the fold
    /// crash-replay uses (`replay::apply_event`) is the SAME shape a fresh
    /// process would reconstruct from the log. Logged as `Event::TurnSpliced`
    /// (not `TurnDelta`) so a genuine operator interjection is distinguishable
    /// from a harness-generated nudge when auditing history. Requires `node`
    /// to be `Running` or `Suspended` (the same non-terminal, forced-state
    /// guard `turn_delta` uses — a splice needs a live transcript to land in).
    pub fn splice(&self, node: NodeId, content: &str) -> Result<(), HarnessError> {
        let mut convos = self.convos.lock();
        let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
        let turn = convo.turn_seq;
        convo.transcript.push(Message {
            role: Role::User,
            content: content.to_string(),
            reasoning_items: Vec::new(),
        });
        convo.turn_seq += 1;
        drop(convos);
        self.tree
            .turn_spliced(node, turn, Role::User, content.to_string())?;
        Ok(())
    }
}

// -- session take/put/drop + pending accessors -------------------------------

impl Harness {
    /// Read a node's decl-plane context — the current `Lib.G<g>` module to import
    /// and its include directory — WITHOUT checking the session out, so a caller
    /// can build a session-aware compile before taking the session for the run.
    /// `(None, None)` when the node has no session or no accumulated decl plane.
    fn session_decl_context(&self, node: NodeId) -> (Option<String>, Option<PathBuf>) {
        let Some(sid) = self.tree.session_of(node) else {
            return (None, None);
        };
        self.tree
            .registry()
            .peek(sid, |s| (s.session_import_module(), s.lib_include_dir()))
            .unwrap_or((None, None))
    }

    /// Declare what `node` must produce to resolve the hole it is now
    /// answering — see [`AnswerContract`]. The self-iterating harness driver
    /// sets this per hole, before pushing the hole card, because its per-loop
    /// answerer node is REUSED across holes whose types differ. `None` clears
    /// it (back to the polymorphic `finalize`).
    ///
    /// Takes effect from the node's next turn: every turn compiled while it is
    /// set gets the answer type in scope and `finalize` pinned to it.
    pub fn set_answer_contract(&self, node: NodeId, contract: Option<AnswerContract>) {
        let mut convos = self.convos.lock();
        if let Some(convo) = convos.get_mut(&node) {
            convo.answer_contract = contract;
        }
    }

    /// `node`'s current [`AnswerContract`], if one is set.
    fn answer_contract(&self, node: NodeId) -> Option<AnswerContract> {
        let convos = self.convos.lock();
        convos.get(&node).and_then(|c| c.answer_contract.clone())
    }

    /// Check `node`'s machine out for a NEW TOP-LEVEL turn (`Idle ->
    /// Running`). Maps a registry refusal through [`HarnessError::from_checkout`]
    /// (the one place a `CheckoutError` becomes a node-scoped `HarnessError`).
    fn checkout_run(&self, node: NodeId) -> Result<Checkout<'_, Session>, HarnessError> {
        let sid = self
            .tree
            .session_of(node)
            .ok_or(HarnessError::NoSession(node))?;
        self.tree
            .registry()
            .checkout_run(sid)
            .map_err(|e| HarnessError::from_checkout(node, e))
    }

    /// Check `node`'s machine out to resume/abort its pending `hole`
    /// (`Suspended{hole} -> Running`), validating the hole matches.
    fn checkout_resume(
        &self,
        node: NodeId,
        hole: &HoleId,
    ) -> Result<Checkout<'_, Session>, HarnessError> {
        let sid = self
            .tree
            .session_of(node)
            .ok_or(HarnessError::NoSession(node))?;
        self.tree
            .registry()
            .checkout_resume(sid, hole)
            .map_err(|e| HarnessError::from_checkout(node, e))
    }

    /// Check `node`'s machine out for a NESTED CHILD run against its
    /// suspended continuation (`Suspended{hole} -> RunningChild{hole}`) — the
    /// `run_child` discipline: an answer value crosses via a child run
    /// against the suspended TARGET's own session, never consuming its
    /// continuation.
    fn checkout_child(&self, node: NodeId) -> Result<Checkout<'_, Session>, HarnessError> {
        let sid = self
            .tree
            .session_of(node)
            .ok_or(HarnessError::NoSession(node))?;
        self.tree
            .registry()
            .checkout_child(sid)
            .map_err(|e| HarnessError::from_checkout(node, e))
    }

    /// Run `f` against `checkout`'s machine on the blocking pool, then
    /// restore it based on the machine's OWN post-call state
    /// (`is_idle`/`pending_continuation`) rather than guessing from `f`'s
    /// domain result — correct whether the resident call completed,
    /// suspended, or errored (an errored `run`/`resume` still leaves the
    /// session in a well-defined idle/suspended state; `run_child` never
    /// changes the target's pending hole either way).
    ///
    /// On a `JoinError` (the blocking task panicked — the machine went with
    /// it), the checkout has nothing left to restore: retire the node via
    /// [`Self::terminate_node`] instead of leaving the registry slot wedged
    /// `Running` forever.
    async fn run_checked_out<'a, F, T>(
        &'a self,
        node: NodeId,
        mut checkout: Checkout<'a, Session>,
        f: F,
    ) -> Result<T, HarnessError>
    where
        F: FnOnce(Session) -> (Session, T) + Send + 'static,
        T: Send + 'static,
    {
        let machine = checkout.take();
        match tokio::task::spawn_blocking(move || f(machine)).await {
            Ok((session, result)) => {
                let hole = session
                    .pending_continuation()
                    .map(|h| HoleId(h.to_string()));
                checkout.put(session);
                match hole {
                    Some(h) => checkout.restore_suspended(h),
                    None => checkout.restore_idle(),
                }
                Ok(result)
            }
            Err(join_err) => {
                let _ = self
                    .terminate_node(node, &format!("resident session task panicked: {join_err}"));
                Err(HarnessError::Resident(format!(
                    "session task panicked: {join_err}"
                )))
            }
        }
    }

    /// The ONE retirement path: terminalize the tree entry (a node already
    /// `Done`/`Cancelled` is left as-is — idempotent), remove its session
    /// from the registry (dropping the machine), and remove its `convos`
    /// entry. `Ok(())` even for an already-terminated or unknown node —
    /// every caller (cancellation, a failed fork/fanout child, a panicked-
    /// turn `JoinError`, the self-iterating harness's `retire_answerer`)
    /// wants "this node is retired" as its postcondition, not "this node was
    /// still live when I asked".
    pub fn terminate_node(&self, node: NodeId, reason: &str) -> Result<(), HarnessError> {
        if let Some(state) = self.tree.state(node) {
            if !matches!(
                state,
                crate::tree::NodeState::Done | crate::tree::NodeState::Cancelled { .. }
            ) {
                self.tree.node_cancelled(node, reason.to_string())?;
            }
        }
        if let Some(sid) = self.tree.session_of(node) {
            self.tree.registry().remove(sid);
        }
        self.convos.lock().remove(&node);
        Ok(())
    }

    fn set_pending(&self, node: NodeId, pending: PendingHole) {
        let mut convos = self.convos.lock();
        if let Some(convo) = convos.get_mut(&node) {
            convo.pending = Some(pending);
        }
    }

    /// Replace `node`'s transcript with a single summary message IN PLACE,
    /// keeping the resident session, per-node framing (`render`'s output), and
    /// turn-sequence continuity live — the self-iterating harness's MID-LOOP
    /// in-place compaction relief: replace the context with the summary so
    /// the loop CONTINUES, never a loop-abort. The
    /// accumulated exchange is collapsed to one User-role message carrying
    /// `summary` as prior-window context; the node's running [`Usage`] is reset
    /// (`node_usage` now reflects only the small compacted window, so the
    /// driver's threshold check does not immediately re-fire). The next hole
    /// (or the current hole's next round) drives on under the smaller context.
    pub(crate) fn replace_transcript_with_summary(
        &self,
        node: NodeId,
        summary: &str,
    ) -> Result<(), HarnessError> {
        let mut convos = self.convos.lock();
        let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
        let content = format!(
            "[Prior context compacted to relieve the context window.] Summary of \
             the work you have done in this loop so far:\n\n{summary}\n\nContinue \
             from here; the detailed transcript above has been replaced by this \
             summary."
        );
        let turn = convo.turn_seq;
        convo.transcript = vec![Message {
            role: Role::User,
            content: content.clone(),
            reasoning_items: Vec::new(),
        }];
        convo.turn_seq += 1;
        // Reset the running context-size meter: the live context is now just
        // this summary, so the driver's budget check must see the small
        // compacted window, not the pre-compaction cumulative total. Both the
        // summed `usage` and the high-water `last_input_tokens`
        // reset — the next turn's input_tokens re-establishes the real size.
        convo.usage = Usage::default();
        convo.last_input_tokens = 0;
        drop(convos);
        self.tree
            .turn_delta(node, turn, Role::User, content, None)?;
        Ok(())
    }

    /// Append a User-role message to `node`'s transcript (and log it), without
    /// driving a turn. The self-iterating harness driver pushes each
    /// `runLLMTurn` hole card onto the SAME per-loop answerer node this way, so
    /// hole #2 sees hole #1's exchange (the accumulating context
    /// window). Also the corrective-retry mechanism inside
    /// [`Self::run_to_hole_or_done`].
    pub(crate) fn push_user_turn(&self, node: NodeId, content: &str) -> Result<(), HarnessError> {
        let mut convos = self.convos.lock();
        let convo = convos.get_mut(&node).ok_or(HarnessError::NoSession(node))?;
        let turn = convo.turn_seq;
        convo.transcript.push(Message {
            role: Role::User,
            content: content.to_string(),
            reasoning_items: Vec::new(),
        });
        convo.turn_seq += 1;
        drop(convos);
        tracing::info!(node = node.0, "user turn to model:\n{content}");
        self.tree
            .turn_delta(node, turn, Role::User, content.to_string(), None)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::{Event, LogHeader, LogReader};
    use crate::provider::{ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse};

    /// Never actually called: `flush_effects` touches `self.tree`,
    /// `self.convos`, and `self.cfg.effect_names` only, so a `Harness` built
    /// for this suite needs no live provider.
    struct UnusedProvider;

    impl ModelProvider for UnusedProvider {
        async fn complete(
            &self,
            _req: TurnRequest,
            _sink: Option<StreamSink>,
        ) -> Result<TurnResponse, ProviderError> {
            Err(ProviderError::Api("UnusedProvider was called".into()))
        }
    }

    fn test_engine_cfg() -> EngineConfig {
        EngineConfig::inert(vec!["Console".to_string()])
    }

    // ---- render_compile_error / error coordinates -------------------------
    //
    // `render_compile_error` remaps a `run_turn` compile failure's GHC
    // coordinates from TEMPLATE space to the model's own turn text. These
    // build SYNTHETIC candidate sources (same marker shape the real
    // `expr_turn_template`/`session_bind_template` builders produce, per
    // `EXPR_MARKER`/`BIND_MARKER`) and a synthetic diagnostic, so the offset
    // arithmetic is pinned without a real GHC compile.

    fn diag(file: &str, line: u32, col: u32, message: &str) -> tidepool_runtime::diag::ExtractDiag {
        tidepool_runtime::diag::ExtractDiag {
            span: Some(tidepool_runtime::diag::DiagSpan {
                file: file.to_string(),
                start_line: line,
                start_col: col,
                end_line: line,
                end_col: col + 1,
            }),
            severity: "error".to_string(),
            message: message.to_string(),
        }
    }

    /// A synthetic EXPR-template source: `preamble_lines` filler lines, the
    /// real `EXPR_MARKER`, then one line per `content_lines` standing in for
    /// the model's own turn text.
    fn fake_expr_source(preamble_lines: usize, content_lines: usize) -> String {
        let mut s = "-- preamble\n".repeat(preamble_lines);
        s.push_str(EXPR_MARKER);
        for i in 0..content_lines {
            s.push_str(&format!("userExprLine{i}\n"));
        }
        s.push_str(" } in __b\n");
        s
    }

    /// A synthetic BIND-template source: same shape, `BIND_MARKER` instead.
    fn fake_bind_source(preamble_lines: usize, content_lines: usize) -> String {
        let mut s = "-- preamble\n".repeat(preamble_lines);
        s.push_str(BIND_MARKER);
        for i in 0..content_lines {
            s.push_str(&format!("userStmtLine{i}\n"));
        }
        s.push_str(" ; pure x\n }\n");
        s
    }

    /// The assertion that matters most, mutation-closed: a turn whose user
    /// code fails on its FIRST line reports line 1, not the preamble-offset
    /// raw line. Break `candidate_window`'s `offset + 1` (e.g. to plain
    /// `offset`) and this goes red.
    #[test]
    fn render_compile_error_remaps_first_line_of_user_code() {
        let expr_source = fake_expr_source(0, 1);
        let bind_source = fake_bind_source(0, 1);
        // EXPR_MARKER has 2 newlines, so the first user line lands on raw
        // line 3.
        let err = tidepool_runtime::CompileError::Diagnostics(vec![diag(
            "Expr.hs",
            3,
            1,
            "Variable not in scope: garbage",
        )]);
        let out = render_compile_error(&err, "garbage", &expr_source, &bind_source);
        assert!(out.contains("<turn>:1:"), "{out}");
        assert!(
            !out.contains("Expr.hs:3"),
            "raw template line leaked: {out}"
        );
    }

    /// A multi-line user turn failing on its Nth line reports N.
    #[test]
    fn render_compile_error_remaps_nth_line_of_user_code() {
        let expr_source = fake_expr_source(0, 3);
        let bind_source = fake_bind_source(0, 3);
        // Raw line 5 = offset(2) + 3rd content line.
        let err = tidepool_runtime::CompileError::Diagnostics(vec![diag(
            "Expr.hs",
            5,
            1,
            "type error on the third line",
        )]);
        let block = "userExprLine0\nuserExprLine1\nuserExprLine2";
        let out = render_compile_error(&err, block, &expr_source, &bind_source);
        assert!(out.contains("<turn>:3:"), "{out}");
    }

    /// The verdict-ambiguity case: when the diagnostic's raw line falls
    /// outside the EXPR candidate's window but inside the BIND candidate's,
    /// the BIND candidate is used instead — never the (wrong) EXPR offset.
    #[test]
    fn render_compile_error_falls_back_to_bind_candidate_when_expr_window_misses() {
        // EXPR: 0 preamble lines, 1-line window at raw line 3.
        let expr_source = fake_expr_source(0, 1);
        // BIND: 10 preamble lines pushes its window well past EXPR's.
        let bind_source = fake_bind_source(10, 1);
        let bind_offset = candidate_window(&bind_source, BIND_MARKER, 1).unwrap().0;
        let raw_line = (bind_offset + 1) as u32;
        let err =
            tidepool_runtime::CompileError::Diagnostics(vec![diag("Expr.hs", raw_line, 1, "oops")]);
        let out = render_compile_error(&err, "x", &expr_source, &bind_source);
        assert!(out.contains("<turn>:1:"), "{out}");
    }

    /// A diagnostic whose raw line falls in NEITHER candidate's window keeps
    /// its raw template-space span — remapping against the wrong candidate
    /// would silently shift the line by a wrong constant, which is worse
    /// than not remapping at all.
    #[test]
    fn render_compile_error_leaves_out_of_window_diagnostic_raw() {
        let expr_source = fake_expr_source(0, 1);
        let bind_source = fake_bind_source(0, 1);
        let err = tidepool_runtime::CompileError::Diagnostics(vec![diag(
            "Expr.hs",
            9999,
            1,
            "deep in generated scaffolding",
        )]);
        let out = render_compile_error(&err, "x", &expr_source, &bind_source);
        assert!(out.contains("Expr.hs:9999:1"), "{out}");
        assert!(!out.contains("<turn>"), "{out}");
    }

    /// A non-`Diagnostics` variant carries no GHC coordinates to remap —
    /// renders verbatim via `Display`, exactly as before this fix.
    #[test]
    fn render_compile_error_non_diagnostics_variant_renders_verbatim() {
        let err = tidepool_runtime::CompileError::IOTypeDetected;
        let out = render_compile_error(&err, "x", "", "");
        assert_eq!(out, err.to_string());
    }

    /// A template-internal binder (`__b`) whose OWN span is in the
    /// scaffold-preamble region must not leak into a remapped diagnostic —
    /// exercising `tidepool_runtime::diag`'s scrubbing through the harness's
    /// own picked `RenderOpts`, not a second implementation of it.
    #[test]
    fn render_compile_error_scrubs_template_internal_binder() {
        let expr_source = fake_expr_source(0, 1);
        let bind_source = fake_bind_source(0, 1);
        let message = "* Ambiguous type variable `f0'\n\
             Relevant bindings include\n  \
             __b :: f0 (Value, b0) (bound at Expr.hs:1:2)\n  \
             (Some bindings suppressed; use -fmax-relevant-binds=N or -fno-max-relevant-binds)";
        let err = tidepool_runtime::CompileError::Diagnostics(vec![diag("Expr.hs", 3, 1, message)]);
        let out = render_compile_error(&err, "garbage", &expr_source, &bind_source);
        assert!(!out.contains("__b"), "{out}");
        assert!(out.contains("Ambiguous type variable"), "{out}");
    }

    /// A `Harness` built without `Harness::new`/`Harness::force` (both need a
    /// real `tidepool-extract` compile) — every field is filled directly with
    /// an inert placeholder, since `flush_effects` never reads `provider` or
    /// the seed/escalation maps. This keeps `flush_effects`' unit coverage in
    /// the pure-Rust fast tier.
    fn test_harness() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let writer = LogWriter::create(
            dir.path().join("test.jsonl"),
            &LogHeader {
                prelude_hash: "test".into(),
                extract_fingerprint: "test".into(),
                harness_version: "test".into(),
            },
        )
        .unwrap();
        let provider: Arc<dyn DynModelProvider> = Arc::new(UnusedProvider);
        Harness {
            run_id: generate_run_id(),
            tree: NodeTree::new(writer),
            cfg: test_engine_cfg(),
            child_cfg: test_engine_cfg(),
            provider,
            convos: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            escalations: Mutex::new(HashMap::new()),
        }
    }

    /// Like [`test_harness`] but keeps the log's tempdir alive and returns its
    /// path, for a test that reads the durable log back after driving the
    /// harness — `test_harness`'s own tempdir is dropped (and the file
    /// unlinked) before it returns. `Harness::force` needs no GHC/extract
    /// compile at all now (it registers an [`ResidentSession::unbootstrapped`]
    /// session — the machine comes up on the node's first real turn), so this
    /// fixture no longer needs to fabricate a boot expr either.
    fn test_harness_with_log() -> (Harness, std::path::PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let writer = LogWriter::create(
            &path,
            &LogHeader {
                prelude_hash: "test".into(),
                extract_fingerprint: "test".into(),
                harness_version: "test".into(),
            },
        )
        .unwrap();
        let provider: Arc<dyn DynModelProvider> = Arc::new(UnusedProvider);
        let harness = Harness {
            run_id: generate_run_id(),
            tree: NodeTree::new(writer),
            cfg: test_engine_cfg(),
            child_cfg: test_engine_cfg(),
            provider,
            convos: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            escalations: Mutex::new(HashMap::new()),
        };
        (harness, path, dir)
    }

    /// A machine-less `Session` for tests that only need `NodeTree::force` to
    /// have SOME session to register — never actually run a turn.
    /// `NodeTree::force` accepts any `Session` regardless of whether its
    /// machine is live, so this needs no boot expr, no GHC/extract, and
    /// cannot fail. `Harness::build_stack`'s LLM handler captures
    /// `Handle::current()`, so the CALLER must run inside a tokio runtime
    /// (`#[tokio::test]`) even though nothing here is actually awaited.
    fn fake_session(harness: &Harness) -> Session {
        let (stack, _trace) = harness.build_stack();
        ResidentSession::unbootstrapped(
            stack,
            0,
            vec![],
            CapturedOutput::new(),
            vec![],
            DEFAULT_NURSERY_SIZE,
            None,
        )
    }

    fn insert_convo(harness: &Harness, node: NodeId, effect_trace: EffectTrace) {
        harness.convos.lock().insert(
            node,
            NodeConvo {
                transcript: Vec::new(),
                turn_seq: 0,
                effect_trace,
                effect_seq: 0,
                pending: None,
                answer_contract: None,
                suspend_table: None,
                suspend_asks: AsksSidecar::default(),
                pending_bind: None,
                usage: Usage::default(),
                last_input_tokens: 0,
                framing: None,
                turn_lease: false,
            },
        );
    }

    /// A real `TreeError` from `self.tree.effect(...)` — the node is
    /// `Suspended`, not `Running`, so the durable log's own guard rejects the
    /// append before ever touching the log file. `flush_effects` must not
    /// advance `effect_seq` past that failure, and must restore both
    /// unwritten records into the node's trace, in their original order.
    #[tokio::test]
    async fn flush_effects_does_not_advance_seq_or_lose_records_on_append_failure() {
        let harness = test_harness();
        let node = harness
            .tree()
            .create_node(
                None,
                "test",
                vec!["Console".to_string()],
                ForkShape::Exact(0),
                false,
            )
            .unwrap();
        harness
            .tree()
            .force(node, Actor::Operator, fake_session(&harness))
            .unwrap();

        let rec_a = EffectRecord {
            tag: 0,
            req: serde_json::json!({"call": "a"}),
            resp: serde_json::json!({"result": "a"}),
        };
        let rec_b = EffectRecord {
            tag: 0,
            req: serde_json::json!({"call": "b"}),
            resp: serde_json::json!({"result": "b"}),
        };
        let effect_trace: EffectTrace =
            Arc::new(std::sync::Mutex::new(vec![rec_a.clone(), rec_b.clone()]));
        insert_convo(&harness, node, effect_trace);

        // Suspend the node: `NodeTree::effect` requires `Running`, so the
        // next flush's very first append hits a genuine `TreeError` straight
        // out of the durable log's own guard — no seam, no fabricated
        // failure mode.
        harness
            .tree()
            .hole_published(
                node,
                HoleId("h0".into()),
                None,
                None,
                "prompt".to_string(),
                false,
            )
            .unwrap();

        let result = harness.flush_effects(node);
        assert!(
            matches!(result, Err(HarnessError::Tree(TreeError::NotRunning(..)))),
            "expected a TreeError::NotRunning, got {result:?}"
        );

        let convos = harness.convos.lock();
        let convo = convos.get(&node).unwrap();
        assert_eq!(
            convo.effect_seq, 0,
            "effect_seq must not advance past the failed append"
        );
        let restored = convo.effect_trace.lock().unwrap();
        assert_eq!(
            restored.iter().map(|r| r.req.clone()).collect::<Vec<_>>(),
            vec![rec_a.req.clone(), rec_b.req.clone()],
            "both unwritten records must be restored, in their original order"
        );
    }

    /// A flush with nothing traced is a no-op success, not an error — the
    /// common case (an answerer/outer stack that declares no base effects).
    #[tokio::test]
    async fn flush_effects_is_a_noop_when_the_trace_is_empty() {
        let harness = test_harness();
        let node = harness
            .tree()
            .create_node(None, "test", Vec::new(), ForkShape::Exact(0), false)
            .unwrap();
        harness
            .tree()
            .force(node, Actor::Operator, fake_session(&harness))
            .unwrap();
        insert_convo(&harness, node, Arc::new(std::sync::Mutex::new(Vec::new())));

        assert!(harness.flush_effects(node).is_ok());
        assert_eq!(harness.convos.lock().get(&node).unwrap().effect_seq, 0);
    }

    /// The empty-`turn_delta` fix: a node created with an EMPTY seed (the
    /// self-iterating harness's framing-only answerer,
    /// `create_root_framed(_, "", _)`) must have no opening user turn — not
    /// in its live transcript, not in the durable log. A node created WITH a
    /// prompt must still have both.
    #[tokio::test]
    async fn force_skips_the_seed_turn_delta_only_when_the_seed_is_empty() {
        let (harness, log_path, _dir) = test_harness_with_log();

        let answerer = harness
            .create_root_framed("loop answerer", "", None)
            .unwrap();
        harness.force(answerer, Actor::Operator).unwrap();

        let prompted = harness.create_root_framed("agent", "hello", None).unwrap();
        harness.force(prompted, Actor::Operator).unwrap();

        {
            let convos = harness.convos.lock();
            assert!(
                convos.get(&answerer).unwrap().transcript.is_empty(),
                "an empty-seed node must have no opening user turn in its transcript"
            );
            assert_eq!(
                convos.get(&prompted).unwrap().transcript.len(),
                1,
                "a node created with a prompt must have one opening user turn"
            );
        }

        let (_header, events) = LogReader::open(&log_path).unwrap();
        let mut answerer_has_turn_delta = false;
        let mut prompted_turn0_content = None;
        for record in events {
            if let Event::TurnDelta {
                node,
                turn,
                content,
                ..
            } = record.unwrap().event
            {
                if node == answerer {
                    answerer_has_turn_delta = true;
                }
                if node == prompted && turn == 0 {
                    prompted_turn0_content = Some(content);
                }
            }
        }
        assert!(
            !answerer_has_turn_delta,
            "an empty-seed answerer must have NO turn_delta at all, let alone an empty one"
        );
        assert_eq!(
            prompted_turn0_content,
            Some("hello".to_string()),
            "a node created with a prompt must still log its opening user turn_delta"
        );
    }
}
