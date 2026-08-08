//! Pluggable observers for the driver's lifecycle events: the driver emits an
//! [`Event`] at each loop/turn/compaction boundary to whatever [`Observer`] it
//! was built with. Ships [`LogObserver`] (events to stderr); `persistence`
//! adds `JsonlObserver` (appends them to a durable transcript). This is the
//! render/loop analogue of `crate::forcing`'s durable per-node event log.

use serde::Serialize;

use crate::tree::NodeId;

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
    /// evaluated `render(state, lastCompaction)` for its system prompt.
    LoopBoundary,
    /// An Agent turn started while servicing a `runLLMTurn` hole.
    TurnStart { node: NodeId },
    /// An Agent turn ended (completed or suspended again on a further
    /// hole — e.g. a nested dialog/operator elicitation mid-answer).
    TurnEnd { node: NodeId },
    /// `loop` suspended on a `runLLMTurn @A` hole; the driver is about to
    /// service it (`site`/`ty` mirror
    /// `crate::engine::HoleRouting::RunLLMTurn`).
    RunLLMTurnHole { site: u32, ty: Option<String> },
    /// An Agent turn resolved the pending hole via `finalize`,
    /// resuming the parent `loop`.
    Finalize { node: NodeId },
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
