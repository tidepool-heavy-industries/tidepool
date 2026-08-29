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
use tidepool_codegen::scope::ScopeId;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_eval::value::Value;
use tidepool_mcp::CapturedOutput;
use tidepool_repr::{DataConTable, Generation, SessionId};
use tidepool_runtime::session::{
    classify_block, run_turn, Aged, BoundBinder, CompiledTurn, ResidentError, ResidentHole,
    ResidentOutcome, ResidentSession, SessionLib, TemplateSelector, TurnKind, TurnRequest,
    TurnResult, TurnTemplate, DECL_TEMPLATE_SOURCE,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;

use crate::effect_trace::{EffectRecord, EffectTrace, TracingDispatcher};
use crate::engine::{
    self, AsksSidecar, ClassifiedSuspension, EngineConfig, EngineError, SuspensionRouting,
    TurnOutcome,
};
use crate::forcing::{NodeTree, TreeError};
use crate::log::{Actor, AnswerOutcome, LogWriter};
use crate::provider::{DynModelProvider, Message, Role, Usage};
use crate::registry::{Checkout, CheckoutError};
use crate::timing;
use crate::tree::{HoleId, NodeId};

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
    #[error(transparent)]
    Classify(#[from] engine::ClassifyError),
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
    #[error("node {0:?} already has a turn in flight")]
    TurnInFlight(NodeId),
    /// A registry checkout landed on a state mismatch that is neither "no
    /// session" nor "busy" — a resume aimed at a non-member hole, or a child
    /// checkout on a holeless session (see
    /// [`CheckoutError::NotSuspended`]/[`CheckoutError::WrongHole`]). Kept
    /// distinct from both so a caller never mistakes a hole/state mismatch
    /// for "never forced" or "busy, retry". Carries the STRUCTURED
    /// `CheckoutError` (not a flattened string) so a caller can
    /// programmatically ask "what *was* pending" (e.g.
    /// `CheckoutError::WrongHole`'s `attempted`/`parked` fields) the way
    /// `tidepool-repl`'s `SessionManager::suspension_for` already lets a
    /// caller do without a second read — see the resident-session kernel
    /// design doc §1.5/§2, Open Question 4.
    #[error("node {node:?}: {source}")]
    SessionMismatch {
        node: NodeId,
        #[source]
        source: CheckoutError,
    },
    /// [`Self::resume_with_borrowed_root`] landed on a
    /// [`crate::tree`]-suspended [`ResidentHole::Binding`] hole (F9: a
    /// bind-shaped answerer turn, `h <- async (…closure-valued…); wait h`,
    /// carries the Binding obligation through every resume of that
    /// continuation) — the raw `resume_handle_borrowed` seam carries no
    /// binder/generation obligation, so honoring it would silently drop the
    /// binding the hole owes. Kept distinct from [`Self::Resident`] (a
    /// stringly-typed catch-all) so a caller CAN distinguish this specific,
    /// model-attributable shape from an opaque mechanism failure — the
    /// answerer-plane green scheduler does, routing it through
    /// `GreenRoundExit::AsyncMisuse` instead of hard-failing the run.
    #[error(
        "node {0:?}: a borrowed-root resume cannot honor a Binding hole's binder \
         obligation — refuse rather than silently drop it"
    )]
    BorrowedRootOnBindingHole(NodeId),
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
            CheckoutError::Running(_) => HarnessError::TurnInFlight(node),
            // `NoSession` (the `SingleSlot` facade's "nothing installed") and
            // `Terminal` (the REPL-only `Wedged` slot) are never produced by
            // this crate's keyed registry usage — this crate never installs
            // via `SingleSlot` and never constructs `Slot::Wedged` (a wedged
            // turn here retires the whole node via `terminate_node` instead)
            // — but the match must stay total across the shared error type.
            other @ (CheckoutError::NotSuspended(_)
            | CheckoutError::WrongHole { .. }
            | CheckoutError::NoSession
            | CheckoutError::Terminal { .. }) => HarnessError::SessionMismatch {
                node,
                source: other,
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
/// each `TurnDelta`. A node's pending suspension (when it has one) lives in
/// [`Harness::pending_suspensions`], not here — see that field's doc.
struct NodeConvo {
    transcript: Vec<Message>,
    turn_seq: u64,
    /// Shared buffer the session's [`TracingDispatcher`] appends each effect to;
    /// drained per turn by [`Harness::flush_effects`] into `Event::Effect`.
    effect_trace: EffectTrace,
    /// Monotonic per-node effect sequence number for the logged `Event::Effect`s.
    effect_seq: u64,
    /// The realm this node's turns park under on its session (attached
    /// answerer nodes each get their own realm on the SHARED machine;
    /// retirement is that realm's scope exit). `None` = the
    /// session's default realm ([`OUTER_REALM`]).
    realm: Option<tidepool_codegen::suspension::RealmId>,
    /// The scope-tree node this node's turns COMPILE and BIND in. The
    /// `realm` above is the window's HEAP-side lifetime (parked
    /// frames, handles); this is its NAME-side one (decl tip, value-plane
    /// frame). `None` = [`ScopeId::ROOT`], the flat session — which is every
    /// pre-C2 node, unchanged. [`Harness::terminate_node`] exits BOTH in one
    /// step, so a window's names and its heap roots retire together.
    scope: Option<ScopeId>,
    /// The typed hole this node is currently answering, when it answers by
    /// `finalize` (the self-iterating harness's answerer). Set per hole by
    /// [`Harness::set_answer_contract`]; read by [`Harness::run_block`] to pin
    /// `finalize` to the hole's type and to put that type in scope. `None` for
    /// every node that isn't driving toward a `finalize`.
    answer_contract: Option<AnswerContract>,
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
    /// The most recently compiled turn's extracted Haskell block (overwritten
    /// per compiled turn) — read by [`Harness::last_turn_source`] so the
    /// operator GUI can show what the answerer actually ran.
    last_turn_source: Option<String>,
    /// Set while a turn-owning operation (`drive_turn`/`summarize_turn`/an
    /// `answer_*` method) holds this node's [`TurnLease`] — guards the
    /// snapshot → provider await → log append → resident run → outcome
    /// publish span against a second concurrent turn on the SAME node.
    /// Cleared by `TurnLease::drop`, so every exit path (success, `?`, panic
    /// unwind) releases it.
    turn_lease: bool,
    /// When `true`, [`Harness::run_block`]'s checkout attempts for THIS node
    /// retry (short backoff) instead of failing fast on
    /// [`HarnessError::TurnInFlight`] — set via
    /// [`Harness::set_retry_checkout_on_contention`] for a node whose
    /// contention is EXPECTED and benign: a concurrently-driven sibling
    /// realm on the SAME shared session
    /// (`SelfHarnessDriver::drive_fanout_child`), never a re-entrant/manually
    /// held conflict. `false` by default for every node — the existing
    /// fail-fast contract (`tests/turn_lease.rs`) is unchanged unless a
    /// caller explicitly opts in. Read fresh per checkout attempt (not
    /// snapshotted), so it can be set right after node creation and take
    /// effect on that node's very first turn.
    retry_checkout_on_contention: bool,
}

/// An RAII hold on [`NodeConvo::turn_lease`], returned by
/// [`Harness::acquire_turn_lease`]. `Drop` clears the flag under the
/// `convos` lock, so success, an early `?` return, and a panic unwind all
/// release it — a node is never left permanently unleasable by a failed turn.
struct TurnLease<'a> {
    harness: &'a Harness,
    node: NodeId,
}

// A clone would let a second `TurnLease` believe it independently owns
// clearing `turn_lease`, so an early drop of one clone could release the flag
// while the other's turn is still in flight — a non-Clone guard is what makes
// "exactly one lease clears the flag, on the turn that acquired it" true by
// construction.
static_assertions::assert_not_impl_any!(TurnLease<'static>: Clone, Copy);

impl Drop for TurnLease<'_> {
    fn drop(&mut self) {
        if let Some(convo) = self.harness.convos.lock().get_mut(&self.node) {
            convo.turn_lease = false;
        }
    }
}

/// One authoritative record of domain metadata for a currently-parked hole.
/// The session registry's own hole SET (machine-reported) is the ownership
/// truth; this is the harness-level domain truth for what each hole IS and
/// what it takes to resume it — held in [`Harness::pending_suspensions`], keyed by
/// `(SessionId, HoleId)` rather than scattered across four separate
/// `NodeConvo` fields that used to be hand-kept in sync at every suspend/
/// resume/consume site.
#[derive(Clone)]
struct PendingSuspension {
    /// The node this hole belongs to — a node's own turn is suspended on AT
    /// MOST one hole at a time (`resident_hole` below is always REPLACED,
    /// never accumulated, across a suspend → resume → re-suspend cycle), so
    /// this is what makes a node-scoped read ([`Harness::node_pending`]) a
    /// plain scan rather than a second index to keep in sync. The registry's
    /// own hole SET is multi because it spans MULTIPLE NODES sharing one
    /// session (concurrently-driven attached answerer realms), not because
    /// one node juggles several holes.
    node: NodeId,
    hole: HoleId,
    classified: ClassifiedSuspension,
    /// The raw suspended request `Value`, kept alongside `classified` (which
    /// is JSON-shaped, lossy for a `Finalize` hole — its carried value may be
    /// non-serializable, e.g. a closure). `Harness::take_finalized_value`
    /// reads the finalize payload straight out of this, never through JSON.
    raw_request: Value,
    /// The typed continuation token for the suspended turn — `resume_parent`
    /// is the sole consumer, via the ONE `ResidentSession::resume`. A
    /// [`ResidentHole::Binding`] carries its own binder/generation obligation
    /// (the session materializes it into the value plane on completion);
    /// there is no second, external flag to keep in sync with which resume
    /// method to call — there is only one method, and the hole itself says
    /// what it owes.
    resident_hole: ResidentHole,
    /// Compile artifacts of the turn that suspended — needed to bridge an
    /// answer Value against the same constructor set.
    suspend_table: DataConTable,
    suspend_asks: AsksSidecar,
}

/// What crosses a node's parked hole on the node-aware resume path
/// ([`Harness::resume_parent_input`]): a bridged `Value`, or a borrowed
/// session-owned heap root (a green thread's settled handle result).
enum ResumeParentInput {
    Answer(Value),
    BorrowedRoot(tidepool_codegen::suspension::ValueHandle),
}

/// A just-created node's staged opening context, held between node creation
/// (`create_root_framed`/`register_fork_child_with_card`) and `force` (a thunk node has
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
    /// A fork/fanout child's inherited context, from `register_fork_child_with_card`:
    /// the cloned parent transcript prefix (through the fork checkpoint) plus
    /// the hole card, and the parent's framing (its system message), so the
    /// child's request prefix is byte-identical to the parent's through the
    /// checkpoint.
    Forked {
        transcript: Vec<Message>,
        framing: Option<String>,
    },
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

/// The per-item output of [`Harness::live_turn_context`] — everything one
/// singleton item's `run_turn` call needs from live session/contract state.
/// See that method's doc for why this is gathered FRESH per item rather than
/// once per block.
struct LiveTurnContext {
    expr_imports: String,
    include: Vec<PathBuf>,
    session_root: PathBuf,
    inject_modules: Vec<String>,
    gen: u64,
    /// The generation a BIND verdict materializes into — `None` when the node
    /// has no decl plane, in which case a real (non-discarding) bind is
    /// rejected (mirrors `run_block`'s single-item path).
    bind_ctx_gen: Option<Generation>,
    bind_source: String,
    binddiscard_source: String,
}

/// Render a turn-compile failure as text a MODEL can act on, with GHC's
/// coordinates remapped from TEMPLATE space to the model's own turn text
/// (`block`).
///
/// `CompileError::Diagnostics`' own `Display` reports only how many
/// diagnostics there were, not what they said — fine for a log line, useless
/// as the corrective user turn [`Harness::run_to_hole_or_done`] feeds back,
/// which is the whole mechanism by which a model fixes its own Haskell. This
/// renders the full content (severity/span/message per entry) with the
/// COORDINATES remapped, reusing `tidepool_runtime::diag`'s remapper (the
/// same one `tidepool-mcp`'s eval path uses) rather than a second one.
///
/// `run_turn` builds up to TWO full module sources before it knows which
/// verdict GHC will pick (`expr_source` always; `bind_source` — representing
/// both `Bind`/`BindDiscard`, which share its exact preamble/offset — only
/// when the node has a value-plane bind context worth trying), and a compile
/// FAILURE carries no verdict tag: `run_turn` returns before ever decoding
/// which template applied. A diagnostic is remapped against whichever
/// candidate's own (deterministically computed, from `block`) user-code
/// window its raw line falls inside — tried EXPR first, then BIND — or, when
/// neither window contains it (the wrapper-origin case: the wrapper sits
/// textually AFTER the user's code, so its line is never inside either
/// window), against the first candidate that built at all, so it renders as
/// wrapper FALLOUT rather than a raw template-coordinate dump (see
/// [`pick_render_opts`]'s doc). Only when NEITHER candidate's marker is even
/// found in its source (no candidate built) does a diagnostic keep its raw
/// template-space span — there is nothing to remap against.
fn render_compile_error(
    e: &tidepool_runtime::CompileError,
    block: &str,
    expr_source: &str,
    bind_source: &str,
) -> String {
    let tidepool_runtime::CompileError::Diagnostics(diags) = e else {
        return e.to_string();
    };
    if let Some((source, offset, ranges)) = pick_render_opts(diags, block, expr_source, bind_source)
    {
        let opts = tidepool_runtime::diag::RenderOpts {
            anchor: TURN_ANCHOR,
            label: TURN_LABEL,
            user_lines: Some(&ranges),
            line_offset: offset,
            col_indent: 0,
            // Turn compiles routinely import the session's own decl-plane
            // library modules (`Tidepool/Session/Lib/G<n>.hs`); a warning
            // anchored there is the same class of noise as a wrapper-origin
            // error — it is not something a turn's own code edits — so drop
            // every such warning unconditionally on this path.
            drop_foreign_gen_warnings_except: Some(""),
            source,
        };
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

/// `(source, line_offset, user_lines)` — the module [`render_compile_error`]
/// should render a diagnostic batch against and the `RenderOpts` fields
/// derived from it: which candidate template applies, its `line_offset`, and
/// every user-authored range within it (see [`pick_render_opts`]).
type PickedRender<'a> = (&'a str, usize, Vec<(usize, usize)>);

/// Pick which candidate template a batch of diagnostics should be remapped
/// against, and every user-authored range within it — see
/// [`render_compile_error`]'s doc for why this exists and why the choice is
/// derived, never guessed. All diagnostics in one `CompileError::Diagnostics`
/// batch come from the SAME compile, so one representative (anchor-file)
/// diagnostic's raw line decides for the whole batch.
///
/// A candidate whose window CONTAINS the representative line is preferred
/// (an ordinary, correctly-attributed diagnostic). When neither candidate's
/// window contains it — which is exactly what happens for a WRAPPER-origin
/// diagnostic, since the wrapper (`__anchor`/`paginateResult`/the render
/// call) sits textually AFTER the user's own code in the turn template —
/// this falls back to the first candidate that actually built (a non-empty
/// window), rather than returning `None`. That fallback is safe: the
/// diagnostic's line still lands OUTSIDE every user-authored range, so
/// `tidepool_runtime::diag::render_diagnostics`'s own fallout partition
/// classifies it as wrapper fallout — never displayed as if it were the
/// user's own code — instead of [`render_compile_error`] raw-dumping every
/// diagnostic in template coordinates because no candidate was picked at all
/// (poke-round finding 5, hole 2).
///
/// The returned `user_lines` prefers the REAL `-- [user-*-lines]` markers
/// baked into the chosen `source` itself (via
/// [`tidepool_runtime::diag::extract_user_code_ranges`]) over the single
/// arithmetic window [`candidate_window`] computes: the markers are parsed
/// straight out of the ACTUAL compiled text (and, for the EXPR candidate,
/// additionally cover the `helpers`/`imports` params, which
/// `candidate_window`'s single code-only window never did — the source of
/// the helpers/imports-param masking bug this fixes), so they cannot drift
/// from what GHC actually saw the way an independently-recomputed line count
/// could. Falls back to the single arithmetic window only when `source`
/// carries no markers at all (e.g. `session_bind_template`'s BIND source,
/// which never calls `TurnTemplate::render` and so never emits any — see
/// `BIND_MARKER`'s own doc; a synthetic/test source is the same shape).
fn pick_render_opts<'a>(
    diags: &[tidepool_runtime::diag::ExtractDiag],
    block: &str,
    expr_source: &'a str,
    bind_source: &'a str,
) -> Option<PickedRender<'a>> {
    let representative_line = diags.iter().find_map(|d| {
        let span = d.span.as_ref()?;
        span.file
            .ends_with(TURN_ANCHOR)
            .then_some(span.start_line as usize)
    })?;
    let content_lines = engine::content_line_count(block);
    let ranges_for = |source: &'a str, window: (usize, usize)| {
        tidepool_runtime::diag::extract_user_code_ranges(source).unwrap_or_else(|| vec![window])
    };
    let candidates: Vec<(&'a str, usize, (usize, usize))> =
        [(expr_source, EXPR_MARKER), (bind_source, BIND_MARKER)]
            .into_iter()
            .filter_map(|(source, marker)| {
                candidate_window(source, marker, content_lines)
                    .map(|(offset, window)| (source, offset, window))
            })
            .collect();

    if let Some(&(source, offset, window)) = candidates
        .iter()
        .find(|(_, _, (start, end))| representative_line >= *start && representative_line <= *end)
    {
        return Some((source, offset, ranges_for(source, window)));
    }

    candidates
        .into_iter()
        .next()
        .map(|(source, offset, window)| (source, offset, ranges_for(source, window)))
}

