//! The node tree + forcing gates. Owns the `NodeId` ↔ `SessionId`
//! binding: [`NodeTree`] is the session-tree state machine the protocol
//! server serves over HTTP.
//!
//! A node is minted as [`NodeState::Thunk`] — no session, no tokens, no
//! effects (forcing is the only work-begins mechanism). [`NodeTree::force`]
//! is the ONLY way out of `Thunk`: it emits `Event::Forced` BEFORE any
//! session exists, then mints a fresh [`SessionId`] and registers the
//! caller-supplied machine with the session registry. Every other
//! lifecycle event (`turn_start`, `effect`, `hole_published`,
//! `hole_answer_attempt`, `hole_consumed`, `node_done`, `node_cancelled`)
//! is rejected for a node still in `Thunk` — that rejection, not a type-level
//! trick, is what makes "events for an unforced node" unrepresentable in the
//! durable log: the writer is never called.
//!
//! `autoForce = never` is hard-coded here: [`NodeTree::create_node`] never
//! calls [`NodeTree::force`] itself, regardless of the fan badge. A
//! `Dynamic`-fan parent's materialized children are ordinary `Thunk` nodes
//! like any other — each one still needs its own `Forced` event. There is
//! no policy ladder to bypass — an auto-force policy ladder is R2 scope,
//! not built here.

use std::collections::HashMap;

use parking_lot::Mutex;
use serde_json::Value;
use tidepool_repr::SessionId;

use crate::log::{Actor, AnswerOutcome, Event, LogWriter, WriteError};
use crate::registry::SessionRegistry;
use crate::tree::{FanBadge, HoleId, NodeId, NodeState, PriceClass, SiteId};

/// What a fork request's shape tells us about child count BEFORE any child
/// is materialized — the input to [`fan_badge`]. Extract-time information
/// (a single `runLLMTurnFork` vs. a statically bounded loop vs. a
/// runtime-length fan-out) maps onto this, not the other way around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkShape {
    /// Precisely `n` children, known before any child runs (`n == 0` for a
    /// node that never forks).
    Exact(u32),
    /// At most `max` children, cap known statically but the live count is
    /// runtime-dependent.
    Bounded(u32),
    /// Child count is not knowable until runtime (e.g. `mapM fork` over a
    /// computed list).
    Dynamic,
}

/// Pre-force fan badge derivation: pure function of [`ForkShape`].
/// `Dynamic` converts to `Exact` only at materialization, and that
/// conversion re-checks the (hard-coded `never`) forcing policy — since
/// `create_node` never auto-forces, every materialized child under a
/// `Dynamic` parent is still a `Thunk` requiring its own `Forced` event.
#[must_use]
pub fn fan_badge(shape: ForkShape) -> FanBadge {
    match shape {
        ForkShape::Exact(n) => FanBadge::Exact { n },
        ForkShape::Bounded(max) => FanBadge::Bounded { max },
        ForkShape::Dynamic => FanBadge::Dynamic,
    }
}

/// Pre-force price-class derivation: pure function of the node's
/// static effect row and whether it spawns calling-model (frontier)
/// children. `Frontier` dominates — a node with an otherwise `Llm`-free row
/// that still forks frontier children is priced `Frontier`, not `Zero`.
#[must_use]
pub fn price_class(effect_row: &[String], spawns_frontier_children: bool) -> PriceClass {
    if spawns_frontier_children {
        PriceClass::Frontier
    } else if effect_row.iter().any(|e| e == "Llm") {
        PriceClass::Llm
    } else {
        PriceClass::Zero
    }
}

/// Harness-generated teaser text — never program-authored. Currently a
/// minimal deterministic composition of the title hint and effect row; may
/// grow richer over time without breaking that invariant.
#[must_use]
pub fn derive_teaser(title: &str, effect_row: &[String]) -> String {
    if effect_row.is_empty() {
        title.to_string()
    } else {
        format!("{title} [{}]", effect_row.join(", "))
    }
}

/// Why a tree operation was refused.
#[derive(Debug, thiserror::Error)]
pub enum TreeError {
    #[error("node {0:?} unknown")]
    UnknownNode(NodeId),
    #[error("node {0:?} is not a thunk (state: {1:?}); forcing is the only way to leave Thunk")]
    NotThunk(NodeId, NodeState),
    #[error("node {0:?} is an unforced thunk — no turn/effect/hole events until it is forced")]
    UnforcedThunk(NodeId),
    #[error("node {0:?} is not running (state: {1:?})")]
    NotRunning(NodeId, NodeState),
    #[error("node {0:?} is not suspended (state: {1:?})")]
    NotSuspended(NodeId, NodeState),
    #[error("node {node:?}: suspended on {pending:?}, attempted {attempted:?}")]
    HoleMismatch {
        node: NodeId,
        pending: HoleId,
        attempted: HoleId,
    },
    #[error("node {0:?} is already terminal (state: {1:?})")]
    AlreadyTerminal(NodeId, NodeState),
    #[error(
        "node {0:?} cannot be reopened for a follow-up (state: {1:?}); only a Done node continues"
    )]
    NotReopenable(NodeId, NodeState),
    #[error("event log write failed: {0}")]
    Log(#[from] WriteError),
}

