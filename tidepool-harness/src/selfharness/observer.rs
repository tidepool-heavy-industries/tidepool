//! Pluggable observers for the driver's lifecycle events: the driver emits an
//! [`Event`] at each loop/turn/compaction boundary to whatever [`Observer`] it
//! was built with. Ships [`LogObserver`] (events to stderr); `persistence`
//! adds `JsonlObserver` (appends them to a durable transcript). This is the
//! render/loop analogue of `crate::forcing`'s durable per-node event log.

use serde::{Deserialize, Serialize};

use crate::selfharness::operator::FormShape;
use crate::tree::NodeId;

/// Which side of the driver presented an `askUser` form: a nested answerer's
/// own form ([`Self::Answerer`]), or one the AUTHORED OUTER loop evaluated
/// directly ([`Self::OuterLoop`], which has no node of its own — see
/// `SelfHarnessDriver::service_outer_askuser_hole`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FormSource {
    Answerer { node: NodeId },
    OuterLoop,
}

/// A driver-minted identity for one `askUser` presentation, so a
/// [`Event::FormPresented`]/[`Event::FormSubmitted`] pair (and, across a
/// decode-failure re-prompt, EVERY pair for the same logical ask) is
/// unambiguous in the wire log rather than inferred from adjacency.
/// `OperatorGate::present_form` returns only the operator's raw submission
/// value — no id of its own — so this is minted driver-side: a single
/// monotonic counter shared by every [`SelfHarnessDriver::present_askuser_form`]
/// call regardless of [`FormSource`] (the nested answerer's own forms and the
/// authored outer loop's both funnel through that one site, and `source`
/// already disambiguates which raised a given id in the log — a second,
/// per-source counter would add a map for no extra debugging power).
/// `#[serde(default)]`: an event logged before this field existed decodes as
/// `AskId(0)`, a sentinel meaning "not recorded", never a hard error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AskId(pub u64);