/// The orchestrator. Cloneable-cheap? No — it owns the tree + sessions, so it
/// is shared behind an `Arc`.
/// The reserved realm every NODE-LESS outer-surface park is owned by —
/// `with_session` resets the session's ambient realm to this before every
/// outer run/resume, so an outer frame parked by a re-suspension can never
/// be owned by (and accidentally closed with) whichever answerer realm ran
/// last. Attached answerer realms are minted per loop from 1 upward.
pub const OUTER_REALM: tidepool_codegen::suspension::RealmId =
    tidepool_codegen::suspension::RealmId(0);

/// A queued window exit: the two halves of an attached node's retirement that
/// need the machine in hand. Either half may be absent (a node with a realm and
/// no scope is every pre-C2 attached node).
struct PendingSessionExit {
    session: tidepool_repr::SessionId,
    node: NodeId,
    realm: Option<tidepool_codegen::suspension::RealmId>,
    scope: Option<ScopeId>,
}

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
    provider: Arc<dyn DynModelProvider>,
    convos: Mutex<HashMap<NodeId, NodeConvo>>,
    /// Item 5 of the session-ownership capstone: the ONE suspension-metadata
    /// truth, keyed by `(SessionId, HoleId)` rather than bare `HoleId` — the
    /// JIT's `scont_N` continuation ids are minted per-machine, so two
    /// independent sessions can legitimately produce the same string. See
    /// [`PendingSuspension`]'s doc for why the map is node-scannable without a
    /// second index, and [`Harness::publish_suspension`]/[`Harness::consume_suspension`]
    /// for the one place transitions happen (mutations of this map are what
    /// drive the tree's `hole_published`/`hole_consumed` log events, not the
    /// other way around).
    /// Each entry is [`Aged`]-wrapped (#22 design doc §3.2 item 5): the
    /// kernel's abandonment-liveness primitive, giving [`Self::
    /// pending_hole_age`] a hole's age for FREE at the cost of one `Instant`
    /// per entry. No sweep reads it today — the kernel drives no timer and
    /// this crate constructs no reaper (OQ3: hook exposed, no default) — an
    /// unanswered hole still sits suspended forever exactly as before; the
    /// age is simply now a query away instead of undiscoverable, ready for
    /// a future harness that wants to opt into a TTL the way `tidepool-repl`
    /// already does.
    pending_suspensions: Mutex<HashMap<(SessionId, HoleId), Aged<PendingSuspension>>>,
    /// Window exits QUEUED because the attached node's retirement found the
    /// shared machine out on a turn — drained by the next path holding the
    /// machine (`run_checked_out`/`with_session`). An eventual postcondition,
    /// never a best-effort side effect; both halves of a window's identity (its
    /// REALM, whose close reclaims parked frames and handles, and its SCOPE,
    /// whose retirement drops the value-plane frame and deregisters the roots
    /// it solely owns) live here until the exit is confirmed.
    pending_session_exits: Mutex<Vec<PendingSessionExit>>,
    /// A just-created node's staged [`NodeSeed`] — a root's opening prompt or
    /// a fork child's inherited transcript, either way paired with its
    /// framing — between node creation and `force` (a thunk node has no live
    /// `NodeConvo` to hold it yet). Removed once consumed at force time.
    pending: Mutex<HashMap<NodeId, NodeSeed>>,
}

impl Harness {
    /// The effect-row a window-opening hole card should STATE:
    /// `self.cfg.effect_names` for a non-delegating config, or the narrow
    /// `Delegate`-form row a delegating config's model-facing block actually
    /// compiles against — see [`EngineConfig::finalize_typed_request_prompt_effect_row`]. Never
    /// used for tag lookup (`self.cfg.effect_names`/`flush_effects` stay the
    /// real dispatched row).
    pub fn finalize_typed_request_prompt_effect_row(&self) -> Vec<String> {
        self.cfg.finalize_typed_request_prompt_effect_row()
    }

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
        Ok(Harness {
            tree: NodeTree::new(writer),
            cfg,
            run_id,
            provider,
            convos: Mutex::new(HashMap::new()),
            pending_suspensions: Mutex::new(HashMap::new()),
            pending_session_exits: Mutex::new(Vec::new()),
            pending: Mutex::new(HashMap::new()),
        })
    }

    /// `node`'s current pending hole, if it has one — a plain scan of
    /// [`Self::pending_suspensions`] (small in practice: bounded by concurrent
    /// fanout width, not corpus size), never a second node→hole index to
    /// keep in sync. See [`PendingSuspension`]'s doc for why a node has at most
    /// one entry here at a time.
    fn node_pending(&self, node: NodeId) -> Option<PendingSuspension> {
        self.pending_suspensions
            .lock()
            .values()
            .find(|p| p.get().node == node)
            .map(|p| p.get().clone())
    }

    /// How long `node`'s pending hole (if any) has been parked — the
    /// abandonment-liveness hook's read side (#22 design doc §3.2 item 5,
    /// OQ3). `None` when `node` has no pending suspension. Nothing calls
    /// this today; it exists so a future harness reaper can be built
    /// without first inventing where a hole's age would even come from.
    pub fn pending_hole_age(&self, node: NodeId) -> Option<std::time::Duration> {
        self.pending_suspensions
            .lock()
            .values()
            .find(|p| p.get().node == node)
            .map(Aged::age)
    }

    /// Publish a hole: log `Event::HolePublished` (the tree's `Running →
    /// Suspended{hole}` transition) and insert `pending`'s domain metadata
    /// into [`Self::pending_suspensions`] — ONE call replacing the four-mutation
    /// choreography (`hole_published` + `set_pending` + stashing
    /// `resident_hole`/`suspend_table`/`suspend_asks` separately) that used
    /// to be duplicated verbatim at every suspend site. Called from both a
    /// first suspend ([`Self::finish_run`]) and a re-suspend
    /// ([`Self::resume_parent`]).
    fn publish_suspension(
        &self,
        node: NodeId,
        sid: SessionId,
        pending: PendingSuspension,
    ) -> Result<(), HarnessError> {
        let classified = &pending.classified;
        let fork = matches!(classified.routing, SuspensionRouting::Fork { .. });
        let ty = match &classified.routing {
            SuspensionRouting::Fork { ty, .. }
            | SuspensionRouting::RunLLMTurn { ty, .. }
            | SuspensionRouting::Finalize { ty, .. } => ty.clone(),
            _ => None,
        };
        let site = match &classified.routing {
            SuspensionRouting::Fork { site, .. }
            | SuspensionRouting::RunLLMTurn { site, .. }
            | SuspensionRouting::Finalize { site, .. } => Some(*site),
            _ => None,
        };
        self.tree.hole_published(
            node,
            pending.hole.clone(),
            site,
            ty,
            classified.prompt.clone(),
            fork,
        )?;
        self.pending_suspensions
            .lock()
            .insert((sid, pending.hole.clone()), Aged::new(pending));
        Ok(())
    }

    /// Consume `node`'s pending `hole`: log `Event::HoleConsumed` (the
    /// tree's `Suspended{hole} → Running` transition) and remove its domain
    /// metadata from [`Self::pending_suspensions`] — the two-mutation counterpart
    /// to [`Self::publish_suspension`], replacing the scattered `convo.pending =
    /// None` / `convo.resident_hole = None` pairs at every resume-success
    /// site.
    fn consume_suspension(
        &self,
        node: NodeId,
        sid: SessionId,
        hole: &HoleId,
    ) -> Result<(), HarnessError> {
        self.tree.hole_consumed(node, hole.clone())?;
        self.pending_suspensions.lock().remove(&(sid, hole.clone()));
        Ok(())
    }

    /// Drive one model turn via the provider — no `StreamSink`: nothing here
    /// observes deltas, and the provider contract promises the same
    /// assembled `TurnResponse` with `sink: None` as with one wired (see
    /// `provider::oauth::codex_responses`'s doc). Shared by the root turn
    /// loop and the fork/fanout answerer loops.
    async fn stream_turn(
        &self,
        transcript: &[Message],
        framing: Option<&str>,
    ) -> Result<engine::DrivenTurn, HarnessError> {
        engine::drive_model_turn(
            self.provider.as_ref(),
            transcript,
            Some(engine::DEFAULT_MAX_TOKENS),
            framing,
            None,
        )
        .await
        .map_err(Into::into)
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
            let records: Vec<EffectRecord> = std::mem::take(&mut *convo.effect_trace.lock());
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
                        let mut trace = convo.effect_trace.lock();
                        unwritten.append(&mut trace);
                        *trace = unwritten;
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
        let node = self
            .tree
            .create_node(None, title, self.cfg.effect_names.clone())?;
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
        self.force_with_extra_include(node, actor, Vec::new())
    }

    /// As [`Self::force`], but with `extra_include` roots ALSO on every
    /// compile this node's session runs — the seam a fork child's session
    /// uses to resolve its PARENT's decl-plane module by name:
    /// `AnswerContract`'s pin (built from
    /// [`tidepool_runtime::AsksSidecar::modules_of`]) names the module, and
    /// this is what makes that name findable on disk. `SessionModule`'s
    /// dotted name (`Tidepool.Session.{Val|Lib}.G<g>`) carries no node
    /// identity, so the file layout under any node's own decl root matches
    /// the same relative path regardless of which node it belongs to —
    /// adding the PARENT's root here is exactly as if the child's own
    /// session had declared the SAME thing, without actually sharing a
    /// session (parent and child still have their own, independent planes —
    /// see [`Self::force`]'s doc).
    fn force_with_extra_include(
        &self,
        node: NodeId,
        actor: Actor,
        extra_include: Vec<PathBuf>,
    ) -> Result<(), HarnessError> {
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
        let mut include = self.cfg.include.clone();
        include.extend(extra_include);
        let session = ResidentSession::unbootstrapped(
            stack,
            self.cfg.suspend_tag,
            self.cfg.effect_names.clone(),
            CapturedOutput::new(),
            include,
            DEFAULT_NURSERY_SIZE,
            lib,
        );

        // Register with the tree AND the session registry it owns (emits
        // Forced before the session is visible, then mints the SessionId and
        // inserts the machine as Idle — `NodeTree::force`'s one job).
        self.tree.force(node, actor, session)?;

        self.seed_convo(node, effect_trace)?;
        Ok(())
    }

    /// Force `node` ONTO the shared session `sid`: the
    /// same consent line and transcript/convo seeding as [`Self::force`], but
    /// no machine is built — the node's turns run as a realm on the shared
    /// machine (assign one via [`Self::set_node_realm`]), and its retirement
    /// is realm scope-exit ([`Self::terminate_node`] on a non-owning node).
    /// The convo's effect trace is fresh and never fed (the scoped answerer
    /// rows are all-suspending; nothing dispatches — the absence IS the
    /// capability boundary, unchanged by sharing the machine).
    pub fn force_attached(
        &self,
        node: NodeId,
        actor: Actor,
        sid: tidepool_repr::SessionId,
    ) -> Result<(), HarnessError> {
        self.tree.force_attached(node, actor, sid)?;
        self.seed_convo(node, EffectTrace::default())?;
        Ok(())
    }

    /// Adopt a node-less session into the tree's registry (the shared OUTER
    /// session) — the caller owns its retirement (see
    /// [`Self::retire_adopted_session`]).
    pub fn adopt_session(&self, session: Session) -> tidepool_repr::SessionId {
        self.tree.adopt_session(session)
    }

    /// Retire a node-LESS adopted session (F6: [`Self::adopt_session`]'s own
    /// doc names the caller as owning retirement, but until this existed
    /// nothing actually called it — the driver's own
    /// `discard_resident_state` dropped only its own `sid` handle on a cycle
    /// error, never removing the machine from the registry, so the session
    /// — heap, code arena, every still-parked frame — stayed alive there
    /// forever; the next `bootstrap` adopts a FRESH session under a NEW
    /// `sid`, so the old one becomes unreachable garbage that is never
    /// collected). This is [`Self::terminate_node`]'s sibling for a session
    /// that was never forced onto the tree at all, so there is no [`NodeId`]
    /// to route a removal through.
    pub fn retire_adopted_session(&self, sid: tidepool_repr::SessionId) {
        self.tree.registry().remove(sid);
    }

    /// Replace the machine under `sid` with a fresh one (machine ROTATION).
    /// The caller guarantees quiescence (no
    /// parked holes, no turn in flight): this is the driver's own
    /// loop-boundary maintenance on a session it owns, and the old machine
    /// drops here (heap + roots reclaimed; the leaked code arena is the
    /// bounded cost rotation exists to bound).
    pub fn replace_session(
        &self,
        sid: tidepool_repr::SessionId,
        session: Session,
    ) -> Result<(), HarnessError> {
        self.tree.registry().insert_idle(sid, session);
        Ok(())
    }

    /// Seed a freshly-forced node's transcript + convo entry (shared tail of
    /// [`Self::force`] and [`Self::force_attached`]).
    fn seed_convo(&self, node: NodeId, effect_trace: EffectTrace) -> Result<(), HarnessError> {
        // Seed the transcript: a fork/fanout answerer inherits its parent's
        // transcript (staged by `register_fork_child_with_card`); a plain root gets its
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
                realm: None,
                scope: None,
                answer_contract: None,
                last_input_tokens: 0,
                framing,
                last_turn_source: None,
                turn_lease: false,
                retry_checkout_on_contention: false,
            },
        );
        Ok(())
    }

    /// Run a closure against the shared (node-less) session `sid` under the
    /// full checkout discipline — the driver's outer render/loop
    /// runs and resumes go through here, restoring with the session's OWN
    /// reported hole set. Synchronous by design (the driver's outer calls
    /// always were); the machine mutation happens on the caller's thread.
    ///
    /// Two realm disciplines live here (codex review 2026-08-12, both Highs):
    /// the session's ambient realm is RESET to the reserved outer realm
    /// before `f` (an outer frame parked by a re-suspension must never be
    /// owned by whichever answerer realm ran last — ambient stickiness would
    /// let an answerer's retirement close an OUTER frame), and any QUEUED
    /// realm closes for this session are drained while the machine is in
    /// hand (the eventual-postcondition half of attached-node retirement).
    pub fn with_session<T>(
        &self,
        sid: tidepool_repr::SessionId,
        f: impl FnOnce(&mut Session) -> T,
    ) -> Result<T, HarnessError> {
        let co = self
            .tree
            .registry()
            .checkout_run(sid)
            .map_err(|e| HarnessError::Resident(format!("outer session checkout: {e}")))?;
        Ok(self.run_with_checkout(sid, co, f))
    }

    /// [`Self::with_session`], but — ONLY when `node` opted in via
    /// [`Self::set_retry_checkout_on_contention`] — retries a checkout
    /// contention refusal with a short backoff instead of failing fast, the
    /// SAME idiom [`Self::checkout_run_retrying`] applies to a node-keyed
    /// checkout (F4: a raw `with_session` call against an attached answerer's
    /// SHARED session has no contention retry at all today, so a concurrently
    /// -driven sibling holding the machine turns "expected, benign
    /// contention" into a mechanism failure that hard-fails the whole
    /// fanout). `node` is used solely to look up that opt-in flag — the
    /// checkout itself is still against `sid`, exactly like `with_session`.
    /// A node that never opts in gets EXACTLY `with_session`'s behavior.
    pub async fn with_session_retrying<T>(
        &self,
        node: NodeId,
        sid: tidepool_repr::SessionId,
        f: impl FnOnce(&mut Session) -> T,
    ) -> Result<T, HarnessError> {
        let retry = self
            .convos
            .lock()
            .get(&node)
            .map(|c| c.retry_checkout_on_contention)
            .unwrap_or(false);
        if !retry {
            return self.with_session(sid, f);
        }
        let deadline = tokio::time::Instant::now() + Self::CONTENTION_RETRY_BUDGET;
        loop {
            match self.tree.registry().checkout_run(sid) {
                Ok(co) => return Ok(self.run_with_checkout(sid, co, f)),
                Err(CheckoutError::Running(_)) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Self::CONTENTION_RETRY_BACKOFF).await;
                }
                Err(e) => {
                    return Err(HarnessError::Resident(format!(
                        "outer session checkout: {e}"
                    )));
                }
            }
        }
    }

    /// Shared tail of [`Self::with_session`]/[`Self::with_session_retrying`]:
    /// given an already-successful checkout, drain queued window exits,
    /// reset the ambient realm/scope, run `f`, and restore with the
    /// session's own reported hole set — the one place this sequence is
    /// written, so the two checkout strategies (fail-fast, retrying) cannot
    /// drift on what happens once the machine is actually in hand.
    fn run_with_checkout<T>(
        &self,
        sid: tidepool_repr::SessionId,
        mut co: Checkout<'_, Session>,
        f: impl FnOnce(&mut Session) -> T,
    ) -> T {
        self.drain_pending_session_exits(sid, co.machine());
        co.machine().set_realm(OUTER_REALM);
        // Same reset, name side: the shared session's own runs are ROOT-scoped,
        // never sticky on whichever answerer window ran last. ROOT is always
        // live (`ScopeTree::is_live`), so this can never be refused.
        #[allow(clippy::expect_used, reason = "ScopeId::ROOT is always live")]
        co.machine()
            .set_scope(ScopeId::ROOT)
            .expect("ScopeId::ROOT is always live");
        let r = f(co.machine());
        let holes: Vec<HoleId> = co
            .machine()
            .parked_holes()
            .into_iter()
            .map(|h| HoleId(h.to_string()))
            .collect();
        co.restore_suspended(holes);
        r
    }

    /// Apply every queued window exit for `sid` (attached-node retirements
    /// that found the machine out on a turn). Called by each path that has
    /// the machine in hand, so a window's exit converges even when retirement
    /// raced a running turn.
    fn drain_pending_session_exits(&self, sid: tidepool_repr::SessionId, session: &mut Session) {
        let pending: Vec<_> = {
            let mut q = self.pending_session_exits.lock();
            let (mine, rest): (Vec<_>, Vec<_>) = q.drain(..).partition(|e| e.session == sid);
            *q = rest;
            mine
        };
        for exit in pending {
            self.exit_agent_session(session, exit.node, exit.realm, exit.scope);
        }
    }

    /// The ONE place a window's realm close and scope retirement happen, so
    /// the immediate path (`terminate_node` with the machine in hand) and the
    /// queued path (`drain_pending_session_exits`) cannot diverge. Realm first
    /// (parked frames and outstanding handles go), then scope — scope
    /// retirement's sole-ownership rule reads the handle registry, so a handle
    /// the realm still owned would otherwise wrongly pin a root.
    fn exit_agent_session(
        &self,
        session: &mut Session,
        node: NodeId,
        realm: Option<tidepool_codegen::suspension::RealmId>,
        scope: Option<ScopeId>,
    ) {
        if let Some(realm) = realm {
            let (frames, handles) = session.close_realm(realm);
            if frames + handles > 0 && std::env::var("HARNESS_DEBUG").is_ok() {
                eprintln!(
                    "[harness] realm scope-exit for {node:?}: {frames} frame(s), \
                     {handles} handle(s) released"
                );
            }
        }
        if let Some(scope) = scope.filter(|s| !s.is_root()) {
            let receipt = session.retire_scope(scope);
            if std::env::var("HARNESS_DEBUG").is_ok() {
                eprintln!(
                    "[harness] scope retirement for {node:?} ({scope:?}): {} scope(s), \
                     {} binding(s), {} root(s) released",
                    receipt.scopes_retired, receipt.bindings_retired, receipt.roots_released
                );
            }
        }
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
        let trace: EffectTrace = Arc::new(Mutex::new(Vec::new()));
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
        // The faithful decl env for this node's ACTUAL effect set (the full
        // include keeps the effects dir, so `Tidepool.Effects`/companions
        // resolve here) — `standalone_default` has no Prelude, so a decl
        // naming ambient turn vocabulary (`Text`, `object`) failed validation.
        SessionLib::open(
            SessionId(node.0),
            &root,
            tidepool_mcp::session_decl_module_env(&self.cfg.decls, false),
        )
        .map(|lib| lib.with_validation_include(self.cfg.include.clone()))
        .ok()
    }
}

