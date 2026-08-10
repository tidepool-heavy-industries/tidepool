//! Pluggable observers for the driver's lifecycle events: the driver emits an
//! [`Event`] at each loop/turn/compaction boundary to whatever [`Observer`] it
//! was built with. Ships [`LogObserver`] (events to stderr); `persistence`
//! adds `JsonlObserver` (appends them to a durable transcript). This is the
//! render/loop analogue of `crate::forcing`'s durable per-node event log.

use serde::Serialize;

use crate::selfharness::operator::FormSpec;
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
    /// service it (`site`/`ty` mirror `crate::engine::HoleRouting::RunLLMTurn`;
    /// `prompt` is the hole's human-facing ask text).
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
    /// 1-based WITHIN this hole's servicing (reset per hole, mirroring
    /// `SelfHarnessDriver::drive_answerer_to_finalize`'s own local counter).
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
    /// A typed operator form (`askUser`) was presented — either a nested
    /// answerer's own form, or one the authored OUTER loop evaluated
    /// directly (see [`FormSource`]).
    FormPresented { source: FormSource, spec: FormSpec },
    /// The operator's submission for the most recently presented form. A
    /// decode failure re-suspends on a fresh form (Haskell-side recursion,
    /// no `Either`), so a re-prompt shows as another `FormPresented` /
    /// `FormSubmitted` pair for the same [`FormSource`].
    /// `submission` is the gate's answer VALUE; valid forms can produce an
    /// object, scalar, or `null`.
    FormSubmitted {
        source: FormSource,
        submission: serde_json::Value,
    },
    /// The driver compiled one of the OUTER session's own fragments —
    /// `render(state, lastCompaction)` or `loop __selfHarnessState` — the
    /// compiles `crate::log::Event::TurnStart` never covers (the outer
    /// session is not a tree node). `source` is the full templated module
    /// text actually compiled, verbatim — never truncated, even when large.
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