struct NodeEntry {
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    state: NodeState,
    session: Option<SessionId>,
    /// Whether this node OWNS its session's registry slot (true for a
    /// session minted by [`NodeTree::force`]) or merely runs ON a shared one
    /// (false — [`NodeTree::force_attached`], the one-session collapse:
    /// answerer nodes execute as realms on the outer session). Retirement
    /// removes the registry slot only for owners; a non-owner's retirement
    /// is realm scope-exit, done by the caller against the shared machine.
    owns_session: bool,
}

struct Inner {
    nodes: HashMap<NodeId, NodeEntry>,
    writer: LogWriter,
    next_node_id: u64,
    next_session_id: u64,
}

impl Inner {
    fn entry(&self, node: NodeId) -> Result<&NodeEntry, TreeError> {
        self.nodes.get(&node).ok_or(TreeError::UnknownNode(node))
    }

    /// Every non-creation, non-forcing lifecycle event requires the node to
    /// be `Running` — which a `Thunk` node structurally cannot be (only
    /// [`NodeTree::force`] leaves `Thunk`), so this is where "unforced node
    /// emits a work event" gets rejected.
    fn require_running(&self, node: NodeId) -> Result<(), TreeError> {
        match &self.entry(node)?.state {
            NodeState::Thunk => Err(TreeError::UnforcedThunk(node)),
            NodeState::Running => Ok(()),
            other => Err(TreeError::NotRunning(node, other.clone())),
        }
    }
}

/// The session tree: parent/child structure, per-node [`NodeState`], and
/// the `NodeId` ↔ `SessionId` binding, backed by a durable event log.
///
/// Generic over the machine handle `M` — same discipline as
/// [`SessionRegistry`] — so this crate stays free of the JIT/runtime
/// dependency; the binary wiring instantiates `M` with a concrete resident
/// session type.
pub struct NodeTree<M> {
    registry: SessionRegistry<M>,
    inner: Mutex<Inner>,
}

impl<M> NodeTree<M> {
    /// A fresh tree backed by `writer` (already past its header line).
    pub fn new(writer: LogWriter) -> Self {
        NodeTree {
            registry: SessionRegistry::new(),
            inner: Mutex::new(Inner {
                nodes: HashMap::new(),
                writer,
                next_node_id: 0,
                next_session_id: 0,
            }),
        }
    }

    /// The session registry backing this tree — forced nodes' machines live
    /// here, keyed by the `SessionId` this tree minted for them.
    pub fn registry(&self) -> &SessionRegistry<M> {
        &self.registry
    }

    /// Register a NODE-LESS session (the one-session collapse's OUTER
    /// session): mints a `SessionId` from the same counter `force` uses and
    /// inserts the machine as `Idle`. The caller owns retirement (there is
    /// no node whose termination would remove it); attached nodes
    /// ([`Self::force_attached`]) run on it as realms.
    pub fn adopt_session(&self, machine: M) -> SessionId {
        let mut inner = self.inner.lock();
        let session = SessionId(inner.next_session_id);
        inner.next_session_id += 1;
        self.registry.insert_idle(session, machine);
        session
    }

    /// Mint a new node as [`NodeState::Thunk`] under `parent` (root if
    /// `None`), deriving its badges purely from `effect_row`/`fork_shape`/
    /// `spawns_frontier_children` and emitting `Event::NodeCreated`. Never
    /// forces — `autoForce = never` is hard-coded by simply not calling
    /// [`Self::force`] here, for any fan shape.
    pub fn create_node(
        &self,
        parent: Option<NodeId>,
        title: &str,
        effect_row: Vec<String>,
        fork_shape: ForkShape,
        spawns_frontier_children: bool,
    ) -> Result<NodeId, TreeError> {
        let mut inner = self.inner.lock();
        if let Some(p) = parent {
            inner.entry(p)?;
        }

        let node = NodeId(inner.next_node_id);
        inner.next_node_id += 1;

        let fan = fan_badge(fork_shape);
        let price = price_class(&effect_row, spawns_frontier_children);
        let teaser = derive_teaser(title, &effect_row);

        inner.writer.append(Event::NodeCreated {
            node,
            parent,
            teaser,
            effect_row,
            fan,
            price,
        })?;

        inner.nodes.insert(
            node,
            NodeEntry {
                parent,
                children: Vec::new(),
                state: NodeState::Thunk,
                session: None,
                owns_session: false,
            },
        );
        if let Some(p) = parent {
            inner
                .nodes
                .get_mut(&p)
                .expect("checked present above")
                .children
                .push(node);
        }
        Ok(node)
    }

    /// The ONLY transition out of `Thunk`. Emits `Event::Forced{actor}`
    /// BEFORE any session exists, then mints a fresh `SessionId` and
    /// registers `machine` with the registry as idle. Refuses a node that
    /// is not currently `Thunk` ([`TreeError::NotThunk`]).
    pub fn force(&self, node: NodeId, actor: Actor, machine: M) -> Result<SessionId, TreeError> {
        let mut inner = self.inner.lock();
        match &inner.entry(node)?.state {
            NodeState::Thunk => {}
            other => return Err(TreeError::NotThunk(node, other.clone())),
        }

        // Log the forcing decision BEFORE the session comes into being —
        // consent integrity reads the log, not the registry.
        inner.writer.append(Event::Forced { node, actor })?;

        let session = SessionId(inner.next_session_id);
        inner.next_session_id += 1;
        self.registry.insert_idle(session, machine);

        let entry = inner.nodes.get_mut(&node).expect("checked present above");
        entry.state = NodeState::Running;
        entry.session = Some(session);
        entry.owns_session = true;
        Ok(session)
    }