/// Project a [`CompiledTurn`]'s `(site, type, modules)` asks down to the
/// `(site, type)` pairs [`Harness::log_turn_extracted`]/the durable
/// `Event::TurnExtracted` log carry — that event's own wire shape is
/// unaffected by this lane's module lookup (nothing reads modules off it;
/// a durable log a caller is replaying should not gain a new required
/// field), so this is a projection at the log-writing boundary, not a
/// second copy of the asks data.
fn asks_log_pairs(asks: &[(u32, String, Vec<String>)]) -> Vec<(u32, String)> {
    asks.iter().map(|(s, t, _)| (*s, t.clone())).collect()
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
            // The latest turn's input_tokens IS the node's real context
            // size (the provider re-sends the whole transcript each round, so
            // its input count already includes every prior turn). Overwrite,
            // don't accumulate — this is the high-water the compaction
            // threshold reads.
            convo.last_input_tokens = driven.usage.input_tokens;
        }

        let blocks = driven.blocks;
        if blocks.is_empty() {
            // A prose-only turn ran no Haskell, but
            // still record a `TurnStart` whose `source` is the reply text — so a
            // node's durable log always shows one `TurnStart` per model turn
            // (`tail`ing it never has a silent gap), and consent integrity's
            // "no Turn/Effect before Forced" holds (this is well after Forced).
            self.tree.turn_start(node, driven.reply.clone(), None)?;
            return Ok(engine::TurnOutcome::NoBlock {
                reply: driven.reply,
            });
        }

        // Record the EXTRACTED executed Haskell as this turn's
        // `TurnStart.source` — so `tail -f <log>` shows the exact source the
        // turn ran, not a coarse "model" provenance tag. ONE `TurnStart` per
        // model turn (the durable-log invariant), carrying the whole runnable
        // sequence; emitted before any block runs, so it precedes this turn's
        // Effect / HolePublished events in the durable log. A multi-block
        // sequence gets comment separators so the log (and the operator GUI's
        // turn pane, fed from `last_turn_source`) shows where each separately
        // compiled block began; a single block stays byte-exact.
        let joined = if blocks.len() == 1 {
            blocks[0].clone()
        } else {
            blocks
                .iter()
                .enumerate()
                .map(|(i, b)| format!("-- ── block {} of {} ──\n{b}", i + 1, blocks.len()))
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        self.tree.turn_start(node, joined.clone(), None)?;
        // INFO, not DEBUG: a person watching the console must see the exact
        // source every compile ran (dogfood-observability deliverable 1) —
        // full text, never truncated (a pathologically large source is
        // itself signal worth seeing).
        tracing::info!(node = node.0, source = %joined, "compiled turn source");
        if let Some(convo) = self.convos.lock().get_mut(&node) {
            convo.last_turn_source = Some(joined);
        }

        // Run the blocks in order as ONE sequence (the multi-block contract:
        // every ```haskell block runs; later blocks see earlier blocks'
        // declarations and bindings). Stop at the first failure — a later
        // block's compile can depend on an earlier bind's LIVE value (the
        // Val-module mechanism), so pre-compiling the whole sequence is not
        // possible, and stop-at-first-error with an explicit resume point is
        // the honest contract. A suspension parks the sequence: unrun blocks
        // are dropped, with a transcript note so the model resends them when
        // the window continues (a `finalize` ends the window — the model
        // never sees another turn, so that case is observer-only).
        let total = blocks.len();
        let mut receipts: Vec<String> = Vec::new();
        let mut last_rendered = String::new();
        for (ix, block) in blocks.iter().enumerate() {
            let n = ix + 1;
            if ix > 0 {
                // The previous block's completion left the node `Done`.
                self.reopen_node(node)?;
            }
            let (imports, body) = engine::split_imports(block);
            match self.run_block(node, &body, &imports, "").await {
                Ok(engine::TurnOutcome::Completed { rendered }) => {
                    receipts.push(engine::block_receipt(n, block, &rendered));
                    last_rendered = rendered;
                }
                Ok(out @ engine::TurnOutcome::Suspended { .. }) => {
                    if n < total {
                        let is_finalize = matches!(
                            &out,
                            engine::TurnOutcome::Suspended { classified, .. }
                                if matches!(
                                    classified.routing,
                                    engine::SuspensionRouting::Finalize { .. }
                                )
                        );
                        if is_finalize {
                            tracing::warn!(
                                node = node.0,
                                unrun = total - n,
                                "finalize in block {n} of {total} — later blocks never run"
                            );
                        } else {
                            self.push_user_turn(
                                node,
                                &format!(
                                    "Note: block {n} of {total} suspended awaiting an \
                                     answer, so the blocks after it did not run. Blocks \
                                     1–{n} ran and persist — when your window continues, \
                                     pick up from block {}.",
                                    n + 1
                                ),
                            )?;
                        }
                    }
                    return Ok(out);
                }
                // `run_block` never yields `NoBlock`; pass it through if it ever does.
                Ok(out @ engine::TurnOutcome::NoBlock { .. }) => return Ok(out),
                // A compile-class failure mid-sequence: wrap the GHC error in
                // the sequence context (what ran, what didn't, where to resume)
                // so every corrective wrapper carries it verbatim. A
                // single-block turn keeps the bare error — today's shape.
                Err(HarnessError::Compile(msg)) if total > 1 => {
                    return Err(HarnessError::Compile(engine::sequence_failure_context(
                        &receipts, n, total, &msg,
                    )));
                }
                Err(e) => return Err(e),
            }
        }
        Ok(engine::TurnOutcome::Completed {
            rendered: last_rendered,
        })
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
            convo.last_input_tokens = driven.usage.input_tokens;
        }
        Ok((driven.reply, driven.usage))
    }

    /// Compile a `block` (with optional imports/helpers) and run it against
    /// `node`'s resident session as a TOP-LEVEL turn. Classifies a suspension.
    ///
    /// A block that [`engine::split_block_items`] finds more than one item in
    /// (a helper declaration followed by the answer expression, say) routes
    /// to [`Self::run_multi_item_block`] instead — see that method for the
    /// block lane's classify-then-run contract. **A single-item block takes
    /// the ORIGINAL path below, unchanged**: this is the pinned behavior —
    /// same spawn count (one `run_turn`, verdict discovered internally), same
    /// error surfaces — that adopting the block lane must not disturb.
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
        if engine::split_block_items(block).len() > 1 {
            return self
                .run_multi_item_block(node, block, imports, helpers)
                .await;
        }
        // A delegating config's `runDelegate` wrap lives entirely in the
        // Expr/Bind/BindDiscard TEMPLATES (`engine::expr_turn_template`/
        // `engine::session_bind_template`, via `EngineConfig::delegate_wrap`)
        // — applied at each candidate's own RESULT position, never as a text
        // prepend to `block` itself. `block` (and so every candidate,
        // including `Decl`) therefore compiles the model's text UNMODIFIED,
        // which is what makes a top-level `data`/decl item legal in a
        // delegating window.
        // The node's answer contract (when driving toward a `finalize`)
        // instantiates the ROW (`Finalize <ty>`) this turn compiles against —
        // `turn_target` resolves both the include dir and the stack string
        // from the SAME row, so they cannot disagree.
        let contract = self.answer_contract(node);
        // The node's session bind context, read BEFORE `turn_target` so a
        // pinned row naming a MODEL-declared decl-plane type validates
        // through the same include set AND session-value injection its turn
        // BODY compiles against below (`live_turn_context`'s own
        // `session_bind_context` read) — see
        // `EngineConfig::turn_target_with_extra_validation_include`'s doc.
        let session_bind = self.session_bind_context(node);
        let extra_session = session_bind.as_ref().map(|(_, inject_modules, root, _)| {
            tidepool_runtime::SessionInject {
                session_root: root.as_path(),
                inject_modules: inject_modules.as_slice(),
            }
        });
        let target = self.cfg.turn_target_with_extra_validation_include(
            contract
                .as_ref()
                .map(|c| (c.ty.as_str(), c.imports.as_slice())),
            extra_session,
        )?;

        // Session/contract/bind-template context — exactly what
        // `run_multi_item_block`'s singleton-item path gathers fresh per item
        // (see `live_turn_context`'s doc); a single-item block is that same
        // shape with one item.
        let ctx = self.live_turn_context(node, imports, helpers, &target.include)?;

        // Build every template `run_turn` might select — the verdict, and so
        // which one applies, isn't known until it returns. Timed from HERE,
        // not from the context gathering above: a new answer type
        // materializing an effects module (a filesystem write) is not
        // templating cost and would inflate this stage's attribution.
        let template_started = std::time::Instant::now();
        let expr_source =
            engine::expr_turn_template(&self.cfg, &target.stack, block, &ctx.expr_imports, helpers);
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
                source: ctx.bind_source.clone(),
            },
            TurnTemplate {
                kind: TemplateSelector::BindDiscard,
                source: ctx.binddiscard_source.clone(),
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

        let include = ctx.include.clone();
        let session_root = ctx.session_root.clone();
        let inject_modules = ctx.inject_modules.clone();
        let gen = ctx.gen;

        // The DECL path needs the turn's import lines back: `drive_turn`
        // split them off the block, but a declaration's imports (author
        // types it mentions) must ride INTO the decl plane, where
        // `extract_user_imports` hoists them into the generated module —
        // without this a decl naming an author type fails validation with
        // "not in scope". ONLY the define input gets them re-glued: the turn
        // COMPILE below must keep the import-stripped block (the templates
        // splice imports separately; re-gluing them into the expression
        // placeholder is a parse error in every template).
        let decl_source = if imports.is_empty() {
            block.to_string()
        } else {
            let rebuilt: String = imports
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| format!("import {l}\n"))
                .collect();
            format!("{rebuilt}\n{block}")
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
            HarnessError::Compile(render_compile_error(
                &e,
                block,
                &expr_source,
                &ctx.bind_source,
            ))
        })?;

        match outcome {
            TurnResult::Decl { .. } => {
                let checkout = self.checkout_run_retrying(node).await?;
                // Into the node's OWN scope: the definition joins that scope's
                // decl tip (which already re-exports its ancestors'), so it is
                // visible to this window and its descendants and to nobody
                // else — a sibling window never gains it, and neither does the
                // parent. `run_checked_out` has already applied the scope to
                // the session, so this reads it back rather than re-deriving.
                let scope = self.node_scope(node);
                let res = self
                    .run_checked_out(node, checkout, move |mut session| {
                        let r = session.define_scoped_in(scope, &[&decl_source]);
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
                    // A failed decl validation is a COMPILE-class error — the
                    // caller's corrective-retry loop feeds it back as another
                    // round, exactly like a failed turn compile. Surfacing it
                    // as `Resident` killed the whole driver on the model's
                    // first bad decl (companion dogfood, 2026-08-14).
                    Err(e) => Err(HarnessError::Compile(e.to_string())),
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
                let Some(gen) = ctx.bind_ctx_gen else {
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
                self.run_bind_turn(node, binder, compiled, gen, true).await
            }
            // A discarding bind (`_ <- e`) or a bare expression: run for
            // effect/value, no binding materializes on the value plane.
            TurnResult::Bind { compiled, .. } | TurnResult::Expr { compiled, .. } => {
                self.log_turn_extracted(node, &asks_log_pairs(&compiled.asks), None)?;
                let asks = AsksSidecar::from_entries(compiled.asks);
                let table = compiled.table;
                let expr = compiled.expr;

                // Run the compiled fragment against the session (move it onto
                // the blocking pool and back — the resident session is `Send`).
                let checkout = self.checkout_run_retrying(node).await?;
                let run_table = table.clone();
                let run_outcome = self
                    .run_checked_out(node, checkout, move |mut session| {
                        let out = session.run("turn", &expr, &run_table);
                        (session, out)
                    })
                    .await?;

                self.finish_run(node, run_outcome, table, asks, true)
            }
        }
    }

    /// The session/contract context ONE item's compile needs — everything
    /// [`Self::run_block`]'s single-item path gathers before its `run_turn`
    /// call, factored out so [`Self::run_multi_item_block`] can gather it
    /// FRESH before every singleton item rather than once for the whole
    /// block: an earlier item in the SAME block may have committed a
    /// declaration or materialized a bind, and this item must compile
    /// against whatever the session actually looks like right now — exactly
    /// what a sequence of independent turns already does.
    fn live_turn_context(
        &self,
        node: NodeId,
        imports: &str,
        helpers: &str,
        target_include: &[PathBuf],
    ) -> Result<LiveTurnContext, HarnessError> {
        let (session_module, session_include) = self.session_decl_context(node);
        let bind_ctx = self.session_bind_context(node);
        let contract = self.answer_contract(node);

        let mut expr_import_lines: Vec<String> = contract
            .iter()
            .flat_map(|c| c.imports.iter().cloned())
            .collect();
        if !imports.is_empty() {
            expr_import_lines.push(imports.to_string());
        }
        expr_import_lines.extend(session_module.clone());
        if let Some((session_imports, ..)) = &bind_ctx {
            if !session_imports.is_empty() {
                expr_import_lines.push(session_imports.clone());
            }
        }
        let expr_imports = expr_import_lines.join("\n");

        let mut bind_import_lines: Vec<String> = contract
            .iter()
            .flat_map(|c| c.imports.iter().cloned())
            .collect();
        if !imports.is_empty() {
            bind_import_lines.push(imports.to_string());
        }
        if let Some((session_imports, ..)) = &bind_ctx {
            if !session_imports.is_empty() {
                bind_import_lines.push(session_imports.clone());
            }
        }
        let bind_imports = bind_import_lines.join("\n");

        let bind_source =
            engine::session_bind_template(&self.cfg, "{{BINDERS}}", &bind_imports, helpers);
        let binddiscard_source =
            engine::session_bind_template(&self.cfg, "()", &bind_imports, helpers);

        let mut include = target_include.to_vec();
        if let Some(dir) = session_include {
            include.push(dir);
        }

        let scratch_root;
        let (session_root, inject_modules, gen, bind_ctx_gen) = match &bind_ctx {
            Some((_, inject, root, gen)) => (root.clone(), inject.clone(), gen.0, Some(*gen)),
            None => {
                scratch_root = tempfile::TempDir::new()
                    .map_err(|e| HarnessError::Resident(format!("scratch session root: {e}")))?;
                (scratch_root.path().to_path_buf(), Vec::new(), 0, None)
            }
        };

        Ok(LiveTurnContext {
            expr_imports,
            include,
            session_root,
            inject_modules,
            gen,
            bind_ctx_gen,
            bind_source,
            binddiscard_source,
        })
    }

    /// Multi-item sibling of [`Self::run_block`], reached only when
    /// [`engine::split_block_items`] finds more than one item in `block` (a
    /// helper declaration followed by the answer expression, say), adopted
    /// from `tidepool-repl`'s `Session::run_block`/`drive_block`: classify the
    /// WHOLE block in ONE [`classify_block`] spawn, then run each item with
    /// its verdict already in hand.
    ///
    /// A maximal run of consecutive DECL-shaped items batches into ONE
    /// [`ResidentSession::define_scoped_in`] generation (so a signature and
    /// its binding, split across items, typecheck together) — the same
    /// batching primitive the decl-items harvest already uses, not a second
    /// implementation. A bind/expr item compiles through the same `run_turn`
    /// rail [`Self::run_block`]'s single-item path uses, with `verdict:
    /// Some(..)` so the extract skips its own re-parse.
    ///
    /// Stops at the first compile error or suspension — on suspension the
    /// remaining items are simply dropped and a corrective note is pushed
    /// (unless the suspension is a `finalize`, which ends the window), the
    /// exact contract [`Self::drive_turn`]'s outer multi-BLOCK sequence
    /// already has, one level down. The block's LAST item must classify as
    /// something that RUNS (a bind or a bare expression) — ending on a bare
    /// declaration compiles and persists fine but advances nothing, so it is
    /// a typed error rather than a silent "declared" completion (a
    /// single-item block keeps its existing, unrestricted decl-ending
    /// behavior in [`Self::run_block`]).
    async fn run_multi_item_block(
        &self,
        node: NodeId,
        block: &str,
        imports: &str,
        helpers: &str,
    ) -> Result<engine::TurnOutcome, HarnessError> {
        let items = engine::split_block_items(block);
        let contract = self.answer_contract(node);
        // See `run_block`'s identical read: the pinned row's probe-validate
        // must see the node's session decl-plane dir (and any live
        // `Val.G<g>` a decl module there may itself import) too, not just
        // this config's static include set.
        let session_bind = self.session_bind_context(node);
        let extra_session = session_bind.as_ref().map(|(_, inject_modules, root, _)| {
            tidepool_runtime::SessionInject {
                session_root: root.as_path(),
                inject_modules: inject_modules.as_slice(),
            }
        });
        let target = self.cfg.turn_target_with_extra_validation_include(
            contract
                .as_ref()
                .map(|c| (c.ty.as_str(), c.imports.as_slice())),
            extra_session,
        )?;

        // The user-import lines a decl item's compiled module needs re-glued
        // — the same rebuild `run_block`'s single-item decl path does
        // (`decl_source`), applied once per decl RUN below: a batch's
        // declarations land in one module, so the import block is needed
        // only once at the top of the run, not repeated per source.
        let decl_import_prefix: String = if imports.is_empty() {
            String::new()
        } else {
            imports
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| format!("import {l}\n"))
                .collect()
        };

        let template_started = std::time::Instant::now();
        let item_refs: Vec<&str> = items.iter().map(String::as_str).collect();
        let verdicts = classify_block(&item_refs)
            .map_err(|e| HarnessError::Compile(format!("batch classify failed: {e}")))?;
        timing::record_stage(
            node.0,
            timing::NO_ROUND,
            timing::STAGE_TEMPLATE,
            template_started.elapsed(),
            0,
        );
        if verdicts.len() != items.len() {
            return Err(HarnessError::Resident(format!(
                "batch classify returned {} verdict(s) for {} item(s)",
                verdicts.len(),
                items.len()
            )));
        }

        let scope = self.node_scope(node);
        let mut index = 0usize;
        let mut last_outcome: Option<engine::TurnOutcome> = None;
        // Every binder name any decl run in THIS block commits, in order —
        // reported back to the model in the eventual failure message (see
        // `engine::decl_salvage_note`) so a corrective never leaves it
        // guessing whether a declaration it wrote survived. Populated
        // whether or not the block ultimately fails; unused when it doesn't.
        let mut kept_decls: Vec<String> = Vec::new();
        // The FIRST compile-class failure this block hits, if any. Once set,
        // the walk below stops RUNNING anything further (a bind/expr item
        // past a failure never executes — nothing computed it against) but
        // keeps SCANNING for more declaration runs to salvage: any item in
        // the block that is individually a valid declaration commits to the
        // decl plane in order, regardless of where some other item failed.
        let mut pending_failure: Option<HarnessError> = None;

        while index < items.len() {
            if verdicts[index].kind == TurnKind::Decl {
                let start = index;
                while index < items.len() && verdicts[index].kind == TurnKind::Decl {
                    index += 1;
                }
                let mut decl_texts: Vec<String> = items[start..index].to_vec();
                if !decl_import_prefix.is_empty() {
                    decl_texts[0] = format!("{decl_import_prefix}\n{}", decl_texts[0]);
                }

                let checkout = self.checkout_run_retrying(node).await?;
                let res = self
                    .run_checked_out(node, checkout, move |mut session| {
                        let refs: Vec<&str> = decl_texts.iter().map(String::as_str).collect();
                        let r = session.define_scoped_in(scope, &refs);
                        (session, r)
                    })
                    .await?;
                self.flush_effects(node)?;
                let rendered = match res {
                    Ok(gen) => format!("declared (gen {})", gen.0),
                    // Same COMPILE-class treatment `run_block`'s single-item
                    // decl path gives a failed decl validation — the
                    // corrective-retry loop feeds it back as another round.
                    // An invalid decl is rejected on its OWN diagnostic, never
                    // salvaged — but the walk still continues past it so a
                    // LATER, independently valid declaration is not
                    // sacrificed to this one's mistake.
                    Err(e) => {
                        if pending_failure.is_none() {
                            pending_failure = Some(HarnessError::Compile(e.to_string()));
                        }
                        continue;
                    }
                };
                for v in &verdicts[start..index] {
                    kept_decls.extend(v.binders.iter().cloned());
                }
                if pending_failure.is_none() && index == items.len() {
                    if start == 0 {
                        // The WHOLE block was declarations — no earlier item
                        // in THIS round ran anything else. This is exactly a
                        // single-item decl turn's shape (`run_block`'s
                        // singleton `TurnResult::Decl` arm), just batched
                        // into one `define_scoped_in` generation: it commits
                        // and completes directly, same as that arm does
                        // (`node_done`, no corrective retry). Requiring a
                        // trailing answer expression from a turn that never
                        // claimed to answer anything desynced the
                        // corrective-retry loop's reply consumption —
                        // exactly the shape `companion_scope_trees.rs`'s
                        // `locked_decision_4_holds_through_the_real_compile_path`
                        // and `acceptance_cross_turn.rs`'s
                        // `multi_block_reply_runs_in_order_and_fails_with_resume_point`
                        // pin (a scripted declare-only reply must not eat an
                        // extra reply meant for the next turn).
                        self.tree.node_done(node, rendered.clone())?;
                        return Ok(engine::TurnOutcome::Completed { rendered });
                    }
                    // A decl run TRAILING after earlier content in this same
                    // block (`start > 0`) — every declaration in it,
                    // including this trailing run, is now COMMITTED to the
                    // node's decl plane (the `define_scoped_in` call above
                    // already ran), so a later round/turn in this same
                    // window sees it. The round itself still did not
                    // ADVANCE (no bind/expr ran as the block's terminal
                    // item, so nothing ever calls `finish_run(terminal:
                    // true)`), which is a MODEL-AUTHORED failure, not a
                    // mechanism one — `HarnessError::Compile` is what every
                    // driver retry ladder treats as "correct and go again"
                    // (the driver's corrective turn embeds this message
                    // verbatim), and what a branch position folds as a
                    // typed window exit when rounds run out. Returning
                    // `Resident` here escalated a stray trailing
                    // declaration into a turn-killing mechanism failure
                    // (dogfood crash, 2026-08-20 — the very first
                    // exploratory window of the interaction-surface run
                    // died on it). No `push_user_turn` here: the retry
                    // protocol is the DRIVER's, one corrective message per
                    // failed round, not two.
                    let msg = "This block's last item is a declaration, not something \
                               that runs. A multi-item block must end with the answer \
                               expression (or a bind) — move any trailing declaration \
                               earlier in the block, or follow it with the expression \
                               that uses it."
                        .to_string();
                    pending_failure = Some(HarnessError::Compile(msg));
                    continue;
                }
                last_outcome = Some(engine::TurnOutcome::Completed { rendered });
                continue;
            }

            if pending_failure.is_some() {
                // A real failure already happened earlier in this block —
                // nothing computed this item against, so it must not run.
                // Keep walking: a declaration further on is still salvaged.
                index += 1;
                continue;
            }

            // Singleton bind/expr item — fresh live context every iteration
            // (see `live_turn_context`'s doc).
            let ctx = self.live_turn_context(node, imports, helpers, &target.include)?;
            let item_text = items[index].clone();
            let verdict = verdicts[index].clone();
            let expr_source = engine::expr_turn_template(
                &self.cfg,
                &target.stack,
                &item_text,
                &ctx.expr_imports,
                helpers,
            );
            let templates = vec![
                TurnTemplate {
                    kind: TemplateSelector::Decl,
                    source: DECL_TEMPLATE_SOURCE.to_string(),
                },
                TurnTemplate {
                    kind: TemplateSelector::Bind,
                    source: ctx.bind_source.clone(),
                },
                TurnTemplate {
                    kind: TemplateSelector::BindDiscard,
                    source: ctx.binddiscard_source.clone(),
                },
                TurnTemplate {
                    kind: TemplateSelector::Expr,
                    source: expr_source.clone(),
                },
            ];

            let include = ctx.include.clone();
            let session_root = ctx.session_root.clone();
            let inject_modules = ctx.inject_modules.clone();
            let gen = ctx.gen;
            let req_item_text = item_text.clone();
            let req_verdict = verdict.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                let include_refs: Vec<&Path> = include.iter().map(PathBuf::as_path).collect();
                let req = TurnRequest {
                    turn_text: &req_item_text,
                    templates: &templates,
                    include: &include_refs,
                    session_root: &session_root,
                    inject_modules: &inject_modules,
                    gen,
                    verdict: Some(req_verdict),
                    target: None,
                };
                run_turn(req)
            })
            .await
            .map_err(|e| HarnessError::Resident(format!("turn compile task join: {e}")))?;
            let outcome = match outcome {
                Ok(o) => o,
                // A genuine compile failure on this item — the same
                // COMPILE-class treatment `run_block`'s single-item path
                // gives it, except this walk does not stop here: it keeps
                // scanning the rest of the block for declaration runs to
                // salvage (the `pending_failure.is_some()` guard above skips
                // running anything further, but a later decl item is still
                // individually valid and still committed).
                Err(e) => {
                    if pending_failure.is_none() {
                        pending_failure = Some(HarnessError::Compile(render_compile_error(
                            &e,
                            &item_text,
                            &expr_source,
                            &ctx.bind_source,
                        )));
                    }
                    index += 1;
                    continue;
                }
            };

            // Only the block's LAST item may finalize the node — an
            // intermediate bind/expr item completes its own step but must
            // leave the node `Running` for the items still to come (see
            // `finish_run`'s doc: getting this wrong finalizes the whole
            // node after item 1 of a multi-bind block).
            let terminal = index + 1 == items.len();
            let step_outcome = match outcome {
                TurnResult::Decl { .. } => {
                    return Err(HarnessError::Resident(
                        "internal: batch verdict said bind/expr but the compile returned a decl"
                            .into(),
                    ));
                }
                TurnResult::Bind {
                    binders,
                    bound,
                    compiled,
                    ..
                } if !binders.is_empty() => {
                    let Some(bind_gen) = ctx.bind_ctx_gen else {
                        return Err(HarnessError::Resident(
                            "value-plane bind requires a node decl plane".into(),
                        ));
                    };
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
                    self.run_bind_turn(node, binder, compiled, bind_gen, terminal)
                        .await?
                }
                TurnResult::Bind { compiled, .. } | TurnResult::Expr { compiled, .. } => {
                    self.log_turn_extracted(node, &asks_log_pairs(&compiled.asks), None)?;
                    let asks = AsksSidecar::from_entries(compiled.asks);
                    let table = compiled.table;
                    let expr = compiled.expr;
                    let checkout = self.checkout_run_retrying(node).await?;
                    let run_table = table.clone();
                    let run_outcome = self
                        .run_checked_out(node, checkout, move |mut session| {
                            let out = session.run("turn", &expr, &run_table);
                            (session, out)
                        })
                        .await?;
                    self.finish_run(node, run_outcome, table, asks, terminal)?
                }
            };

            if matches!(step_outcome, engine::TurnOutcome::Suspended { .. }) {
                if index + 1 < items.len() {
                    let is_finalize = matches!(
                        &step_outcome,
                        engine::TurnOutcome::Suspended { classified, .. }
                            if matches!(classified.routing, engine::SuspensionRouting::Finalize { .. })
                    );
                    if is_finalize {
                        tracing::warn!(
                            node = node.0,
                            unrun = items.len() - index - 1,
                            "finalize in item {} of {} — later items never run",
                            index + 1,
                            items.len()
                        );
                    } else {
                        self.push_user_turn(
                            node,
                            &format!(
                                "Note: item {} of {} in this block suspended awaiting an \
                                 answer, so the items after it did not run. Items 1–{} ran \
                                 and persist — when your window continues, pick up from \
                                 item {}.",
                                index + 1,
                                items.len(),
                                index + 1,
                                index + 2
                            ),
                        )?;
                    }
                }
                return Ok(step_outcome);
            }
            last_outcome = Some(step_outcome);
            index += 1;
        }

        // A failure anywhere in the block (a decl's own invalid compile, a
        // trailing-decl block, or a bind/expr item that failed to compile)
        // ends the round the same COMPILE-class way `run_block`'s
        // single-item path always has — but by now every individually valid
        // declaration in the block, wherever it sat relative to the
        // failure, is already committed to the decl plane. The corrective
        // says so by name rather than leaving the model to guess whether a
        // declaration it wrote survived.
        if let Some(err) = pending_failure {
            return Err(match err {
                HarnessError::Compile(msg) => HarnessError::Compile(format!(
                    "{}{msg}",
                    engine::decl_salvage_note(&kept_decls)
                )),
                other => other,
            });
        }

        // The loop above always sets `last_outcome` on every iteration that
        // did not hit a failure (decl-run or singleton) before advancing
        // `index`, and the trailing-decl precheck guarantees the FINAL
        // segment is a singleton — so a normal loop exit with no pending
        // failure always has one. A `None` here would mean `items` was
        // empty, which `run_block`'s `> 1` guard already rules out.
        #[allow(
            clippy::expect_used,
            reason = "run_multi_item_block: the loop always sets last_outcome when nothing failed"
        )]
        Ok(last_outcome.expect("run_multi_item_block: the loop always sets last_outcome"))
    }

    /// Shared turn epilogue: flush effects, and turn a [`ResidentOutcome`]
    /// into a [`engine::TurnOutcome`] — `node_done` on completion,
    /// [`Self::publish_suspension`] on suspension. The [`ResidentHole`] itself
    /// rides along on the published [`PendingSuspension`] so `resume_parent` can
    /// drive the ONE `ResidentSession::resume` later — a value-plane BIND
    /// turn that suspended (`x <- fork …`) already got a
    /// `ResidentHole::Binding` from `session.run_bind`, carrying its own
    /// binder/generation, so there is nothing extra to remember here beyond
    /// the hole. A completion needs no hole handling — `run_bind` already
    /// materialized it.
    /// `terminal` gates the `node_done` call on a `Completed` outcome: `true`
    /// for every caller except an INTERMEDIATE item of
    /// [`Self::run_multi_item_block`] (a bind/expr item that is not the
    /// block's last), which completes its own step but must leave the node
    /// `Running` for the items still to come. Getting this wrong finalizes
    /// the whole node after item 1 of a multi-bind block — the item loop
    /// mid-sequence dying with `TreeError::NotRunning(.., Done)` on item 2's
    /// next log/checkout call (caught by
    /// `multi_item_block_contiguous_binds_persist_across_rounds`, the exact
    /// shape a contiguous GHCi-style bind sequence now reaches once
    /// `split_block_items` stopped requiring a blank line between items).
    fn finish_run(
        &self,
        node: NodeId,
        outcome: Result<ResidentOutcome, ResidentError>,
        table: DataConTable,
        asks: AsksSidecar,
        terminal: bool,
    ) -> Result<engine::TurnOutcome, HarnessError> {
        self.flush_effects(node)?;

        match outcome {
            Ok(ResidentOutcome::Completed { result, .. }) => {
                let rendered = result.to_string_pretty();
                if terminal {
                    self.tree.node_done(node, rendered.clone())?;
                }
                Ok(engine::TurnOutcome::Completed { rendered })
            }
            Ok(ResidentOutcome::Suspended { hole, request, .. }) => {
                let classified = engine::classify_hole(&request, &table, &asks)?;
                let hole_id = hole.cont_id().to_string();
                let sid = self
                    .tree
                    .session_of(node)
                    .ok_or(HarnessError::NoSession(node))?;
                self.publish_suspension(
                    node,
                    sid,
                    PendingSuspension {
                        node,
                        hole: HoleId(hole_id.clone()),
                        classified: classified.clone(),
                        raw_request: request,
                        resident_hole: hole,
                        suspend_table: table,
                        suspend_asks: asks,
                    },
                )?;
                Ok(engine::TurnOutcome::Suspended {
                    hole: hole_id,
                    classified,
                })
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
        let scope = self.node_scope(node);
        self.tree.registry().peek(sid, |s| {
            let root = s.lib_include_dir()?;
            // Imports: the decl `Lib.G<g>` module + the CURRENT `Val.G<g>` module
            // of each live name (newest gen only — shadowed gens are injected,
            // not imported, to avoid an ambiguous occurrence). Injection
            // (`--inject-val`) uses ALL live gens.
            //
            // BOTH import lists are resolved FROM THE NODE'S SCOPE, not from
            // ROOT: the decl tip module a child imports already re-exports its
            // parent's chain (so parent declarations are callable here), and
            // the visible `Val.G<g>` set is the upward walk with child frames
            // shadowing parent ones (so a sibling's bindings are not even
            // nameable). At ROOT both are the pre-C2 lists verbatim.
            let mut import_lines: Vec<String> = Vec::new();
            if let Some(m) = s.session_import_module_in(scope) {
                import_lines.push(m);
            }
            import_lines.extend(s.current_val_modules_in(scope));
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
    /// bind suspends here and its value is materialized on resume — the
    /// `ResidentHole::Binding` `session.run_bind` mints on suspension already
    /// carries `binder`/`gen` forward, so `finish_run` needs nothing extra.
    async fn run_bind_turn(
        &self,
        node: NodeId,
        binder: BoundBinder,
        compiled: CompiledTurn,
        gen: Generation,
        terminal: bool,
    ) -> Result<engine::TurnOutcome, HarnessError> {
        self.log_turn_extracted(
            node,
            &asks_log_pairs(&compiled.asks),
            Some((&binder.name, &binder.type_display)),
        )?;
        let asks = AsksSidecar::from_entries(compiled.asks);
        let table = compiled.table;
        let expr = compiled.expr;

        let checkout = self.checkout_run_retrying(node).await?;
        let binder_for_run = binder.clone();
        let run_table = table.clone();
        let outcome = self
            .run_checked_out(node, checkout, move |mut session| {
                let out = session.run_bind("bind", &expr, &run_table, &binder_for_run, gen);
                (session, out)
            })
            .await?;

        self.finish_run(node, outcome, table, asks, terminal)
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
            if turns >= engine::DEFAULT_MAX_TURNS {
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
                        "Reply with ```haskell blocks to run (or answer the \
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
                        engine::DEFAULT_MAX_TURNS
                    );
                    let ghc = truncate_ghc_error(&msg);
                    self.push_user_turn(
                        node,
                        &format!(
                            "That Haskell did not compile. Fix it and reply with \
                             corrected ```haskell blocks. Common causes: a verb \
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
    pub fn pending_suspension(&self, node: NodeId) -> Option<ClassifiedSuspension> {
        self.node_pending(node).map(|p| p.classified)
    }

    /// Like [`Self::pending_suspension`], but also returns the hole id and the
    /// compile table the pending suspension's constructor ids resolve
    /// against — what a caller needs to build a
    /// [`engine::TurnOutcome::Suspended`] out of a `pending_suspension` read (the
    /// self-iterating-harness driver's `AskUser` servicing loop, which reads
    /// the pending hole again after a resume rather than threading the
    /// original `drive_turn` outcome through). `None` if `node` isn't
    /// suspended.
    pub(crate) fn pending_suspension_full(
        &self,
        node: NodeId,
    ) -> Option<(HoleId, ClassifiedSuspension, DataConTable)> {
        let p = self.node_pending(node)?;
        Some((p.hole, p.classified, p.suspend_table))
    }

    /// Like [`Self::pending_suspension_full`], plus the RAW suspended request
    /// `Value` — what a caller needs to dispatch a suspension whose payload
    /// `ClassifiedSuspension` doesn't carry ([`SuspensionRouting::Subagent`]
    /// is a unit variant — no spec/schema/cycle id — because the OUTER
    /// loop's own equivalent servicing reads those off the original request
    /// it never discards; a nested-answerer node discards it once
    /// `classify_hole` runs UNLESS a caller reaches for this accessor first,
    /// same `raw_request` [`PendingSuspension`] already stores for
    /// [`Self::take_finalized_value`]).
    pub(crate) fn pending_suspension_with_request(
        &self,
        node: NodeId,
    ) -> Option<(HoleId, ClassifiedSuspension, DataConTable, Value)> {
        let p = self.node_pending(node)?;
        Some((p.hole, p.classified, p.suspend_table, p.raw_request))
    }

    /// The FULL pending record — [`Self::pending_suspension_with_request`] plus the
    /// asks sidecar the suspension's site ids resolve against. What the
    /// answerer-plane green scheduler
    /// (`SelfHarnessDriver::service_green_round`) needs: within one round
    /// every chain (the node's own turn and every green thread it spawned)
    /// shares ONE compile, so the table+asks captured off the node's pending
    /// record classify every thread-raised suspension of that round too.
    #[allow(clippy::type_complexity)]
    pub(crate) fn pending_suspend_artifacts(
        &self,
        node: NodeId,
    ) -> Option<(
        HoleId,
        ClassifiedSuspension,
        DataConTable,
        AsksSidecar,
        Value,
    )> {
        let p = self.node_pending(node)?;
        Some((
            p.hole,
            p.classified,
            p.suspend_table,
            p.suspend_asks,
            p.raw_request,
        ))
    }

    /// Resume `node`'s parked continuation with a RAW `Value` answer,
    /// bypassing [`Self::answer_dialog`]'s `Ask`/`AskUser`/`ReadState`
    /// routing restriction — for a suspension whose answer is already a
    /// bridged Core `Value` rather than operator-submitted JSON
    /// ([`SuspensionRouting::Subagent`], serviced the same way the AUTHORED outer
    /// loop's own Subagent suspension already is —
    /// `SelfHarnessDriver::service_outer_subagent`'s dispatch, just resumed
    /// against a NODE's own session instead of the outer one).
    pub(crate) async fn resume_with_value(
        &self,
        node: NodeId,
        hole: &HoleId,
        value: Value,
    ) -> Result<(), HarnessError> {
        let _lease = self.acquire_turn_lease(node)?;
        self.resume_parent(node, hole, value).await
    }

    /// [`Self::resume_with_value`]'s borrowed-root sibling: deliver a green
    /// thread's session-owned heap result to `node`'s own parked turn
    /// (answerer-plane `wait` on a thread whose value settled by handle),
    /// keeping the node-aware re-publish bookkeeping [`Self::resume_parent`]
    /// does. The root stays owned by the session realm — this borrows it, the
    /// same delivery `ResidentSession::resume_handle_borrowed` performs on
    /// the outer plane's raw path.
    pub(crate) async fn resume_with_borrowed_root(
        &self,
        node: NodeId,
        hole: &HoleId,
        handle: tidepool_codegen::suspension::ValueHandle,
    ) -> Result<(), HarnessError> {
        let _lease = self.acquire_turn_lease(node)?;
        self.resume_parent_input(node, hole, ResumeParentInput::BorrowedRoot(handle))
            .await
    }

    /// Reconstruct a [`TurnOutcome::Suspended`] from `node`'s CURRENT pending
    /// hole (self-iterating-harness fork widen: after a fork/fanout resumes
    /// a parent answerer, the driver needs the parent's freshly
    /// re-published hole in the same shape [`Self::drive_turn`] returns,
    /// without re-running a model turn). `None` if `node` isn't currently
    /// suspended.
    pub fn pending_turn_outcome(&self, node: NodeId) -> Option<TurnOutcome> {
        let p = self.node_pending(node)?;
        Some(TurnOutcome::Suspended {
            hole: p.hole.0,
            classified: p.classified,
        })
    }

    /// The harness-level primitive `service_typed_request_suspension`
    /// (`selfharness/driver.rs`) calls once a nested Agent node
    /// suspends on `finalize @T x`: read the
    /// finalized value straight out of the suspended request `Value` (NEVER
    /// through JSON — it may carry a closure or other non-serializable
    /// value, per `finalize`'s relaxed function-arrow rule) and terminate
    /// the node.
    ///
    /// `finalize` does NOT resume the Agent (unlike answering a
    /// `RunLLMTurn`/`Fork` hole in context) — it
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
        let sid = self
            .tree
            .session_of(node)
            .ok_or(HarnessError::NoSession(node))?;
        let pending = self
            .node_pending(node)
            .ok_or(HarnessError::NotSuspended(node))?;
        if !matches!(
            pending.classified.routing,
            SuspensionRouting::Finalize { .. }
        ) {
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
        self.pending_suspensions.lock().remove(&(sid, pending.hole));
        Ok((value, pending.suspend_table))
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
        let hole = self
            .node_pending(node)
            .ok_or(HarnessError::NotSuspended(node))?
            .hole;
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
        // Restore with the session's OWN reported hole set — the aborted
        // finalize hole is gone, but any OTHER parked holes (multi-hole
        // sessions) must survive; a bare restore_idle would desync
        // the slot from the machine's still-rooted frames.
        let holes: Vec<HoleId> = co
            .machine()
            .parked_holes()
            .into_iter()
            .map(|h| HoleId(h.to_string()))
            .collect();
        co.restore_suspended(holes);

        // Tree state: Suspended → Running, so the reused node accepts a new turn.
        self.tree.hole_consumed(node, hole)?;
        Ok((value, rendered))
    }

    /// Closure sibling of [`Self::take_finalized_value_keep_open`]:
    /// the finalize payload is a live closure, so
    /// instead of bridging a data `Value` (which would sentinel it), MINT a
    /// [`ValueHandle`] over the parked frame's payload, then consume the
    /// finalize hole exactly like the value path (abort the frame — the
    /// handle owns the payload root now — restore with the surviving hole
    /// set, `hole_consumed` the tree). The caller delivers the handle into
    /// the awaiting hole via `ResidentSession::resume_handle`. The node and
    /// its session stay live and reusable.
    pub(crate) fn take_finalized_handle_keep_open(
        &self,
        node: NodeId,
    ) -> Result<tidepool_runtime::session::RootCustody, HarnessError> {
        let sid = self
            .tree
            .session_of(node)
            .ok_or(HarnessError::NoSession(node))?;
        let pending = self
            .node_pending(node)
            .ok_or(HarnessError::NotSuspended(node))?;
        if !matches!(
            pending.classified.routing,
            SuspensionRouting::Finalize { .. }
        ) {
            return Err(HarnessError::RoutingMismatch {
                node,
                routing: "Finalize",
                actual: format!("{:?}", pending.classified.routing),
            });
        }
        let hole = pending.hole;
        let mut co = self.checkout_resume(node, &hole)?;
        let handle = co.machine().finalized_handle(&hole.0);
        let handle = match handle {
            Some(h) => h,
            None => {
                // Restore before erroring — the frame is untouched.
                let holes: Vec<HoleId> = co
                    .machine()
                    .parked_holes()
                    .into_iter()
                    .map(|h| HoleId(h.to_string()))
                    .collect();
                co.restore_suspended(holes);
                return Err(HarnessError::Resident(
                    "finalize hole carries no untaken closure payload".into(),
                ));
            }
        };
        let _ = co
            .machine()
            .abort(&hole.0, "finalize consumed (answerer reused)".to_string());
        let holes: Vec<HoleId> = co
            .machine()
            .parked_holes()
            .into_iter()
            .map(|h| HoleId(h.to_string()))
            .collect();
        co.restore_suspended(holes);
        // Clear the consumed pending hole (the value path's
        // take_finalized_value_core does this; the handle path must too).
        self.pending_suspensions.lock().remove(&(sid, hole.clone()));
        self.tree.hole_consumed(node, hole)?;
        Ok(handle)
    }

    /// REFUSE `node`'s pending hole: abort the parked continuation (the
    /// block's suspended computation dies; the WINDOW survives), clear the
    /// hole's bookkeeping, and return the node to `Running` so the caller
    /// can push a corrective turn — the fork-budget guard's teeth
    /// (`SelfHarnessDriver`'s answerer dispatcher is the caller). A refusal
    /// cannot resume the hole instead: a `fork @T` hole's continuation
    /// expects a `T`, and there is no honest `T` to fabricate. The session's
    /// OTHER parked frames (a green thread's) survive via the same
    /// restore-with-reported-holes discipline the finalize-consume paths
    /// use.
    pub(crate) fn refuse_pending_suspension(
        &self,
        node: NodeId,
        reason: String,
    ) -> Result<(), HarnessError> {
        let sid = self
            .tree
            .session_of(node)
            .ok_or(HarnessError::NoSession(node))?;
        let pending = self
            .node_pending(node)
            .ok_or(HarnessError::NotSuspended(node))?;
        let hole = pending.hole;
        let mut co = self.checkout_resume(node, &hole)?;
        // The abort's Err outcome IS the expected "continuation discarded"
        // signal (see `take_finalized_value_keep_open`), not a failure.
        let _ = co.machine().abort(&hole.0, reason);
        let holes: Vec<HoleId> = co
            .machine()
            .parked_holes()
            .into_iter()
            .map(|h| HoleId(h.to_string()))
            .collect();
        co.restore_suspended(holes);
        self.pending_suspensions.lock().remove(&(sid, hole.clone()));
        self.tree.hole_consumed(node, hole)?;
        Ok(())
    }

    /// Whether `node`'s pending finalize hole carries a CLOSURE value: the
    /// tolerant suspend bridge substituted a
    /// `CLOSURE_SENTINEL` placeholder for field 1, so the finalized value is a
    /// live closure kept in-heap (applied by reference), not data. `false` for a
    /// plain-data finalize, or when `node` isn't suspended on a finalize hole.
    pub fn finalize_is_closure(&self, node: NodeId) -> bool {
        let Some(pending) = self.node_pending(node) else {
            return false;
        };
        if !matches!(
            pending.classified.routing,
            SuspensionRouting::Finalize { .. }
        ) {
            return false;
        }
        // DEEP scan via the one exported predicate (mirrors the machine's
        // request_carries_closure_sentinel, codex review 2026-08-12, finding
        // 3): a closure nested inside the finalized product — a record of
        // functions — must route through the handle-delivery path exactly
        // like a top-level closure.
        tidepool_codegen::heap_bridge::field_contains_closure_sentinel(&pending.raw_request, 1)
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
            self.pending_suspension(node).map(|c| c.routing),
            Some(SuspensionRouting::Finalize { .. })
        );
        if !is_finalize {
            return Err(HarnessError::RoutingMismatch {
                node,
                routing: "Finalize",
                actual: format!("{:?}", self.pending_suspension(node).map(|c| c.routing)),
            });
        }
        // The suspend turn's table, passed through so the apply fragment's
        // compile merges the closure's own defining constructors into the
        // accumulated session table (see `ResidentSession::apply_finalized`'s
        // doc for why no `I#`-id matching is needed here).
        let suspend_table = self.node_pending(node).map(|p| p.suspend_table);
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

    /// The most recently compiled turn's extracted Haskell on `node`
    /// (overwritten per compiled turn) — what the self-iterating harness
    /// driver posts to the operator GUI's last-turn-source pane
    /// (`OperatorGate::post_turn_source`). `None` before the node's first
    /// compiled turn or if `node` has no live session.
    pub fn last_turn_source(&self, node: NodeId) -> Option<String> {
        self.convos
            .lock()
            .get(&node)
            .and_then(|c| c.last_turn_source.clone())
    }

    /// The MOST RECENT turn's `input_tokens` on `node` — the node's real
    /// current context size (the provider re-sends the whole transcript each
    /// round, so its per-round input count already includes every prior turn).
    /// This is a HIGH-WATER mark, not a running sum: the self-iterating
    /// harness's compaction threshold reads THIS. `Some(0)` before the
    /// node's first turn; `None` if `node` has no live session.
    pub fn node_last_input_tokens(&self, node: NodeId) -> Option<u64> {
        self.convos.lock().get(&node).map(|c| c.last_input_tokens)
    }

    /// Answer an operator hole — `askUser` ([`SuspensionRouting::AskUser`]) or a
    /// plain `ask` ([`SuspensionRouting::Ask`]) — with the operator's submission.
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
            .node_pending(node)
            .ok_or(HarnessError::NotSuspended(node))?;
        match &pending.classified.routing {
            SuspensionRouting::Ask { .. }
            | SuspensionRouting::AskUser { .. }
            | SuspensionRouting::ReadState => {}
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
        let table = pending.suspend_table.clone();
        let value = engine::json_answer_to_value(&submission, &table)?;
        // `resume_parent` logs the Consumed attempt itself, exactly once,
        // only after the resume actually succeeds — the single source of
        // truth for the Consumed record; this call site must not log again.
        self.resume_parent(node, &pending.hole, value).await?;
        Ok(())
    }

    /// Resume a `note` hole ([`SuspensionRouting::Note`]) immediately with `()` —
    /// no operator interaction. Mirrors [`Self::answer_dialog`]'s shape
    /// (lease, routing check, `resume_parent`) — the same audited resume
    /// path a mechanical dialog answer uses — but the resumed value is the
    /// REAL Core `()` ([`tidepool_bridge::ToCore`] for `()`), not the
    /// aeson-wire `Value` `answer_dialog`'s submission bridges to:
    /// `NoteWith`'s continuation is `() -> M ()`, not `Value -> M Value`, so
    /// routing it through `json_answer_to_value`'s aeson-`Null` bridge would
    /// hand the continuation the wrong constructor.
    pub async fn answer_note(&self, node: NodeId) -> Result<(), HarnessError> {
        let _lease = self.acquire_turn_lease(node)?;
        let pending = self
            .node_pending(node)
            .ok_or(HarnessError::NotSuspended(node))?;
        match &pending.classified.routing {
            SuspensionRouting::Note { .. } => {}
            other => {
                return Err(HarnessError::RoutingMismatch {
                    node,
                    routing: "note",
                    actual: format!("{other:?}"),
                })
            }
        }

        let table = pending.suspend_table.clone();
        use tidepool_bridge::ToCore;
        let value = ()
            .to_value(&table)
            .map_err(|e| EngineError::Run(format!("bridge unit answer to Value: {e}")))?;
        self.resume_parent(node, &pending.hole, value).await?;
        Ok(())
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
        self.resume_parent_input(node, hole, ResumeParentInput::Answer(answer))
            .await
    }

    /// [`Self::resume_parent`] generalized over WHAT crosses the hole: a
    /// bridged `Value`, or a BORROWED session-owned heap root (a green
    /// thread's settled closure/handle result delivered to the node's own
    /// turn — the same borrow `ResidentSession::resume_handle_borrowed`
    /// performs on the raw path, here with the node-aware re-publish
    /// bookkeeping kept intact).
    ///
    /// A borrowed-root resume is refused on a [`ResidentHole::Binding`] hole:
    /// the raw `resume_handle_borrowed` seam carries no binder/generation
    /// obligation, so honoring it here would silently drop the binding the
    /// hole owes. Reachable from the answerer green lane (F9): a bind-shaped
    /// turn (`h <- async (…closure-valued…); wait h`) carries the Binding
    /// obligation through every resume of that continuation, so a settled
    /// closure-valued thread result delivered here lands on exactly this
    /// hole shape. The typed [`HarnessError::BorrowedRootOnBindingHole`]
    /// this returns is what lets the answerer-plane scheduler route it to
    /// the model as a corrective instead of hard-failing the run.
    async fn resume_parent_input(
        &self,
        node: NodeId,
        hole: &HoleId,
        input: ResumeParentInput,
    ) -> Result<(), HarnessError> {
        let sid = self
            .tree
            .session_of(node)
            .ok_or(HarnessError::NoSession(node))?;
        // `Session::resume` continues the ALREADY-COMPILED fragment the node
        // suspended with (it does not recompile), so any hole reached further
        // down that same continuation — including a second sequential ask —
        // resolves its site-id -> type against this SAME table, exactly like
        // `run_block` resolves the first hole's. Snapshot it now (session is
        // about to be taken out) so the re-suspend arm below can classify the
        // next hole with real site/type instead of publishing `None`/`None`.
        let pending = self.node_pending(node).ok_or_else(|| {
            HarnessError::Resident(format!(
                "node {node:?}: no resident hole stashed for pending continuation {hole:?}"
            ))
        })?;
        let (table, asks, resident_hole) = (
            pending.suspend_table,
            pending.suspend_asks,
            pending.resident_hole,
        );

        if matches!(input, ResumeParentInput::BorrowedRoot(_))
            && !matches!(resident_hole, ResidentHole::Plain(_))
        {
            return Err(HarnessError::BorrowedRootOnBindingHole(node));
        }
        let checkout = self.checkout_resume(node, hole)?;
        // ONE consuming resume: `resident_hole` already carries its own
        // completion obligation (`ResidentHole::Binding` materializes on
        // completion; `ResidentHole::Plain` needs nothing extra) — no
        // external flag to pick a method by.
        let outcome = self
            .run_checked_out(node, checkout, move |mut session| {
                let out = match input {
                    ResumeParentInput::Answer(answer) => session.resume(resident_hole, answer),
                    ResumeParentInput::BorrowedRoot(h) => {
                        session.resume_handle_borrowed(resident_hole.cont_id(), h)
                    }
                };
                (session, out)
            })
            .await?;
        self.flush_effects(node)?;

        // Only log the hole as Consumed (and clear its domain metadata) once
        // the resume has ACTUALLY SUCCEEDED — a fault here (e.g. `error`
        // forced mid-resume) must not leave a durable Consumed record with no
        // matching NodeDone or re-HolePublished.
        let outcome = outcome.map_err(|e| HarnessError::Resident(e.to_string()))?;

        self.log_answer_attempt(node, hole, "harness", AnswerOutcome::Consumed)?;
        self.consume_suspension(node, sid, hole)?;

        match outcome {
            ResidentOutcome::Completed { result, .. } => {
                let rendered = result.to_string_pretty();
                self.tree.node_done(node, rendered)?;
                // Keep the session ALIVE past completion (don't `terminate_node`),
                // same as `run_block`: a node that suspended on a hole and then
                // resumed to Done is still a followable conversation — its
                // persisted session lets `follow_up` reopen it with heap intact.
                Ok(())
            }
            ResidentOutcome::Suspended {
                hole: fresh_hole,
                request,
                ..
            } => {
                // The resumed turn hit ANOTHER hole. Re-classify against the
                // table snapshotted above (the compile the still-executing
                // fragment was built with) and re-publish with its REAL
                // site + type, same as a first-suspend `run_block` hole.
                let classified = engine::classify_hole(&request, &table, &asks)?;
                let hole_id = fresh_hole.cont_id().to_string();
                self.publish_suspension(
                    node,
                    sid,
                    PendingSuspension {
                        node,
                        hole: HoleId(hole_id),
                        classified,
                        raw_request: request,
                        resident_hole: fresh_hole,
                        suspend_table: table,
                        suspend_asks: asks,
                    },
                )?;
                Ok(())
            }
        }
    }

    /// Register a fork/fanout child under `parent` with the OPENING CARD
    /// supplied by the caller — the selfharness driver's window-pump fork
    /// path (fork-subsumes-split step 1) builds
    /// `engine::finalize_typed_request_prompt` (multi-round teaching:
    /// explore/define rounds, `finalize @T` as the answer verb). Same
    /// seeding seam ([`Self::seed_forked_child`]) every forked child goes
    /// through.
    pub(crate) fn register_fork_child_with_card(
        &self,
        parent: NodeId,
        title: &str,
        card: String,
    ) -> Result<NodeId, HarnessError> {
        let (parent_transcript, parent_framing) = {
            let convos = self.convos.lock();
            let convo = convos.get(&parent).ok_or(HarnessError::NoSession(parent))?;
            (convo.transcript.clone(), convo.framing.clone())
        };
        self.seed_forked_child(parent, title, parent_transcript, parent_framing, card)
    }

    /// Mint a THUNK child under `parent` seeded with `prefix` (its inherited
    /// context) + `opening` (its own first user turn), and emit `TurnForked`
    /// at the checkpoint `prefix` ends at.
    ///
    /// The ONE place a [`NodeSeed::Forked`] is staged — every forked child
    /// (whose opening is a hole card, [`Self::register_fork_child_with_card`])
    /// goes through here, so nothing about how a forked child is created,
    /// referenced, or later seeded at force time can diverge.
    ///
    /// The checkpoint is the inherited prefix's LENGTH (a durable transcript
    /// position), not the assistant-only `turn_seq` counter — the child
    /// carries exactly that prefix, so the two agree by construction, and the
    /// child's assembled request is byte-identical to the parent's through it.
    fn seed_forked_child(
        &self,
        parent: NodeId,
        title: &str,
        prefix: Vec<Message>,
        framing: Option<String>,
        opening: String,
    ) -> Result<NodeId, HarnessError> {
        let checkpoint = prefix.len() as u64;
        let child = self
            .tree
            .create_node(Some(parent), title, self.cfg.effect_names.clone())?;
        self.tree.turn_forked(child, parent, checkpoint)?;
        let mut transcript = prefix;
        transcript.push(Message {
            role: Role::User,
            content: opening,
            reasoning_items: Vec::new(),
        });
        self.pending.lock().insert(
            child,
            NodeSeed::Forked {
                transcript,
                framing,
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
        let scope = self.node_scope(node);
        self.tree
            .registry()
            .peek(sid, |s| {
                (s.session_import_module_in(scope), s.lib_include_dir())
            })
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

    /// `node`'s current [`AnswerContract`], if one is set. `pub(crate)`: the
    /// self-iterating harness driver reads it back (`types_in_scope_hint`) to
    /// report which modules a pinned turn actually imports, rather than
    /// keeping a second copy of the same information.
    pub(crate) fn answer_contract(&self, node: NodeId) -> Option<AnswerContract> {
        let convos = self.convos.lock();
        convos.get(&node).and_then(|c| c.answer_contract.clone())
    }

    /// The defining modules `asks.json` reported for `site`'s answer type,
    /// per `node`'s currently suspended turn's own asks sidecar
    /// ([`NodeConvo::suspend_asks`]) — the extract-side module lookup a
    /// fork/fanout child's `finalize` pins its imports from, replacing the
    /// harness-import-scraping guess. Empty when `node` has no live convo
    /// or `site` has no sidecar entry.
    pub(crate) fn asks_modules(&self, node: NodeId, site: u32) -> Vec<String> {
        self.node_pending(node)
            .map(|p| p.suspend_asks.modules_of(site).to_vec())
            .unwrap_or_default()
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

    /// How long [`Self::checkout_run_retrying`] backs off between checkout
    /// attempts while contested by a sibling realm.
    const CONTENTION_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(3);

    /// Bound on contention retries, as an ELAPSED-TIME budget (~2 minutes)
    /// rather than an iteration count — a fanout child contending behind
    /// several siblings' multi-second JIT turns across rounds can
    /// legitimately need to wait longer than a fixed attempt count assuming
    /// zero-cost checkouts would allow — so a genuinely wedged machine still
    /// fails loud, just bounded by wall-clock time instead.
    const CONTENTION_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

    /// [`Self::checkout_run`], but — ONLY when `node` opted in via
    /// [`Self::set_retry_checkout_on_contention`] — retries with a short
    /// backoff instead of failing fast on [`HarnessError::TurnInFlight`].
    ///
    /// This is the ONE place [`Self::run_block`] checks a machine out, so it
    /// is where a concurrently-driven sibling realm's checkout race against
    /// this SAME shared session gets resolved by WAITING rather than
    /// erroring: unlike retrying [`Self::drive_turn`] as a
    /// whole (NOT safe — it has already called the provider and appended
    /// the assistant reply to the transcript by the time a checkout could
    /// contend), retrying just this checkout is safe because nothing
    /// observable has happened yet at this point — `f` (the actual resident
    /// call) is invoked at most once, only after a checkout succeeds.
    ///
    /// A node that never opts in (every existing caller) gets EXACTLY
    /// [`Self::checkout_run`]'s behavior — fail fast, no retry — so
    /// `tests/turn_lease.rs`'s fail-fast contract is unchanged.
    async fn checkout_run_retrying(
        &self,
        node: NodeId,
    ) -> Result<Checkout<'_, Session>, HarnessError> {
        let retry = self
            .convos
            .lock()
            .get(&node)
            .map(|c| c.retry_checkout_on_contention)
            .unwrap_or(false);
        if !retry {
            return self.checkout_run(node);
        }
        let deadline = tokio::time::Instant::now() + Self::CONTENTION_RETRY_BUDGET;
        loop {
            match self.checkout_run(node) {
                Err(HarnessError::TurnInFlight(_)) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Self::CONTENTION_RETRY_BACKOFF).await;
                }
                other => return other,
            }
        }
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

    /// Check `node`'s machine out for a CHILD run over its parked frames
    /// (`Suspended{holes} -> Running{holes}`) — the `run_child` discipline:
    /// an answer value crosses via a child run against the suspended
    /// TARGET's own session, never consuming its parked continuations
    /// (which stay rooted in the machine's registry throughout).
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
    /// restore it based on the machine's OWN post-call state — the session's
    /// full reported hole SET (`parked_holes()`), never a guess from `f`'s
    /// domain result — correct whether the resident call completed,
    /// suspended, parked additional holes, or errored (an errored
    /// `run`/`resume` still leaves the session in a well-defined parked
    /// state; `run_child` never changes the target's parked holes either
    /// way).
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
        // Apply the node's realm to the session before the turn — ONE site
        // covering every run/resume/child path, so an attached answerer
        // node's parks are always owned by ITS realm on the shared machine,
        // and a node WITHOUT a realm parks under the reserved outer realm
        // (never whatever ambient realm the last turn left behind — codex
        // review 2026-08-12, High 2). Queued realm closes for this session
        // drain first, while the machine is in hand.
        let (realm, scope) = {
            let convos = self.convos.lock();
            let c = convos.get(&node);
            (
                c.and_then(|c| c.realm).unwrap_or(OUTER_REALM),
                c.and_then(|c| c.scope).unwrap_or(ScopeId::ROOT),
            )
        };
        let sid = checkout.session_id();
        let mut machine = checkout.take();
        self.drain_pending_session_exits(sid, &mut machine);
        match tokio::task::spawn_blocking(move || {
            let mut machine = machine;
            machine.set_realm(realm);
            // The NAME-side half of the same "this window's turn" statement:
            // a node without a scope runs at ROOT, never at whatever scope the
            // last turn on this shared machine left behind (the same ambient-
            // stickiness hazard the realm reset above answers).
            //
            // A dead `scope` here means the node's own recorded scope was
            // retired out from under it (e.g. a queued window exit for this
            // node drained just above, in `drain_pending_session_exits`) — a
            // harness invariant violation, not a normal path. Force back to
            // ROOT rather than let `set_scope` silently no-op and leave
            // whatever scope the machine was last left at (the exact
            // ambient-stickiness hazard this reset exists to prevent).
            if let Err(e) = machine.set_scope(scope) {
                tracing::error!(
                    "run_checked_out: node {node:?}'s recorded scope {scope:?} is dead ({e}); \
                     falling back to ROOT"
                );
                #[allow(clippy::expect_used, reason = "ScopeId::ROOT is always live")]
                machine
                    .set_scope(ScopeId::ROOT)
                    .expect("ScopeId::ROOT is always live");
            }
            f(machine)
        })
        .await
        {
            Ok((session, result)) => {
                let holes: Vec<HoleId> = session
                    .parked_holes()
                    .into_iter()
                    .map(|h| HoleId(h.to_string()))
                    .collect();
                checkout.put(session);
                // One restore, two spellings: an empty set IS Idle.
                checkout.restore_suspended(holes);
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
    /// turn `JoinError`, the self-iterating harness's `retire_typed_request_agent`)
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
            if self.tree.node_owns_session(node) {
                self.tree.registry().remove(sid);
            } else {
                // An ATTACHED node: its retirement is
                // its WINDOW's exit on the shared machine, never slot removal
                // — the outer session outlives every answerer node it hosts.
                // Two halves, retired together in `exit_agent_session`: the REALM
                // (parked frames + outstanding handles) and the node's SCOPE
                // (its value-plane frame, and the GC roots that frame solely
                // owns). A window's names and its heap roots
                // have one lifetime, so there is one retirement step, not two.
                //
                // The exit is an EVENTUAL POSTCONDITION, not a best-effort side
                // effect: if the machine is out on a turn right now, it is
                // queued (`pending_session_exits`) and applied by the next code
                // path that has the machine in hand (`run_checked_out`/
                // `with_session` drain the queue before restoring) — the realm
                // and scope identities are retained until the exit is
                // CONFIRMED, never dropped with the convo.
                let (realm, scope) = {
                    let convos = self.convos.lock();
                    let c = convos.get(&node);
                    (c.and_then(|c| c.realm), c.and_then(|c| c.scope))
                };
                if realm.is_some() || scope.is_some_and(|s| !s.is_root()) {
                    match self.tree.registry().checkout_run(sid) {
                        Ok(mut co) => {
                            self.exit_agent_session(co.machine(), node, realm, scope);
                            let holes: Vec<HoleId> = co
                                .machine()
                                .parked_holes()
                                .into_iter()
                                .map(|h| HoleId(h.to_string()))
                                .collect();
                            co.restore_suspended(holes);
                        }
                        Err(_) => {
                            self.pending_session_exits.lock().push(PendingSessionExit {
                                session: sid,
                                node,
                                realm,
                                scope,
                            });
                        }
                    }
                }
            }
        }
        self.convos.lock().remove(&node);
        // A node that terminates while still holding a pending hole (a
        // cancellation, or a failed fork/fanout child cleanup) would
        // otherwise leave an orphaned entry in `pending_suspensions` forever — no
        // caller can ever consume it once the node itself is gone.
        self.pending_suspensions
            .lock()
            .retain(|_, p| p.get().node != node);
        Ok(())
    }

    /// Assign `node`'s realm — every subsequent turn this node runs on its
    /// session parks under it (set into the session at run time, inside the
    /// checkout). The driver mints one realm per answerer node.
    pub fn set_node_realm(&self, node: NodeId, realm: tidepool_codegen::suspension::RealmId) {
        let mut convos = self.convos.lock();
        if let Some(convo) = convos.get_mut(&node) {
            convo.realm = Some(realm);
        }
    }

    /// Assign `node`'s SCOPE — every subsequent turn it runs compiles against
    /// that scope's decl tip and the value bindings visible from it, and binds
    /// into that scope's own frame (applied inside the checkout by
    /// [`Self::run_checked_out`], the same one site the realm is applied at).
    /// [`Self::terminate_node`] retires it.
    ///
    /// Mint the scope off the session first
    /// (`with_session(sid, |s| s.mint_scope(parent))`) — this only records
    /// which scope the node's window lives in. A node that is never given one
    /// stays at [`ScopeId::ROOT`], the flat session.
    pub fn set_node_scope(&self, node: NodeId, scope: ScopeId) {
        let mut convos = self.convos.lock();
        if let Some(convo) = convos.get_mut(&node) {
            convo.scope = Some(scope);
        }
    }

    /// `node`'s scope — [`ScopeId::ROOT`] for a node that was never given one
    /// (every pre-C2 node) and for an unknown node.
    pub fn node_scope(&self, node: NodeId) -> ScopeId {
        self.convos
            .lock()
            .get(&node)
            .and_then(|c| c.scope)
            .unwrap_or(ScopeId::ROOT)
    }

    /// Opt `node` into retrying (rather than failing fast) a
    /// [`Self::run_block`] checkout contested by [`HarnessError::TurnInFlight`]
    /// — see [`NodeConvo::retry_checkout_on_contention`]'s doc for the exact
    /// contract and why this is safe to enable ONLY for a node whose
    /// contention is a concurrently-driven sibling realm on the SAME shared
    /// session. `false` by default; every existing caller
    /// (which never calls this) keeps today's fail-fast behavior unchanged.
    pub fn set_retry_checkout_on_contention(&self, node: NodeId, retry: bool) {
        let mut convos = self.convos.lock();
        if let Some(convo) = convos.get_mut(&node) {
            convo.retry_checkout_on_contention = retry;
        }
    }

    /// Replace `node`'s transcript with a single summary message IN PLACE,
    /// keeping the resident session, per-node framing (`render`'s output), and
    /// turn-sequence continuity live — the self-iterating harness's MID-LOOP
    /// in-place compaction relief: replace the context with the summary so
    /// the loop CONTINUES, never a loop-abort. The
    /// accumulated exchange is collapsed to one User-role message carrying
    /// `summary` as prior-window context; the node's `last_input_tokens` high-
    /// water mark is reset (the driver's threshold check does not immediately
    /// re-fire). The next hole (or the current hole's next round) drives on
    /// under the smaller context.
    pub fn replace_transcript_with_summary(
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
        // compacted window, not the pre-compaction cumulative total — the
        // next turn's input_tokens re-establishes the real size.
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

    /// OQ4: `HarnessError::from_checkout` must preserve `CheckoutError::
    /// WrongHole`'s structured `{session, attempted, parked}` data rather
    /// than flattening it into a display string — a caller matching on
    /// `HarnessError::SessionMismatch { source: CheckoutError::WrongHole {
    /// attempted, parked, .. }, .. }` must be able to read the actual
    /// pending hole set back out, the same way `tidepool-repl`'s
    /// `SessionManager::suspension_for` already lets a caller read `Err(Some(pending))`
    /// without a second lookup.
    #[test]
    fn from_checkout_preserves_wrong_hole_structured_data() {
        let node = NodeId(7);
        let session = SessionId(1);
        let attempted = HoleId("scont_9".to_string());
        let parked = vec![HoleId("scont_1".to_string()), HoleId("scont_2".to_string())];
        let err = CheckoutError::WrongHole {
            session,
            attempted: attempted.clone(),
            parked: parked.clone(),
        };

        match HarnessError::from_checkout(node, err) {
            HarnessError::SessionMismatch {
                node: got_node,
                source:
                    CheckoutError::WrongHole {
                        session: got_session,
                        attempted: got_attempted,
                        parked: got_parked,
                    },
            } => {
                assert_eq!(got_node, node);
                assert_eq!(got_session, session);
                assert_eq!(got_attempted, attempted);
                assert_eq!(got_parked, parked);
            }
            other => panic!("expected SessionMismatch{{source: WrongHole}}, got {other:?}"),
        }
    }

    /// The other three checkout refusals `from_checkout` folds into
    /// `SessionMismatch` (`NotSuspended`/`NoSession`/`Terminal`) also survive
    /// as their own structured `CheckoutError` variant, not a shared string.
    #[test]
    fn from_checkout_preserves_not_suspended_and_terminal_variants() {
        let node = NodeId(8);
        let session = SessionId(2);

        match HarnessError::from_checkout(node, CheckoutError::NotSuspended(session)) {
            HarnessError::SessionMismatch {
                source: CheckoutError::NotSuspended(got_session),
                ..
            } => assert_eq!(got_session, session),
            other => panic!("expected SessionMismatch{{source: NotSuspended}}, got {other:?}"),
        }

        let terminal = CheckoutError::Terminal {
            session,
            label: "wedged (a turn timed out)".to_string(),
        };
        match HarnessError::from_checkout(node, terminal) {
            HarnessError::SessionMismatch {
                source:
                    CheckoutError::Terminal {
                        session: got_session,
                        label,
                    },
                ..
            } => {
                assert_eq!(got_session, session);
                assert_eq!(label, "wedged (a turn timed out)");
            }
            other => panic!("expected SessionMismatch{{source: Terminal}}, got {other:?}"),
        }
    }

    /// The abandonment-liveness hook's read side (#22 design doc §3.2 item
    /// 5): a published hole's age is discoverable via `pending_hole_age`
    /// without this crate ever having built a reaper — the hook is exposed,
    /// nothing sweeps it.
    #[tokio::test]
    async fn pending_hole_age_reads_a_published_holes_age() {
        let harness = test_harness();
        let node = harness.create_root("root", "hello").unwrap();
        harness.force(node, Actor::Operator).unwrap();
        let sid = harness
            .tree
            .session_of(node)
            .expect("forced node has a session");
        let hole = HoleId("scont_1".to_string());

        assert!(
            harness.pending_hole_age(node).is_none(),
            "no pending suspension yet"
        );

        harness
            .publish_suspension(
                node,
                sid,
                PendingSuspension {
                    node,
                    hole: hole.clone(),
                    classified: ClassifiedSuspension {
                        routing: SuspensionRouting::Note {
                            text: "why".to_string(),
                        },
                        prompt: "why".to_string(),
                    },
                    raw_request: Value::Con(tidepool_repr::DataConId(0), Vec::new()),
                    resident_hole: ResidentHole::plain(hole.0.clone()),
                    suspend_table: DataConTable::new(),
                    suspend_asks: AsksSidecar::from_pairs(Vec::new()),
                },
            )
            .expect("publish");

        let age = harness.pending_hole_age(node).expect("hole is now pending");
        assert!(age < std::time::Duration::from_secs(5), "freshly published");

        harness
            .consume_suspension(node, sid, &hole)
            .expect("consume");
        assert!(
            harness.pending_hole_age(node).is_none(),
            "consumed hole is no longer pending"
        );
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

    /// A diagnostic whose raw line falls in NEITHER candidate's window — the
    /// wrapper-origin shape, since the wrapper sits textually after the
    /// user's own code — is treated as wrapper FALLOUT, never raw-dumped in
    /// template coordinates: `pick_render_opts` falls back to the first
    /// candidate's window, so `render_diagnostics`'s own fallout partition
    /// classifies the diagnostic as outside the user's code and — since it's
    /// the ONLY diagnostic in the batch — renders the synthetic all-wrapper
    /// framing line PLUS the cleaned underlying diagnostic (poke-round
    /// finding 5, hole 2 — and the follow-up: suppressing it outright left
    /// the recipient with nothing to act on, see the all-wrapper tests in
    /// `tidepool_runtime::diag`).
    #[test]
    fn render_compile_error_wrapper_origin_diagnostic_becomes_fallout_with_diagnostic_included() {
        let expr_source = fake_expr_source(0, 1);
        let bind_source = fake_bind_source(0, 1);
        let err = tidepool_runtime::CompileError::Diagnostics(vec![diag(
            "Expr.hs",
            9999,
            1,
            "deep in generated scaffolding",
        )]);
        let out = render_compile_error(&err, "x", &expr_source, &bind_source);
        // Never remapped to <turn> coordinates (it isn't the user's own
        // code) — but the raw position and message ARE included now.
        assert!(!out.contains("<turn>:"), "{out}");
        assert!(out.contains("Expr.hs:9999:1"), "{out}");
        assert!(out.contains("deep in generated scaffolding"), "{out}");
        assert!(
            out.contains("arose in the harness's result-display wrapper"),
            "expected the synthetic wrapper-fallout framing line too: {out}"
        );
    }

    /// The bug this closes: a `helpers`-param error (real example — a
    /// deliberately-disabled partial function used INSIDE `helpers`) used to
    /// be classified as wrapper-origin fallout, because `helpers` sits
    /// OUTSIDE the single code-only `user_lines` window `pick_render_opts`
    /// used to compute. With the real `-- [user-helpers-lines]` marker
    /// `TurnTemplate::render` now emits (read via
    /// `tidepool_runtime::diag::extract_user_code_ranges`), the SAME
    /// diagnostic is kept and rendered, never folded into the all-wrapper
    /// synthetic message.
    #[test]
    fn render_compile_error_keeps_helpers_and_imports_region_errors() {
        // A synthetic EXPR source carrying the real marker shapes
        // `TurnTemplate::render` emits: an imports marker, a helpers marker,
        // then the ordinary EXPR_MARKER + code + `[user-lines]` marker.
        let expr_source = format!(
            "-- preamble\n\
             import Data.Aeson as Aeson -- [user-imports-lines] 2:2\n\
             -- [user]\n\
             double x = x * 2 -- [user-helpers-lines] 4:4\n\
             {EXPR_MARKER}userExprLine0\n }} in __b  -- [user-lines] 7:7\n"
        );
        let bind_source = fake_bind_source(0, 1);

        // Helpers-region diagnostic.
        let helpers_err = tidepool_runtime::CompileError::Diagnostics(vec![diag(
            "Expr.hs",
            4,
            1,
            "(!!) is partial — use atMay xs i :: Maybe a",
        )]);
        let out = render_compile_error(&helpers_err, "userExprLine0", &expr_source, &bind_source);
        assert!(out.contains("(!!) is partial"), "{out}");
        assert!(
            !out.contains("arose in the harness's result-display wrapper"),
            "a helpers-region error must never be classified as wrapper fallout: {out}"
        );

        // Imports-region diagnostic.
        let imports_err = tidepool_runtime::CompileError::Diagnostics(vec![diag(
            "Expr.hs",
            2,
            1,
            "Could not find module `Data.Aeson'",
        )]);
        let out = render_compile_error(&imports_err, "userExprLine0", &expr_source, &bind_source);
        assert!(out.contains("Could not find module"), "{out}");
        assert!(
            !out.contains("arose in the harness's result-display wrapper"),
            "an imports-region error must never be classified as wrapper fallout: {out}"
        );
    }

    /// A MIXED batch — a genuine user-code error plus a wrapper-origin
    /// fallout diagnostic — keeps the user error verbatim (remapped to
    /// `<turn>` coordinates) and summarizes the wrapper part via the
    /// ordinary fallout footer, never the synthetic all-wrapper message
    /// (reserved for a batch with no surviving in-window diagnostic at all).
    #[test]
    fn render_compile_error_mixed_batch_keeps_user_error_summarizes_wrapper() {
        let expr_source = fake_expr_source(0, 2);
        let bind_source = fake_bind_source(0, 2);
        let user_err = diag("Expr.hs", 3, 1, "Variable not in scope: garbage");
        let wrapper_err = diag("Expr.hs", 999, 1, "No instance for Show Foo");
        let err = tidepool_runtime::CompileError::Diagnostics(vec![user_err, wrapper_err]);
        let block = "userExprLine0\nuserExprLine1";
        let out = render_compile_error(&err, block, &expr_source, &bind_source);
        assert!(out.contains("<turn>:1:"), "{out}");
        assert!(out.contains("Variable not in scope"), "{out}");
        assert!(!out.contains("No instance for Show Foo"), "{out}");
        assert!(out.contains("further error(s) suppressed"), "{out}");
        assert!(
            !out.contains("arose in the harness's result-display wrapper"),
            "{out}"
        );
    }

    /// A WARNING anchored in a generated session decl-plane library module
    /// (`Tidepool/Session/Lib/G<n>.hs`) is dropped unconditionally on this
    /// path — `pick_render_opts` sets `drop_foreign_gen_warnings_except:
    /// Some("")`, since such a warning is the same class of noise as a
    /// wrapper-origin error (not something a turn's own code edits). A real
    /// user-code error in the same batch survives untouched.
    #[test]
    fn render_compile_error_drops_foreign_session_lib_warnings() {
        let expr_source = fake_expr_source(0, 1);
        let bind_source = fake_bind_source(0, 1);
        let user_err = diag("Expr.hs", 3, 1, "Variable not in scope: garbage");
        let lib_warning = tidepool_runtime::diag::ExtractDiag {
            span: Some(tidepool_runtime::diag::DiagSpan {
                file: "Tidepool/Session/Lib/G7.hs".into(),
                start_line: 3,
                start_col: 1,
                end_line: 3,
                end_col: 5,
            }),
            severity: "warning".into(),
            message: "Pattern match(es) are non-exhaustive".into(),
        };
        let err = tidepool_runtime::CompileError::Diagnostics(vec![user_err, lib_warning]);
        let out = render_compile_error(&err, "garbage", &expr_source, &bind_source);
        assert!(out.contains("Variable not in scope"), "{out}");
        assert!(!out.contains("non-exhaustive"), "{out}");
    }

    /// A non-`Diagnostics` variant carries no GHC coordinates to remap —
    /// renders verbatim via `Display`.
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
            provider,
            convos: Mutex::new(HashMap::new()),
            pending_suspensions: Mutex::new(HashMap::new()),
            pending_session_exits: Mutex::new(Vec::new()),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Like [`test_harness`] but keeps the log's tempdir alive and returns its
    /// path, for a test that reads the durable log back after driving the
    /// harness — `test_harness`'s own tempdir is dropped (and the file
    /// unlinked) before it returns. `Harness::force` needs no GHC/extract
    /// compile at all (it registers an [`ResidentSession::unbootstrapped`]
    /// session — the machine comes up on the node's first real turn), so this
    /// fixture does not fabricate a boot expr either.
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
            provider,
            convos: Mutex::new(HashMap::new()),
            pending_suspensions: Mutex::new(HashMap::new()),
            pending_session_exits: Mutex::new(Vec::new()),
            pending: Mutex::new(HashMap::new()),
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
                realm: None,
                scope: None,
                answer_contract: None,
                last_input_tokens: 0,
                framing: None,
                last_turn_source: None,
                turn_lease: false,
                retry_checkout_on_contention: false,
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
            .create_node(None, "test", vec!["Console".to_string()])
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
        let effect_trace: EffectTrace = Arc::new(Mutex::new(vec![rec_a.clone(), rec_b.clone()]));
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
        let restored = convo.effect_trace.lock();
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
            .create_node(None, "test", Vec::new())
            .unwrap();
        harness
            .tree()
            .force(node, Actor::Operator, fake_session(&harness))
            .unwrap();
        insert_convo(&harness, node, Arc::new(Mutex::new(Vec::new())));

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
