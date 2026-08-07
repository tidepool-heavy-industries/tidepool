//! WS-H seam: a pluggable event-observer extension point the
//! [`driver`](crate::selfharness::driver) emits to at each lifecycle event,
//! so logging, a future Datastar-GUI subscriber, and the distillation
//! loop's future "observe and react to events" hooks are all just
//! [`Observer`] (or [`ReactiveHook`]) impls — never hardwired into the
//! driver itself. Reference point: `crate::forcing`'s durable `Event` log is
//! the equivalent extension point for a single node's turn-level history;
//! this is the outer render/loop hylo's analogue, in-memory and pluggable
//! rather than durable and fixed-schema.

use crate::tree::NodeId;

/// One lifecycle event the driver emits, at the granularity of the outer
/// render/loop hylo (§01/02) — NOT a duplicate of `crate::forcing::Event`
/// (which logs one Agent node's turn/effect/hole history durably); this is
/// the loop-boundary + hole-servicing story layered above it.
#[derive(Debug, Clone)]
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
    /// An Agent turn resolved the pending hole via `finalize` (WS-B),
    /// resuming the parent `loop`.
    Finalize { node: NodeId },
    /// The runtime-owned ~80% emergency compaction trigger fired (WS-E).
    CompactionTrigger,
}

/// A subscriber the driver emits [`Event`]s to. Implementations MUST NOT
/// block the driver for long — a slow subscriber (e.g. a future GUI push)
/// should buffer/dispatch internally rather than stall the loop tick that
/// produced the event.
pub trait Observer: Send + Sync {
    fn on_event(&self, event: &Event);
}

/// v1 subscriber: logs each event. The default (and, for the scaffold
/// phase, only wired) [`Observer`] — WS-A's driver emits to one of these by
/// default when no other subscriber is configured.
#[derive(Debug, Default)]
pub struct LogObserver;

impl Observer for LogObserver {
    fn on_event(&self, event: &Event) {
        eprintln!("[selfharness] {event:?}");
    }
}

/// Stub seam for a future Datastar-GUI subscriber (pushing [`Event`]s as SSE
/// fragments to the `tidepool-web` observatory, mirroring how
/// `crate::forcing::Event` already feeds the node-tree pane) — out of scope
/// for this wave (07-impl-orchestration.md's "Out of scope" section).
pub struct GuiObserver;

impl Observer for GuiObserver {
    fn on_event(&self, _event: &Event) {
        unimplemented!("future work: push Event as a Datastar SSE fragment to the observatory")
    }
}

/// Stub seam for the distillation loop's future event reactions — a
/// subscriber that can TRIGGER follow-up work (not just record), the
/// "observe and react to events" extension point named in
/// 07-impl-orchestration.md WS-H. No implementation this wave.
pub trait ReactiveHook: Send + Sync {
    fn react(&self, event: &Event);
}