    /// Force `node` ONTO AN EXISTING session (the one-session collapse):
    /// same `Thunk → Running` transition and `Event::Forced` consent line as
    /// [`Self::force`], but no machine is minted — the node's turns run as a
    /// realm on `session`'s machine, and the node does NOT own the registry
    /// slot (retirement is realm scope-exit, not slot removal). Refuses a
    /// non-`Thunk` node and an unknown/absent `session` (attaching to a
    /// session that was never registered would wedge every later checkout
    /// with `Unknown`, attributed to the wrong place).
    pub fn force_attached(
        &self,
        node: NodeId,
        actor: Actor,
        session: SessionId,
    ) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        match &inner.entry(node)?.state {
            NodeState::Thunk => {}
            other => return Err(TreeError::NotThunk(node, other.clone())),
        }
        inner.writer.append(Event::Forced { node, actor })?;
        let entry = inner.nodes.get_mut(&node).expect("checked present above");
        entry.state = NodeState::Running;
        entry.session = Some(session);
        entry.owns_session = false;
        Ok(())
    }

    /// Whether `node` owns its session's registry slot (see
    /// [`NodeEntry::owns_session`]). `false` for attached nodes and for
    /// nodes with no session at all.
    pub fn node_owns_session(&self, node: NodeId) -> bool {
        self.inner
            .lock()
            .nodes
            .get(&node)
            .is_some_and(|e| e.session.is_some() && e.owns_session)
    }

    /// Log the start of a turn on `node`. Requires `Running`.
    pub fn turn_start(
        &self,
        node: NodeId,
        source: String,
        input: Option<Value>,
    ) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        inner.require_running(node)?;
        inner.writer.append(Event::TurnStart {
            node,
            source,
            input,
        })?;
        Ok(())
    }

    /// Log a just-compiled turn's extracted types on `node` (the `asks.json`
    /// site → type table, plus a value-plane bind's bound name/type, when
    /// either is non-empty). Requires `Running`.
    pub fn turn_extracted(
        &self,
        node: NodeId,
        asks: Vec<(u32, String)>,
        bound: Option<(String, String)>,
    ) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        inner.require_running(node)?;
        inner
            .writer
            .append(Event::TurnExtracted { node, asks, bound })?;
        Ok(())
    }

    /// Log one effect request/response pair on `node`. Requires `Running`.
    pub fn effect(
        &self,
        node: NodeId,
        seq: u64,
        tag: String,
        req: Value,
        resp: Value,
    ) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        inner.require_running(node)?;
        inner.writer.append(Event::Effect {
            node,
            seq,
            tag,
            req,
            resp,
        })?;
        Ok(())
    }

    /// Publish a hole, moving `node` from `Running` to `Suspended{hole}`.
    pub fn hole_published(
        &self,
        node: NodeId,
        hole: HoleId,
        site: Option<SiteId>,
        ty: Option<String>,
        prompt: String,
        fork: bool,
    ) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        inner.require_running(node)?;
        inner.writer.append(Event::HolePublished {
            node,
            hole: hole.clone(),
            site,
            ty,
            prompt,
            fork,
        })?;
        inner
            .nodes
            .get_mut(&node)
            .expect("checked present above")
            .state = NodeState::Suspended { hole };
        Ok(())
    }

    /// Log an attempt to answer `node`'s pending hole. Requires `Suspended`
    /// (on any hole — a stale/wrong-hole attempt is still a loggable
    /// event, typically with a `Rejected` outcome).
    pub fn hole_answer_attempt(
        &self,
        node: NodeId,
        hole: HoleId,
        source: String,
        outcome: AnswerOutcome,
    ) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        match &inner.entry(node)?.state {
            NodeState::Thunk => return Err(TreeError::UnforcedThunk(node)),
            NodeState::Suspended { .. } => {}
            other => return Err(TreeError::NotSuspended(node, other.clone())),
        }
        inner.writer.append(Event::HoleAnswerAttempt {
            node,
            hole,
            source,
            outcome,
        })?;
        Ok(())
    }

    /// Consume `node`'s pending hole, moving `Suspended{hole}` back to
    /// `Running`. `hole` must match the pending one exactly
    /// ([`TreeError::HoleMismatch`] otherwise, state left untouched).
    pub fn hole_consumed(&self, node: NodeId, hole: HoleId) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        match &inner.entry(node)?.state {
            NodeState::Thunk => return Err(TreeError::UnforcedThunk(node)),
            NodeState::Suspended { hole: pending } if *pending == hole => {}
            NodeState::Suspended { hole: pending } => {
                return Err(TreeError::HoleMismatch {
                    node,
                    pending: pending.clone(),
                    attempted: hole,
                })
            }
            other => return Err(TreeError::NotSuspended(node, other.clone())),
        }
        inner.writer.append(Event::HoleConsumed { node, hole })?;
        inner
            .nodes
            .get_mut(&node)
            .expect("checked present above")
            .state = NodeState::Running;
        Ok(())
    }

    /// Log one conversation-turn delta on `node` (the transcript store's
    /// append). Requires `Running` OR `Suspended`: a fork
    /// answerer's assistant turn lands while its own node is `Running`, but the
    /// operator's answer to a still-`Suspended` node is also a turn worth
    /// recording, so both non-terminal working states are accepted.
    pub fn turn_delta(
        &self,
        node: NodeId,
        turn: u64,
        role: crate::provider::Role,
        content: String,
        usage: Option<crate::provider::Usage>,
    ) -> Result<(), TreeError> {
        self.turn_delta_reasoned(node, turn, role, content, usage, None)
    }

    /// Like [`Self::turn_delta`] but also records the assistant turn's
    /// reasoning-summary ("thinking"). Split out so the common no-reasoning
    /// call sites (user/system framing turns) keep their simple signature.
    pub fn turn_delta_reasoned(
        &self,
        node: NodeId,
        turn: u64,
        role: crate::provider::Role,
        content: String,
        usage: Option<crate::provider::Usage>,
        reasoning: Option<String>,
    ) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        match &inner.entry(node)?.state {
            NodeState::Thunk => return Err(TreeError::UnforcedThunk(node)),
            NodeState::Running | NodeState::Suspended { .. } => {}
            other => return Err(TreeError::NotRunning(node, other.clone())),
        }
        inner.writer.append(Event::TurnDelta {
            node,
            turn,
            role,
            content,
            usage,
            reasoning,
        })?;
        Ok(())
    }

    /// Log an OPERATOR-SPLICED message on `node` (the `turn_spliced` kind) —
    /// distinct from [`Self::turn_delta`] so a genuine operator interjection
    /// is never mistaken for a modeled turn when auditing history. Same
    /// non-terminal, forced-state guard as `turn_delta` (`Running` or
    /// `Suspended`): a splice needs a live transcript to land in, exactly
    /// like a turn delta does.
    pub fn turn_spliced(
        &self,
        node: NodeId,
        turn: u64,
        role: crate::provider::Role,
        content: String,
    ) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        match &inner.entry(node)?.state {
            NodeState::Thunk => return Err(TreeError::UnforcedThunk(node)),
            NodeState::Running | NodeState::Suspended { .. } => {}
            other => return Err(TreeError::NotRunning(node, other.clone())),
        }
        inner.writer.append(Event::TurnSpliced {
            node,
            turn,
            role,
            content,
        })?;
        Ok(())
    }

    /// Record a fork's transcript reference: `node` inherits `parent`'s
    /// conversation up to `parent_turn`. `node` must be a `Thunk` (a fork is
    /// registered before it is forced — the reference is set at materialization
    /// time, the child forced separately).
    pub fn turn_forked(
        &self,
        node: NodeId,
        parent: NodeId,
        parent_turn: u64,
    ) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        inner.entry(node)?;
        inner.entry(parent)?;
        inner.writer.append(Event::TurnForked {
            node,
            parent,
            parent_turn,
        })?;
        Ok(())
    }

    /// Record that `node`'s context prefix was frozen as a cache root
    /// ([`Event::SnapshotFrozen`]). Structure-only, like [`Self::turn_forked`]:
    /// freezing observes a transcript, it never moves a node's lifecycle
    /// state, so any known node may be the subject.
    pub fn snapshot_frozen(
        &self,
        node: NodeId,
        digest: crate::snapshot::SnapshotDigest,
        messages: u64,
        prefix_bytes: u64,
    ) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        inner.entry(node)?;
        inner.writer.append(Event::SnapshotFrozen {
            node,
            digest,
            messages,
            prefix_bytes,
        })?;
        Ok(())
    }

    /// Record a snapshot-forked branch's first-turn receipt
    /// ([`Event::BranchInvocation`]). Emitted from inside the turn that
    /// produced the numbers, so `node` is `Running`/`Suspended` — the same
    /// guard [`Self::turn_delta`] applies, for the same reason (a receipt
    /// about a turn belongs to a node that is having one).
    pub fn branch_invocation(
        &self,
        node: NodeId,
        snapshot: crate::snapshot::SnapshotDigest,
        shared_prefix_bytes: u64,
        branch_suffix_bytes: u64,
        input_tokens: u64,
        cached_input_tokens: Option<u64>,
    ) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        match &inner.entry(node)?.state {
            NodeState::Thunk => return Err(TreeError::UnforcedThunk(node)),
            NodeState::Running | NodeState::Suspended { .. } => {}
            other => return Err(TreeError::NotRunning(node, other.clone())),
        }
        inner.writer.append(Event::BranchInvocation {
            node,
            snapshot,
            shared_prefix_bytes,
            branch_suffix_bytes,
            input_tokens,
            cached_input_tokens,
        })?;
        Ok(())
    }

    /// Complete `node`, moving `Running` to `Done`.
    pub fn node_done(&self, node: NodeId, result_rendered: String) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        inner.require_running(node)?;
        inner.writer.append(Event::NodeDone {
            node,
            result_rendered,
        })?;
        inner
            .nodes
            .get_mut(&node)
            .expect("checked present above")
            .state = NodeState::Done;
        Ok(())
    }

    /// Reopen a completed node for a FOLLOW-UP turn: `Done` → `Running`, so the
    /// operator can continue the conversation. Only a `Done` node reopens (a
    /// `Suspended` node has a hole to answer instead; `Thunk`/`Cancelled` don't
    /// continue). No event is logged for the flip itself — the follow-up's
    /// `TurnDelta`/`TurnStart`/`NodeDone` events carry the story, and
    /// crash-replay folds cleanly (the later `NodeDone` wins).
    pub fn reopen(&self, node: NodeId) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        match &inner.entry(node)?.state {
            NodeState::Done => {}
            other => return Err(TreeError::NotReopenable(node, other.clone())),
        }
        inner
            .nodes
            .get_mut(&node)
            .expect("checked present above")
            .state = NodeState::Running;
        Ok(())
    }

    /// Cancel `node` from any non-terminal state (`Thunk`, `Running`, or
    /// `Suspended`). Refuses an already-`Done`/`Cancelled` node
    /// ([`TreeError::AlreadyTerminal`]).
    pub fn node_cancelled(&self, node: NodeId, reason: String) -> Result<(), TreeError> {
        let mut inner = self.inner.lock();
        match &inner.entry(node)?.state {
            NodeState::Done | NodeState::Cancelled { .. } => {
                let state = inner.entry(node)?.state.clone();
                return Err(TreeError::AlreadyTerminal(node, state));
            }
            _ => {}
        }
        inner.writer.append(Event::NodeCancelled {
            node,
            reason: reason.clone(),
        })?;
        inner
            .nodes
            .get_mut(&node)
            .expect("checked present above")
            .state = NodeState::Cancelled { reason };
        Ok(())
    }

    /// `node`'s current lifecycle state.
    pub fn state(&self, node: NodeId) -> Option<NodeState> {
        self.inner.lock().nodes.get(&node).map(|e| e.state.clone())
    }

    /// `node`'s parent, if `node` exists (`Some(None)` for a root node).
    pub fn parent(&self, node: NodeId) -> Option<Option<NodeId>> {
        self.inner.lock().nodes.get(&node).map(|e| e.parent)
    }

    /// `node`'s children in creation order, if `node` exists.
    pub fn children(&self, node: NodeId) -> Option<Vec<NodeId>> {
        self.inner
            .lock()
            .nodes
            .get(&node)
            .map(|e| e.children.clone())
    }

    /// The `SessionId` bound to `node`, if it has been forced.
    pub fn session_of(&self, node: NodeId) -> Option<SessionId> {
        self.inner.lock().nodes.get(&node).and_then(|e| e.session)
    }

    /// Cursor-paged node-id enumeration. Node ids are minted monotonically from 0 and
    /// never reused ([`Self::create_node`]), so every id in `0..next_node_id`
    /// is a live node — this is an exact `O(min(limit, n))` slice, not a
    /// probe. Returns up to `limit` ids strictly greater than `cursor`
    /// (`None` starts from the beginning), plus the next cursor to pass back
    /// for the following page (`None` once the page reaches the end).
    pub fn node_ids_after(
        &self,
        cursor: Option<NodeId>,
        limit: usize,
    ) -> (Vec<NodeId>, Option<NodeId>) {
        let inner = self.inner.lock();
        let start = cursor.map(|c| c.0.saturating_add(1)).unwrap_or(0);
        let end = inner.next_node_id;
        let mut ids = Vec::new();
        let mut id = start;
        while id < end && ids.len() < limit {
            ids.push(NodeId(id));
            id += 1;
        }
        let next = if ids.len() == limit && id < end {
            ids.last().copied()
        } else {
            None
        };
        (ids, next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::LogReader;
    use crate::log::{Event as LogEvent, LogHeader};

    fn header() -> LogHeader {
        LogHeader {
            prelude_hash: "test-prelude".into(),
            extract_fingerprint: "test-extract".into(),
            harness_version: "test-harness".into(),
        }
    }

    fn writer_at(path: &std::path::Path) -> LogWriter {
        LogWriter::create(path, &header()).expect("create log")
    }

    /// Stand-in for the machine handle `M` — the tree's contract is pure
    /// lifecycle bookkeeping, so a counter is enough to exercise the
    /// transitions without the JIT.
    #[derive(Debug, PartialEq, Eq)]
    struct FakeMachine {
        id: u32,
    }

    fn tree_at(path: &std::path::Path) -> NodeTree<FakeMachine> {
        NodeTree::new(writer_at(path))
    }

    /// Extracts the primary `node` field a given [`Event`] variant
    /// concerns, for filtering a log by node in tests.
    fn event_node(event: &LogEvent) -> NodeId {
        match event {
            LogEvent::NodeCreated { node, .. }
            | LogEvent::Forced { node, .. }
            | LogEvent::TurnStart { node, .. }
            | LogEvent::TurnExtracted { node, .. }
            | LogEvent::Effect { node, .. }
            | LogEvent::HolePublished { node, .. }
            | LogEvent::HoleAnswerAttempt { node, .. }
            | LogEvent::HoleConsumed { node, .. }
            | LogEvent::NodeDone { node, .. }
            | LogEvent::NodeCancelled { node, .. }
            | LogEvent::TurnDelta { node, .. }
            | LogEvent::TurnForked { node, .. }
            | LogEvent::TurnSpliced { node, .. }
            | LogEvent::SnapshotFrozen { node, .. }
            | LogEvent::BranchInvocation { node, .. } => *node,
        }
    }

    // ---- pure badge/teaser derivation ----------------------------------

    #[test]
    fn price_class_zero_when_no_llm_and_no_frontier_children() {
        assert_eq!(
            price_class(&["Fs".to_string(), "Exec".to_string()], false),
            PriceClass::Zero
        );
        assert_eq!(price_class(&[], false), PriceClass::Zero);
    }

    #[test]
    fn price_class_llm_when_row_includes_llm_only() {
        assert_eq!(
            price_class(&["Fs".to_string(), "Llm".to_string()], false),
            PriceClass::Llm
        );
    }

    #[test]
    fn price_class_frontier_dominates() {
        // Even with no in-row Llm, spawning frontier children wins.
        assert_eq!(price_class(&["Fs".to_string()], true), PriceClass::Frontier);
        // And it dominates over an Llm-bearing row too.
        assert_eq!(
            price_class(&["Llm".to_string()], true),
            PriceClass::Frontier
        );
    }

    #[test]
    fn fan_badge_maps_shape_directly() {
        assert_eq!(fan_badge(ForkShape::Exact(1)), FanBadge::Exact { n: 1 });
        assert_eq!(fan_badge(ForkShape::Exact(0)), FanBadge::Exact { n: 0 });
        assert_eq!(
            fan_badge(ForkShape::Bounded(4)),
            FanBadge::Bounded { max: 4 }
        );
        assert_eq!(fan_badge(ForkShape::Dynamic), FanBadge::Dynamic);
    }

    #[test]
    fn teaser_includes_effect_row_when_present() {
        assert_eq!(derive_teaser("summarize PR", &[]), "summarize PR");
        assert_eq!(
            derive_teaser("summarize PR", &["Fs".to_string(), "Llm".to_string()]),
            "summarize PR [Fs, Llm]"
        );
    }

    // ---- Thunk-only-exits-via-Forced -----------------------------------

    #[test]
    fn node_starts_thunk_and_rejects_work_events_until_forced() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree_at(&dir.path().join("run.jsonl"));

        let node = tree
            .create_node(None, "root", vec!["Fs".into()], ForkShape::Exact(0), false)
            .unwrap();
        assert_eq!(tree.state(node), Some(NodeState::Thunk));

        // Every work-begins event is rejected while Thunk.
        assert!(matches!(
            tree.turn_start(node, "root turn".into(), None),
            Err(TreeError::UnforcedThunk(n)) if n == node
        ));
        assert!(matches!(
            tree.effect(node, 0, "Fs".into(), Value::Null, Value::Null),
            Err(TreeError::UnforcedThunk(n)) if n == node
        ));
        assert!(matches!(
            tree.hole_published(
                node,
                HoleId("h1".into()),
                None,
                None,
                "prompt".into(),
                false
            ),
            Err(TreeError::UnforcedThunk(n)) if n == node
        ));

        // Forcing is the only way out.
        let session = tree
            .force(node, Actor::Operator, FakeMachine { id: 1 })
            .unwrap();
        assert_eq!(tree.state(node), Some(NodeState::Running));
        assert_eq!(tree.session_of(node), Some(session));

        // Now the work events succeed.
        tree.turn_start(node, "root turn".into(), None).unwrap();
    }

    #[test]
    fn forcing_a_non_thunk_node_errors() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree_at(&dir.path().join("run.jsonl"));
        let node = tree
            .create_node(None, "root", vec![], ForkShape::Exact(0), false)
            .unwrap();

        tree.force(node, Actor::Operator, FakeMachine { id: 1 })
            .unwrap();
        // Second force on an already-Running node is rejected.
        let err = tree
            .force(node, Actor::Operator, FakeMachine { id: 2 })
            .unwrap_err();
        assert!(matches!(err, TreeError::NotThunk(n, NodeState::Running) if n == node));
    }

    #[test]
    fn events_for_unknown_node_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let tree: NodeTree<FakeMachine> = tree_at(&dir.path().join("run.jsonl"));
        let ghost = NodeId(999);
        assert!(matches!(
            tree.turn_start(ghost, "x".into(), None),
            Err(TreeError::UnknownNode(n)) if n == ghost
        ));
        assert!(matches!(
            tree.force(ghost, Actor::Operator, FakeMachine { id: 1 }),
            Err(TreeError::UnknownNode(n)) if n == ghost
        ));
    }

    #[test]
    fn hole_lifecycle_transitions_and_rejects_misuse() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree_at(&dir.path().join("run.jsonl"));
        let node = tree
            .create_node(None, "root", vec![], ForkShape::Exact(0), false)
            .unwrap();
        tree.force(node, Actor::Operator, FakeMachine { id: 1 })
            .unwrap();
        tree.turn_start(node, "turn".into(), None).unwrap();

        let hole = HoleId("scont_1".into());
        tree.hole_published(node, hole.clone(), None, None, "pick one".into(), false)
            .unwrap();
        assert_eq!(
            tree.state(node),
            Some(NodeState::Suspended { hole: hole.clone() })
        );

        // A hole_published while already Suspended is rejected (not Running).
        assert!(matches!(
            tree.hole_published(node, HoleId("scont_2".into()), None, None, "x".into(), false),
            Err(TreeError::NotRunning(n, NodeState::Suspended { .. })) if n == node
        ));

        // node_done while Suspended is rejected.
        assert!(matches!(
            tree.node_done(node, "x".into()),
            Err(TreeError::NotRunning(n, NodeState::Suspended { .. })) if n == node
        ));

        // Answering the wrong hole does not consume the pending one.
        let err = tree
            .hole_consumed(node, HoleId("scont_wrong".into()))
            .unwrap_err();
        assert!(matches!(err, TreeError::HoleMismatch { node: n, .. } if n == node));
        assert_eq!(
            tree.state(node),
            Some(NodeState::Suspended { hole: hole.clone() })
        );

        // Logging the (rejected) attempt is still fine — attempts are
        // loggable regardless of outcome.
        tree.hole_answer_attempt(
            node,
            HoleId("scont_wrong".into()),
            "child".into(),
            AnswerOutcome::Rejected {
                error: "type mismatch".into(),
            },
        )
        .unwrap();

        // The right hole consumes and returns to Running.
        tree.hole_consumed(node, hole).unwrap();
        assert_eq!(tree.state(node), Some(NodeState::Running));

        tree.node_done(node, "42".into()).unwrap();
        assert_eq!(tree.state(node), Some(NodeState::Done));

        // A Done node is terminal: no further work events, no re-forcing,
        // no re-cancelling.
        assert!(matches!(
            tree.turn_start(node, "x".into(), None),
            Err(TreeError::NotRunning(n, NodeState::Done)) if n == node
        ));
        assert!(matches!(
            tree.node_cancelled(node, "too late".into()),
            Err(TreeError::AlreadyTerminal(n, NodeState::Done)) if n == node
        ));
    }

    #[test]
    fn turn_spliced_rejects_thunk_accepts_running_and_suspended_rejects_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("run.jsonl");
        let tree = tree_at(&log_path);
        let node = tree
            .create_node(None, "root", vec![], ForkShape::Exact(0), false)
            .unwrap();

        // Unforced: rejected, same guard as turn_delta.
        assert!(matches!(
            tree.turn_spliced(node, 0, crate::provider::Role::User, "hi".into()),
            Err(TreeError::UnforcedThunk(n)) if n == node
        ));

        tree.force(node, Actor::Operator, FakeMachine { id: 1 })
            .unwrap();

        // Running: accepted.
        tree.turn_spliced(
            node,
            0,
            crate::provider::Role::User,
            "operator says hi".into(),
        )
        .unwrap();

        // Suspended: also accepted (an operator can interject while a node
        // waits on a hole, same as a turn delta can be logged then).
        tree.hole_published(
            node,
            HoleId("scont_1".into()),
            None,
            None,
            "?".into(),
            false,
        )
        .unwrap();
        tree.turn_spliced(node, 1, crate::provider::Role::User, "still here".into())
            .unwrap();

        tree.hole_consumed(node, HoleId("scont_1".into())).unwrap();
        tree.node_done(node, "done".into()).unwrap();

        // Done: terminal, rejected.
        assert!(matches!(
            tree.turn_spliced(node, 2, crate::provider::Role::User, "too late".into()),
            Err(TreeError::NotRunning(n, NodeState::Done)) if n == node
        ));

        // The log carries exactly the two accepted TurnSpliced events, in order.
        let (_header, events) = LogReader::open(&log_path).expect("open log");
        let spliced: Vec<String> = events
            .map(|r| r.expect("well-formed record").event)
            .filter_map(|e| match e {
                LogEvent::TurnSpliced { content, .. } => Some(content),
                _ => None,
            })
            .collect();
        assert_eq!(spliced, vec!["operator says hi", "still here"]);
    }

    #[test]
    fn cancel_works_from_thunk_running_and_suspended_but_not_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree_at(&dir.path().join("run.jsonl"));

        let thunk = tree
            .create_node(None, "a", vec![], ForkShape::Exact(0), false)
            .unwrap();
        tree.node_cancelled(thunk, "abandoned".into()).unwrap();
        assert_eq!(
            tree.state(thunk),
            Some(NodeState::Cancelled {
                reason: "abandoned".into()
            })
        );

        let running = tree
            .create_node(None, "b", vec![], ForkShape::Exact(0), false)
            .unwrap();
        tree.force(running, Actor::Operator, FakeMachine { id: 1 })
            .unwrap();
        tree.node_cancelled(running, "operator stop".into())
            .unwrap();
        assert!(matches!(
            tree.state(running),
            Some(NodeState::Cancelled { .. })
        ));

        // Cancelling twice is rejected.
        assert!(matches!(
            tree.node_cancelled(running, "again".into()),
            Err(TreeError::AlreadyTerminal(n, NodeState::Cancelled { .. })) if n == running
        ));
    }

    #[test]
    fn dynamic_fan_children_are_never_auto_forced() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree_at(&dir.path().join("run.jsonl"));
        let parent = tree
            .create_node(None, "fan-out", vec![], ForkShape::Dynamic, false)
            .unwrap();
        tree.force(parent, Actor::Operator, FakeMachine { id: 0 })
            .unwrap();

        let mut kids = Vec::new();
        for i in 0..3 {
            kids.push(
                tree.create_node(
                    Some(parent),
                    &format!("child {i}"),
                    vec![],
                    ForkShape::Exact(0),
                    false,
                )
                .unwrap(),
            );
        }
        assert_eq!(tree.children(parent), Some(kids.clone()));
        for kid in kids {
            // autoForce=never: materializing a Dynamic-fan child never
            // forces it, no matter the parent's fan badge.
            assert_eq!(tree.state(kid), Some(NodeState::Thunk));
        }
    }

    // ---- consent-integrity, against a real log file --------------------

    #[test]
    fn thunk_child_from_a_fork_request_has_zero_events_until_forced() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("run.jsonl");
        let tree = tree_at(&log_path);

        // Parent is forced and running (as if mid-turn).
        let parent = tree
            .create_node(
                None,
                "parent",
                vec!["Fs".into()],
                ForkShape::Exact(1),
                false,
            )
            .unwrap();
        tree.force(parent, Actor::Operator, FakeMachine { id: 0 })
            .unwrap();
        tree.turn_start(parent, "parent turn".into(), None).unwrap();

        // A `runLLMTurnFork`-shaped request publishes a Thunk child —
        // never a running one.
        let child = tree
            .create_node(
                Some(parent),
                "forked task",
                vec!["Llm".into()],
                ForkShape::Exact(1),
                false,
            )
            .unwrap();
        assert_eq!(tree.state(child), Some(NodeState::Thunk));
        assert_eq!(tree.session_of(child), None);

        // Read the REAL log file back and assert zero turn/effect/hole
        // events reference the child — only its NodeCreated line exists.
        let (_header, events) = LogReader::open(&log_path).expect("open log");
        let child_events: Vec<LogEvent> = events
            .map(|r| r.expect("well-formed record").event)
            .filter(|e| event_node(e) == child)
            .collect();
        assert_eq!(child_events.len(), 1, "only NodeCreated before forcing");
        assert!(matches!(child_events[0], LogEvent::NodeCreated { .. }));
        for e in &child_events {
            assert!(
                !matches!(
                    e,
                    LogEvent::TurnStart { .. }
                        | LogEvent::Effect { .. }
                        | LogEvent::HolePublished { .. }
                ),
                "no turn/effect/hole event before Forced"
            );
        }

        // Now force the child and drive it through a minimal lifecycle.
        tree.force(child, Actor::Operator, FakeMachine { id: 1 })
            .unwrap();
        tree.turn_start(child, "child turn".into(), None).unwrap();
        tree.node_done(child, "done".into()).unwrap();

        // Re-read: the child's event sequence is exactly
        // NodeCreated, Forced, TurnStart, NodeDone, in that order.
        let (_header, events) = LogReader::open(&log_path).expect("reopen log");
        let child_events: Vec<LogEvent> = events
            .map(|r| r.expect("well-formed record").event)
            .filter(|e| event_node(e) == child)
            .collect();
        assert_eq!(child_events.len(), 4);
        assert!(matches!(child_events[0], LogEvent::NodeCreated { .. }));
        assert!(matches!(
            child_events[1],
            LogEvent::Forced {
                actor: Actor::Operator,
                ..
            }
        ));
        assert!(matches!(child_events[2], LogEvent::TurnStart { .. }));
        assert!(matches!(child_events[3], LogEvent::NodeDone { .. }));
    }

    // ---- cursor-paged node-id enumeration --------------------------------

    #[test]
    fn node_ids_after_pages_through_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree_at(&dir.path().join("run.jsonl"));
        let ids: Vec<NodeId> = (0..5)
            .map(|i| {
                tree.create_node(None, &format!("n{i}"), vec![], ForkShape::Exact(0), false)
                    .unwrap()
            })
            .collect();

        let (page1, next1) = tree.node_ids_after(None, 2);
        assert_eq!(page1, ids[0..2]);
        assert_eq!(next1, Some(ids[1]));

        let (page2, next2) = tree.node_ids_after(next1, 2);
        assert_eq!(page2, ids[2..4]);
        assert_eq!(next2, Some(ids[3]));

        let (page3, next3) = tree.node_ids_after(next2, 2);
        assert_eq!(page3, ids[4..5]);
        assert_eq!(next3, None, "the last, partial page has no further cursor");

        // Paging explicitly past the end (cursor = the last real id) is empty.
        let (page4, next4) = tree.node_ids_after(Some(ids[4]), 2);
        assert!(page4.is_empty());
        assert_eq!(next4, None);
    }

    #[test]
    fn node_ids_after_none_with_large_limit_returns_everything() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree_at(&dir.path().join("run.jsonl"));
        let ids: Vec<NodeId> = (0..3)
            .map(|i| {
                tree.create_node(None, &format!("n{i}"), vec![], ForkShape::Exact(0), false)
                    .unwrap()
            })
            .collect();
        let (all, next) = tree.node_ids_after(None, usize::MAX);
        assert_eq!(all, ids);
        assert_eq!(next, None);
    }

    #[test]
    fn node_ids_after_empty_tree_returns_empty_page() {
        let dir = tempfile::tempdir().unwrap();
        let tree: NodeTree<FakeMachine> = tree_at(&dir.path().join("run.jsonl"));
        let (page, next) = tree.node_ids_after(None, 10);
        assert!(page.is_empty());
        assert_eq!(next, None);
    }
}