/// One lifecycle event the driver emits, at the granularity of the outer
/// render/loop hylo — NOT a duplicate of `crate::forcing::Event`
/// (which logs one Agent node's turn/effect/hole history durably); this is
/// the loop-boundary + hole-servicing story layered above it.
/// `Serialize`: [`crate::selfharness::persistence::JsonlObserver`]
/// appends each event as one transcript jsonl line.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Event {
    /// A `render` → `loop` boundary: a fresh loop is starting, having just
    /// composed its system prompt (`SelfHarnessDriver::render_framing`: the
    /// author's `render(state)` output plus the prior compaction summary and
    /// the loop-iteration count).
    LoopBoundary,
    /// An Agent turn started while servicing a `runLLMTurn` hole.
    TurnStart { node: NodeId },
    /// An Agent turn ended (completed or suspended again on a further
    /// hole — e.g. a nested dialog/operator elicitation mid-answer).
    TurnEnd { node: NodeId },
    /// `loop` suspended on a `runLLMTurn @A` hole; the driver is about to
    /// service it (`prompt` is the hole's human-facing ask text).
    RunLLMTurnHole {
        site: u32,
        ty: Option<String>,
        prompt: String,
    },
    /// An Agent turn resolved the pending hole via `finalize`, resuming the
    /// parent `loop`. `value` is the finalized answer, rendered to JSON text
    /// — what the loop actually got back, not just that it got something.
    Finalize { node: NodeId, value: String },
    /// One answerer round while servicing a `runLLMTurn` hole (bracketed by
    /// [`Event::RunLLMTurnHole`]/[`Event::Finalize`]): `site` is the hole's
    /// yield site (correlates rounds to the hole they belong to — a hole's
    /// servicing may span several rounds before it finalizes); `round` is
    /// 1-based WITHIN this hole's servicing (reset per hole).
    /// `error` is the UNTRUNCATED GHC error when this round's block failed to
    /// compile; `None` when it compiled (whether it went on to suspend again
    /// or complete without finalizing). The fold this crate's acceptance test
    /// computes (first-compile success rate, retries-per-hole) is a fold over
    /// exactly these three fields, grouped by `site`.
    AnswererRound {
        node: NodeId,
        site: u32,
        round: u32,
        error: Option<String>,
    },
    /// The answerer (or the AUTHORED OUTER loop) posted display-only
    /// narration via `note` (`NoteWith`, riding the same `AskUser` GADT as
    /// `askUser`) — text pushed to the operator's accumulating feed. `source`
    /// distinguishes a nested answerer's own note from one the AUTHORED
    /// OUTER loop raised directly, same as [`Event::FormPresented`]. Unlike
    /// `FormPresented`, there is no matching submission event: the driver
    /// resumes immediately with `()`, never blocking on the operator.
    NotePosted { source: FormSource, text: String },
    /// A typed operator form (`askUser`) was presented — either a nested
    /// answerer's own form, or one the authored OUTER loop evaluated
    /// directly (see [`FormSource`]). `ask_id` identifies this ONE
    /// presentation (see [`AskId`]) — the matching [`Event::FormSubmitted`]
    /// carries the same value, including across a decode-failure re-prompt,
    /// which mints a FRESH id for its own re-presentation rather than
    /// reusing the original.
    FormPresented {
        source: FormSource,
        shape: FormShape,
        #[serde(default)]
        ask_id: AskId,
    },
    /// The operator's submission for the most recently presented form. A
    /// decode failure re-suspends on a fresh form (Haskell-side recursion,
    /// no `Either`), so a re-prompt shows as another `FormPresented` /
    /// `FormSubmitted` pair for the same [`FormSource`], with its OWN
    /// [`AskId`]. `submission` is the gate's answer VALUE; valid forms can produce an
    /// object, scalar, or `null`.
    FormSubmitted {
        source: FormSource,
        submission: serde_json::Value,
        #[serde(default)]
        ask_id: AskId,
    },
    /// The driver compiled one or more of the OUTER session's own fragments —
    /// the compiles `crate::log::Event::TurnStart` never covers (the outer
    /// session is not a tree node). `label` is `"render+loop"` for the
    /// PRE-loop fused compile (`SelfHarnessDriver::compile_cycle_entry`: the
    /// pre-loop `render(state, lastCompaction)` and this cycle's
    /// `loop __selfHarnessState`, ONE `tidepool-extract` spawn compiling BOTH
    /// as distinct top-level entries of one module) or `"render"` for the
    /// POST-loop render (`SelfHarnessDriver::render_framing`'s own
    /// `compile_outer` call, which cannot fuse — it compiles against the NEW
    /// state the fused pre-loop compile does not have yet). `source` is the
    /// full templated module text actually compiled, verbatim — never
    /// truncated, even when large.
    OuterCompile { label: String, source: String },
    /// The runtime-owned ~80% emergency compaction fired on `node` (the
    /// per-loop answerer). Carries WHAT compaction produced — so the jsonl
    /// transcript records not just that it fired but the summary itself:
    /// the `summary` text, and the answerer's context size (last-turn
    /// `input_tokens`) `pre_input_tokens` (which crossed threshold) and
    /// `post_input_tokens` (the summarizing turn's own input, for the record).
    CompactionTrigger {
        node: NodeId,
        summary: String,
        pre_input_tokens: u64,
        post_input_tokens: u64,
    },
    /// The operator attached a message to a between-loops continue
    /// ([`ContinueSignal::ContinueWithInput`]) — their one initiating
    /// channel; the driver threads it into the next window's framing.
    OperatorMessage { text: String },
    /// Per-loop-boundary machine instrumentation for the SHARED outer
    /// machine (one-session plan, Phase 4): compiled-fragment count
    /// (monotonic — executable memory is never reclaimed), live session-heap
    /// bytes, and collections run. The rotation-cadence evidence base.
    MachineStats {
        fragments: u64,
        live_bytes: u64,
        gc_count: u64,
    },
    /// The shared machine hit its fragment ceiling and was ROTATED at a
    /// quiescent loop boundary: a fresh machine adopted under the same
    /// session id, durable state flowing through the checkpoint as ever;
    /// `bindings_lost` enumerates the living session values that did NOT
    /// survive (legible loss — also surfaced in the next render).
    MachineRotated {
        fragments: u64,
        bindings_lost: Vec<String>,
    },
    /// A restored checkpoint's harness-source fingerprint does not match the
    /// fingerprint of the harness file this process just loaded — expected
    /// during self-iteration (the harness file is the thing being edited),
    /// so the driver restores anyway rather than refusing to start. Recorded
    /// so a subsequent `StateDecode` failure (the restored `State` no longer
    /// matching the edited author types) is diagnosable instead of
    /// mysterious.
    HarnessSourceChanged {
        restored_fingerprint: String,
        current_fingerprint: String,
    },
}

/// A subscriber the driver emits [`Event`]s to. Implementations MUST NOT
/// block the driver for long — a slow subscriber (e.g. a future GUI push)
/// should buffer/dispatch internally rather than stall the loop tick that
/// produced the event.
pub trait Observer: Send + Sync {
    fn on_event(&self, event: &Event);
}

/// Logs each event via `tracing`. The default [`Observer`] the driver emits
/// to when no other subscriber is configured; the production binary fans
/// out to this AND a durable [`crate::selfharness::persistence::JsonlObserver`].
#[derive(Debug, Default)]
pub struct LogObserver;

impl Observer for LogObserver {
    fn on_event(&self, event: &Event) {
        tracing::info!(?event, "selfharness event");
    }
}
