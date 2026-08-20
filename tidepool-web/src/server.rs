//! The operator server: axum routes + the SSE stream + [`WebGate`], the
//! [`OperatorGate`] implementation the harness driver blocks on — over N
//! REGISTERED NODES (opaque `node_id` strings, convention = slash-separated
//! tree paths), each holding one node lifecycle: an optional SEED prompt, an
//! append-only TIMELINE of notes and asks, and a FINAL VALUE or FAILURE once
//! the node's window ends.
//!
//! Loopback bind only: reachability is the authorization boundary.
//!
//! # Verbs
//!
//! - `GET  /` — the operator page: every registered node as one outline
//!   section, sorted by path (the default node pinned first).
//! - `GET  /sse` — Datastar `patch-elements` stream, one frame per node-scoped
//!   state change, each replacing that node's `#panel-<node>` in place (the
//!   client MOUNTS a panel it has never seen into the tree — a node born
//!   after page load appears live).
//! - `POST /node/{node}/submit/{interaction}` — resolve one pending form
//!   (identified by its own interaction id, the "nonce" an ask is addressed
//!   by) with a flat `{key: scalar}` body; unparks the matching
//!   `present_form` call.
//! - `POST /node/{node}/continue/{interaction}` — resolve one pending
//!   between-loops gate; unparks the matching `await_continue` call.
//!
//! [`router_with_form_api`] additionally mounts `GET`/`POST
//! /node/{node}/api/form` — a disabled-by-default testing-convenience surface
//! over the SAME pending state; see [`crate::formapi`] for the wire shape and
//! hardening properties.
//!
//! # The timeline is append-only
//!
//! Notes and asks land on a node's timeline in true chronological order and
//! STAY there for the node's lifetime: resolving an ask replaces it IN PLACE
//! with its answered form (what the operator submitted, kept read-only) —
//! it never vanishes, and notes are never cleared. The stream above an ask
//! is that ask's context; erasing either would erase the other's meaning.
//! Publishing never supersedes an existing pending ask on the same node —
//! concurrent cognition windows (fanout/fork `RunLLMTurn`) each get their
//! own entry and coexist until answered, in any resolution order.
//!
//! # The gate
//!
//! [`OperatorGate`] is SYNC-BLOCKING by contract (the driver calls it from a
//! blocking context). A [`WebGate`] is bound to exactly one `node_id`
//! ([`AppState::register_node`] mints it); [`WebGate::present_form`] and
//! [`WebGate::await_continue`] publish a NEW ask onto that node's timeline
//! (pinging the SSE tick for that node), then block the calling thread on a
//! `oneshot` receiver resolved from an HTTP handler. The node-lifecycle
//! extensions ([`WebGate::node_seeded`]/[`WebGate::node_finalized`]/
//! [`WebGate::node_failed`], keyed by label like `retire_node`) store the
//! seed and outcome the driver sends across the seam.
//!
//! # Shape-guided submissions — [`collect_form_json`]
//!
//! [`collect_form_json`] takes the flat `{"<dotted.path>": <scalar>}` object
//! `render::generic_shape`'s markup produces via `shell::JS`'s ordinary flat
//! collector and reassembles it into the PLAIN JSON the answer type's generic
//! `FromJSON` decode reads — record objects, tagged record sums, bare strings
//! for enums, `null` for absent optionals. There is no intermediate answer
//! language.
//!
//! Both browser and form-API submissions use this one conversion.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::Mutex;
use serde_json::{json, Map, Value as Jv};
use tidepool_harness::selfharness::operator::{
    child_path, ContinueSignal, FormShape, OperatorGate, ROOT_BIND_PATH,
};
use tokio::sync::{broadcast, oneshot};

use crate::formapi::FormApiConfig;
use crate::render::{self, NodeView, TimelineEntry};
use crate::shell;

/// An opaque node identity — convention is a slash-separated tree path.
/// Never model-authored: every `node_id` a caller registers traces to a
/// substrate identifier (a wire-carried branch label), same discipline as a
/// form's field labels (see the crate's loopback trust model docs).
pub type NodeId = String;

/// One ask's lifecycle state. Pending states carry the resolution channel
/// that unparks the blocked gate call; answered states are what the pending
/// ones become IN PLACE when resolved — the timeline keeps them.
enum AskState {
    /// A form awaiting submission; `resolve` unparks `present_form`.
    PendingForm {
        shape: FormShape,
        resolve: oneshot::Sender<Jv>,
    },
    /// The between-loops gate; `resolve` unparks `await_continue`.
    PendingContinue {
        resolve: oneshot::Sender<ContinueSignal>,
    },
    /// A resolved form: the reassembled answer the gate actually returned.
    AnsweredForm { shape: FormShape, answer: Jv },
    /// A resolved continue gate, with the operator's message if any.
    AnsweredContinue { input: Option<String> },
}

/// One item on a node's append-only timeline. An `Ask`'s `id` is BOTH its
/// identity and its `data-rev` nonce — assigned once from the node's
/// monotonic counter and never reused, stable for the ask's whole lifetime
/// (pending and answered alike). Seeds, finalized values, and failures are
/// TIMELINE items too, at their true chronological position — one node can
/// live several windows in sequence (the unified root does, every turn).
enum TimelineItem {
    Note(String),
    Seeded(String),
    Finalized(String),
    Failed(String),
    Ask { id: u64, state: AskState },
}

/// How many compiled turn sources the history pane retains per node — enough
/// to scroll back through a working session's recent loops without letting a
/// long-lived process grow the page without bound.
const TURN_HISTORY_CAP: usize = 50;

/// One registered node's state — the minimal node lifecycle: `seed` (the
/// starting prompt, when the wire carried one), the append-only `timeline`
/// of notes and asks, and `final_value`/`failure` once the window ends.
/// `next_ask_id` mints ask ids/nonces; `rev` is this node's AGGREGATE
/// revision, bumped under the SAME lock as every mutation (F10, per-node) —
/// the panel-root `data-rev` the client's focus-preserving skip keys off.
/// `done` marks a retired window ([`OperatorGate::retire_node`]) — the
/// section greys, nothing is removed.
#[derive(Default)]
struct NodeSlot {
    timeline: Vec<TimelineItem>,
    next_ask_id: u64,
    done: bool,
    turn_history: VecDeque<String>,
    rev: u64,
}

impl NodeSlot {
    /// The borrowed render view of this slot.
    fn view<'a>(&'a self, node_id: &'a str) -> NodeView<'a> {
        NodeView {
            node_id,
            timeline: self
                .timeline
                .iter()
                .map(|item| match item {
                    TimelineItem::Note(text) => TimelineEntry::Note(text),
                    TimelineItem::Seeded(seed) => TimelineEntry::Seeded(seed),
                    TimelineItem::Finalized(value) => TimelineEntry::Finalized(value),
                    TimelineItem::Failed(reason) => TimelineEntry::Failed(reason),
                    TimelineItem::Ask { id, state } => match state {
                        AskState::PendingForm { shape, .. } => {
                            TimelineEntry::PendingForm { id: *id, shape }
                        }
                        AskState::PendingContinue { .. } => {
                            TimelineEntry::PendingContinue { id: *id }
                        }
                        AskState::AnsweredForm { shape, answer } => TimelineEntry::AnsweredForm {
                            id: *id,
                            shape,
                            answer,
                        },
                        AskState::AnsweredContinue { input } => TimelineEntry::AnsweredContinue {
                            id: *id,
                            input: input.as_deref(),
                        },
                    },
                })
                .collect(),
            done: self.done,
            turn_history: &self.turn_history,
            rev: self.rev,
        }
    }
}

/// Every registered node, plus registration order — ONE lock over the whole
/// registry (not one per node): with the small node/ask counts this GUI ever
/// holds, a single lock is simpler and structurally rules out cross-node
/// races, at no real concurrency cost.
#[derive(Default)]
struct Registry {
    order: Vec<NodeId>,
    nodes: HashMap<NodeId, NodeSlot>,
}

/// Shared server state: every registered node's lifecycle + the re-render
/// tick.
#[derive(Clone)]
pub struct AppState {
    registry: Arc<Mutex<Registry>>,
    /// Broadcast of "this node's panel changed" — a gate pings this after
    /// any mutation so every open SSE stream re-renders that one node's
    /// panel promptly.
    tick: broadcast::Sender<NodeId>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Display order for the outline: the default (loop) node pinned first, then
/// path-lexicographic — children group under their parent because a child's
/// path extends its parent's. The client's mount-on-first-sight insert uses
/// the same rule, so initial render and live inserts agree.
fn node_order(id: &str) -> (bool, &str) {
    (id != crate::DEFAULT_NODE_ID, id)
}

impl AppState {
    pub fn new() -> Self {
        let (tick, _) = broadcast::channel(64);
        AppState {
            registry: Arc::new(Mutex::new(Registry::default())),
            tick,
        }
    }

    fn ping(&self, node_id: NodeId) {
        let _ = self.tick.send(node_id);
    }

    /// Register a node (idempotent — re-registering an existing id reuses
    /// its state) and return a [`WebGate`] bound to it. This is the ONLY way
    /// to mint a `WebGate` — a driver holds an ordinary `Arc<WebGate>`
    /// exactly as before, now scoped to the node it registered. Pings so a
    /// live page mounts the new node's section immediately.
    pub fn register_node(&self, node_id: impl Into<String>) -> Arc<WebGate> {
        let node_id = node_id.into();
        let changed = {
            let mut reg = self.registry.lock();
            match reg.nodes.get_mut(&node_id) {
                None => {
                    reg.order.push(node_id.clone());
                    reg.nodes.insert(node_id.clone(), NodeSlot::default());
                    true
                }
                // REVIVAL: a new window re-registering a retired label is
                // the node living again (the unified root does this every
                // turn) — clear `done` so the section reads live; the
                // timeline keeps every previous chapter.
                Some(slot) if slot.done => {
                    slot.done = false;
                    slot.rev += 1;
                    true
                }
                Some(_) => false,
            }
        };
        if changed {
            self.ping(node_id.clone());
        }
        Arc::new(WebGate {
            state: self.clone(),
            node_id,
        })
    }

    /// Registered node ids in display order (see [`node_order`]).
    pub fn node_ids(&self) -> Vec<NodeId> {
        let mut ids = self.registry.lock().order.clone();
        ids.sort_by(|a, b| node_order(a).cmp(&node_order(b)));
        ids
    }

    /// Append a new pending ask onto `node_id`'s timeline (NEVER supersedes
    /// an existing one), bump that node's revision, and ping. Returns the
    /// new ask's id (its `data-rev` nonce).
    fn publish_ask(&self, node_id: &str, state: AskState) -> u64 {
        let mut reg = self.registry.lock();
        #[allow(
            clippy::expect_used,
            reason = "WebGate only holds ids from register_node, which always inserts one"
        )]
        let slot = reg
            .nodes
            .get_mut(node_id)
            .expect("WebGate only holds ids from register_node, which always inserts one");
        let id = slot.next_ask_id;
        slot.next_ask_id += 1;
        slot.timeline.push(TimelineItem::Ask { id, state });
        slot.rev += 1;
        drop(reg);
        self.ping(node_id.to_string());
        id
    }

    /// Push a `note` onto `node_id`'s timeline, bump its revision, and ping.
    fn push_note(&self, node_id: &str, text: String) {
        let mut reg = self.registry.lock();
        #[allow(clippy::expect_used, reason = "registered node")]
        let slot = reg.nodes.get_mut(node_id).expect("registered node");
        slot.timeline.push(TimelineItem::Note(text));
        slot.rev += 1;
        drop(reg);
        self.ping(node_id.to_string());
    }

    /// Append a lifecycle event ([`WebGate::node_seeded`]/`node_finalized`/
    /// `node_failed`) to `node_id`'s timeline, bump its revision, and ping.
    fn push_lifecycle(&self, node_id: &str, item: TimelineItem) {
        let mut reg = self.registry.lock();
        #[allow(clippy::expect_used, reason = "registered node")]
        let slot = reg.nodes.get_mut(node_id).expect("registered node");
        slot.timeline.push(item);
        slot.rev += 1;
        drop(reg);
        self.ping(node_id.to_string());
    }

    /// Mark `node_id` done ([`WebGate::retire_node`]) — idempotent; a node
    /// already marked done doesn't force a redundant re-render. The slot and
    /// its whole timeline are never removed — only the derived status
    /// changes.
    fn mark_done(&self, node_id: &str) {
        let mut reg = self.registry.lock();
        #[allow(clippy::expect_used, reason = "registered node")]
        let slot = reg.nodes.get_mut(node_id).expect("registered node");
        if slot.done {
            return;
        }
        slot.done = true;
        slot.rev += 1;
        drop(reg);
        self.ping(node_id.to_string());
    }

    /// Append to `node_id`'s turn history (dropping the oldest past
    /// [`TURN_HISTORY_CAP`]), bump its revision, and ping.
    fn push_turn_source(&self, node_id: &str, source: String) {
        let mut reg = self.registry.lock();
        #[allow(clippy::expect_used, reason = "registered node")]
        let slot = reg.nodes.get_mut(node_id).expect("registered node");
        slot.turn_history.push_back(source);
        if slot.turn_history.len() > TURN_HISTORY_CAP {
            slot.turn_history.pop_front();
        }
        slot.rev += 1;
        drop(reg);
        self.ping(node_id.to_string());
    }

    /// Render one node's `id="panel-<node>"` fragment, or `None` if the node
    /// isn't registered.
    fn node_panel_html(&self, node_id: &str) -> Option<String> {
        let reg = self.registry.lock();
        let slot = reg.nodes.get(node_id)?;
        Some(render::node_panel(&slot.view(node_id)).into_string())
    }

    /// The full page: every registered node's section, in display order.
    fn page_markup(&self) -> maud::Markup {
        let reg = self.registry.lock();
        let mut ids: Vec<&NodeId> = reg.order.iter().collect();
        ids.sort_by(|a, b| node_order(a).cmp(&node_order(b)));
        let sections: Vec<(NodeId, maud::Markup)> = ids
            .into_iter()
            .map(|id| (id.clone(), render::node_panel(&reg.nodes[id].view(id))))
            .collect();
        drop(reg);
        shell::page(sections)
    }

    /// Resolve a pending FORM at `(node_id, interaction)`: reassemble the
    /// flat submission against the pending shape, unpark `present_form`, and
    /// replace the ask IN PLACE with its answered form. Shared by the
    /// browser `/submit` verb and the form-api `POST` — one resolution path,
    /// two front doors. A wrong-kind or stale id never drops any pending
    /// interaction (F10).
    pub(crate) fn resolve_form(
        &self,
        node_id: &str,
        interaction: u64,
        submission: Map<String, Jv>,
    ) -> Result<(), ResolveError> {
        let mut reg = self.registry.lock();
        let slot = reg
            .nodes
            .get_mut(node_id)
            .ok_or(ResolveError::UnknownNode)?;
        let state = find_ask(&mut slot.timeline, interaction)?;
        match state {
            AskState::PendingForm { .. } => {}
            AskState::PendingContinue { .. } => return Err(ResolveError::WrongKind),
            // An already-answered ask stays on the timeline, but addressing
            // it again is the same stale-nonce case as an unknown id.
            _ => return Err(ResolveError::NoSuchInteraction),
        }
        let AskState::PendingForm { shape, resolve } =
            std::mem::replace(state, AskState::AnsweredContinue { input: None })
        else {
            // Checked immediately above, under the same lock.
            unreachable!()
        };
        let answer = answer_value(&shape, submission);
        let _ = resolve.send(answer.clone());
        *state = AskState::AnsweredForm { shape, answer };
        slot.rev += 1;
        drop(reg);
        self.ping(node_id.to_string());
        Ok(())
    }

    /// Resolve a pending CONTINUE gate at `(node_id, interaction)`, keeping
    /// the clicked gate on the timeline as its answered form.
    fn resolve_continue(
        &self,
        node_id: &str,
        interaction: u64,
        signal: ContinueSignal,
    ) -> Result<(), ResolveError> {
        let mut reg = self.registry.lock();
        let slot = reg
            .nodes
            .get_mut(node_id)
            .ok_or(ResolveError::UnknownNode)?;
        let state = find_ask(&mut slot.timeline, interaction)?;
        match state {
            AskState::PendingContinue { .. } => {}
            AskState::PendingForm { .. } => return Err(ResolveError::WrongKind),
            _ => return Err(ResolveError::NoSuchInteraction),
        }
        let AskState::PendingContinue { resolve } =
            std::mem::replace(state, AskState::AnsweredContinue { input: None })
        else {
            // Checked immediately above, under the same lock.
            unreachable!()
        };
        let input = match &signal {
            ContinueSignal::Continue => None,
            ContinueSignal::ContinueWithInput(text) => Some(text.clone()),
        };
        let _ = resolve.send(signal);
        *state = AskState::AnsweredContinue { input };
        slot.rev += 1;
        drop(reg);
        self.ping(node_id.to_string());
        Ok(())
    }

    /// The form-api `GET` view: every currently pending FORM ask (a Continue
    /// gate is never surfaced here) for `node_id`, each paired with its
    /// interaction id (the nonce `POST` must echo back). `Err(())` if
    /// `node_id` isn't registered.
    pub(crate) fn pending_forms(&self, node_id: &str) -> Result<Vec<(u64, FormShape)>, ()> {
        let reg = self.registry.lock();
        let slot = reg.nodes.get(node_id).ok_or(())?;
        Ok(slot
            .timeline
            .iter()
            .filter_map(|item| match item {
                TimelineItem::Ask {
                    id,
                    state: AskState::PendingForm { shape, .. },
                } => Some((*id, shape.clone())),
                _ => None,
            })
            .collect())
    }
}

/// Find the ask with `interaction` on a timeline, in any state — the state
/// distinction (pending/answered/wrong kind) is the caller's to make, so
/// each verb reports the precise refusal.
fn find_ask(
    timeline: &mut [TimelineItem],
    interaction: u64,
) -> Result<&mut AskState, ResolveError> {
    timeline
        .iter_mut()
        .find_map(|item| match item {
            TimelineItem::Ask { id, state } if *id == interaction => Some(state),
            _ => None,
        })
        .ok_or(ResolveError::NoSuchInteraction)
}

/// Why resolving a specific `(node_id, interaction)` failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolveError {
    /// No such registered node.
    UnknownNode,
    /// Nothing pending at that interaction id — already resolved, or never
    /// existed (a stale/guessed nonce).
    NoSuchInteraction,
    /// The interaction exists but isn't the kind this verb resolves (e.g.
    /// `/submit` naming a Continue gate).
    WrongKind,
}

/// A human-legible message for a [`ResolveError`], shared by the browser
/// verbs and the form-api's error responses.
pub(crate) fn resolve_error_message(node_id: &str, err: ResolveError, kind: &str) -> String {
    match err {
        ResolveError::UnknownNode => format!("unknown node {node_id:?}"),
        ResolveError::NoSuchInteraction => {
            "no such pending interaction — already resolved, or the nonce is stale/wrong; GET again for the current pending set".to_string()
        }
        ResolveError::WrongKind => format!("that interaction is not a pending {kind}"),
    }
}

/// The web [`OperatorGate`]: bound to exactly one registered `node_id`.
/// Publishes a NEW ask onto that node's timeline, then BLOCKS the calling
/// (driver) thread until an HTTP handler resolves it. See the module docs on
/// why this is sync-blocking, and why publishing never supersedes.
pub struct WebGate {
    state: AppState,
    node_id: NodeId,
}

impl OperatorGate for WebGate {
    fn present_form(&self, shape: &FormShape) -> Jv {
        let (resolve, wait) = oneshot::channel();
        self.state.publish_ask(
            &self.node_id,
            AskState::PendingForm {
                shape: shape.clone(),
                resolve,
            },
        );
        // The sender is dropped only if the ask is somehow resolved without
        // a value reaching us (should not happen in practice); an empty
        // submission then re-prompts via the Haskell-side decode retry
        // rather than deadlocking the driver.
        wait.blocking_recv().unwrap_or_else(|_| json!({}))
    }

    fn await_continue(&self) -> ContinueSignal {
        let (resolve, wait) = oneshot::channel();
        self.state
            .publish_ask(&self.node_id, AskState::PendingContinue { resolve });
        wait.blocking_recv().unwrap_or(ContinueSignal::Continue)
    }

    fn post_note(&self, text: &str) {
        self.state.push_note(&self.node_id, text.to_string());
    }

    fn post_turn_source(&self, source: &str) {
        self.state
            .push_turn_source(&self.node_id, source.to_string());
    }

    /// Register (or reuse — [`AppState::register_node`] is idempotent) a
    /// node for `label` and hand back its own gate, so a labeled branch
    /// child's asks/notes render on their own section instead of this
    /// gate's.
    fn node_gate(&self, label: &str) -> Option<Arc<dyn OperatorGate>> {
        Some(self.state.register_node(label))
    }

    fn retire_node(&self, label: &str) {
        self.state.mark_done(label);
    }

    fn node_seeded(&self, label: &str, seed: &str) {
        self.state
            .push_lifecycle(label, TimelineItem::Seeded(seed.to_string()));
    }

    fn node_finalized(&self, label: &str, value: &str) {
        self.state
            .push_lifecycle(label, TimelineItem::Finalized(value.to_string()));
    }

    fn node_failed(&self, label: &str, reason: &str) {
        self.state
            .push_lifecycle(label, TimelineItem::Failed(reason.to_string()));
    }
}

/// Build the axum router over the app state. The form-api testing surface is
/// disabled — equivalent to `router_with_form_api(state, FormApiConfig::default())`.
pub fn router(state: AppState) -> Router {
    router_with_form_api(state, FormApiConfig::default())
}

/// Build the axum router, optionally mounting the form-api testing surface
/// (`crate::formapi`) alongside the browser verbs. See that module's docs
/// for the hardening properties `form_api` gates.
pub fn router_with_form_api(state: AppState, form_api: FormApiConfig) -> Router {
    let base: Router<AppState> = Router::new()
        .route("/", get(page))
        .route("/sse", get(sse))
        .route("/node/{node}/submit/{interaction}", post(submit))
        .route("/node/{node}/continue/{interaction}", post(continue_loop))
        .fallback(not_found);
    crate::formapi::merge(base, form_api).with_state(state)
}

async fn page(State(st): State<AppState>) -> Html<String> {
    Html(st.page_markup().into_string())
}

/// The SSE stream: one Datastar `patch-elements` frame per node-scoped tick,
/// each carrying that node's freshly rendered panel. Initial frames go out
/// immediately for every currently registered node so a page opened
/// mid-interaction is correct without waiting for a tick. A tick for a node
/// the page has never seen still renders — the client mounts it into the
/// tree on first sight.
async fn sse(
    State(st): State<AppState>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    use tokio_stream::wrappers::BroadcastStream;
    use tokio_stream::StreamExt;

    let initial_frames: Vec<Result<Event, Infallible>> = st
        .node_ids()
        .iter()
        .filter_map(|id| st.node_panel_html(id))
        .map(|html| Ok(frame_html(html)))
        .collect();
    let initial = tokio_stream::iter(initial_frames);

    let st_stream = st.clone();
    let updates = BroadcastStream::new(st.tick.subscribe()).filter_map(move |r| {
        let node_id = r.ok()?;
        let html = st_stream.node_panel_html(&node_id)?;
        Some(Ok(frame_html(html)))
    });

    Sse::new(initial.chain(updates)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

/// One patch-elements frame carrying an already-rendered node panel.
fn frame_html(html: String) -> Event {
    use datastar::prelude::PatchElements;
    PatchElements::new(html).write_as_axum_sse_event()
}

/// Resolve the pending form at `(node, interaction)`. The client posts a
/// flat dotted-path object; the shape-guided collector below validates and
/// reassembles it.
async fn submit(
    State(st): State<AppState>,
    Path((node, interaction)): Path<(String, u64)>,
    body: Option<Json<Jv>>,
) -> Response {
    let raw = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let Jv::Object(submission) = raw else {
        return err_json("submission must be a flat JSON object".to_string());
    };
    match st.resolve_form(&node, interaction, submission) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => err_json(resolve_error_message(&node, e, "form")),
    }
}

/// Resolve the pending continue gate at `(node, interaction)`. The body is
/// the [`render::continue_shape`] sum's flat submission, reassembled by the
/// SAME machinery as `/submit`: `{"tag": "Continue"}` or `{"tag":
/// "ContinueWithInput", "input": ...}`. An absent/empty/unshaped body
/// degrades to a bare continue — the no-body click (and every existing
/// test/curl) still works, and a chosen-but-empty message is a bare continue
/// too.
async fn continue_loop(
    State(st): State<AppState>,
    Path((node, interaction)): Path<(String, u64)>,
    body: Option<Json<Jv>>,
) -> Response {
    let signal = body
        .and_then(|Json(v)| match v {
            Jv::Object(submission) => {
                Some(answer_value(&crate::render::continue_shape(), submission))
            }
            _ => None,
        })
        .and_then(|answer| {
            let tag = answer.get("tag")?.as_str()?.to_string();
            match tag.as_str() {
                "ContinueWithInput" => answer
                    .get("input")
                    .and_then(|i| i.as_str())
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .map(ContinueSignal::ContinueWithInput),
                _ => Some(ContinueSignal::Continue),
            }
        })
        .unwrap_or(ContinueSignal::Continue);
    match st.resolve_continue(&node, interaction, signal) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => err_json(resolve_error_message(&node, e, "continue gate")),
    }
}

fn err_json(msg: String) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(json!({"ok": false, "error": msg})),
    )
        .into_response()
}

/// Any route that matches none of the above — a malformed node path, a typo,
/// a probing curl. Axum's default fallback is an empty-body 404, invisible
/// to the client JS toast and unhelpful to an agent/curl operator; this keeps
/// every response on the same `{"ok": false, "error": ...}` JSON shape the
/// matched handlers speak, just with 404 instead of `err_json`'s 400.
async fn not_found() -> Response {
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(json!({"ok": false, "error": "no such route"})),
    )
        .into_response()
}

// -------------------------------------------------------------------------
// Nested submissions — flat wire object -> the plain JSON answer.
// -------------------------------------------------------------------------

/// Convert a client's flat POST to the answer returned by the gate. The
/// browser `/submit` and form API share this path. It reassembles dotted bind
/// paths into the ordinary JSON the answer type's generic `FromJSON` decode
/// reads; there is no intermediate answer language. An incomplete or
/// wrong-typed submission resolves to `{}`, which that decode rejects and
/// re-presents — the same path every malformed submission takes.
fn answer_value(shape: &FormShape, submission: Map<String, Jv>) -> Jv {
    collect_form_json(shape, ROOT_BIND_PATH, &submission).unwrap_or_else(|| json!({}))
}

/// Whether every variant of a sum is nullary — an enum, whose answer is the
/// chosen constructor as a bare string (matching the generic decode's
/// all-nullary rule). A mixed sum answers as a tagged object instead.
fn all_nullary(variants: &[tidepool_harness::selfharness::operator::VariantShape]) -> bool {
    variants
        .iter()
        .all(|v| matches!(&v.shape, FormShape::Product { fields, .. } if fields.is_empty()))
}

/// Reassemble a flat `{"<dotted.path>": <scalar>}` submission (exactly what
/// `render::generic_shape`'s markup, collected by `shell::JS`'s ordinary
/// flat `[data-bind]` walk, produces) into the plain JSON the generic
/// `FromJSON` decode accepts, guided by the [`FormShape`] the form was
/// rendered from. `path` is the root bind path used at render time (`""`
/// for a form rendered at the root).
///
/// The JSON per shape: leaves are the corresponding scalar; a record is an
/// object of its fields; an all-nullary sum is the chosen constructor as a
/// bare string; a payload sum is a tagged object (`{"tag": <ctor>, ...}` —
/// the chosen variant's record fields merged beside the tag, tag-only for a
/// nullary branch); `Maybe` is the value or `null`; the unit form is `null`.
///
/// Rejects rather than guesses: a missing leaf, a wrong-typed scalar, or an
/// unrecognized sum constructor all yield `None`. For a payload-bearing sum,
/// only the CHOSEN variant's fields are read — the other variants' inputs
/// are present in `raw` (they're always rendered) but their keys are never
/// looked at, so a non-chosen branch's leftover values never leak into the
/// answer.
#[must_use]
pub fn collect_form_json(shape: &FormShape, path: &str, raw: &Map<String, Jv>) -> Option<Jv> {
    match shape {
        FormShape::String => match raw.get(path)? {
            Jv::String(s) => Some(Jv::String(s.clone())),
            _ => None,
        },
        FormShape::Int => match raw.get(path)? {
            n @ Jv::Number(_) if n.as_i64().is_some() => Some(n.clone()),
            _ => None,
        },
        FormShape::Number => match raw.get(path)? {
            n @ Jv::Number(_) if n.as_f64().is_some() => Some(n.clone()),
            _ => None,
        },
        FormShape::Bool => match raw.get(path)? {
            b @ Jv::Bool(_) => Some(b.clone()),
            _ => None,
        },
        FormShape::Unit => Some(Jv::Null),
        FormShape::Optional(inner) => {
            let present_key = format!("{path}#present");
            let present = matches!(raw.get(&present_key), Some(Jv::Bool(true)));
            if present {
                collect_form_json(inner, path, raw)
            } else {
                Some(Jv::Null)
            }
        }
        FormShape::Product { fields, .. } => {
            let mut out = Map::new();
            for field in fields {
                let child = child_path(path, &field.key);
                let value = collect_form_json(&field.shape, &child, raw)?;
                out.insert(field.key.clone(), value);
            }
            Some(Jv::Object(out))
        }
        FormShape::Sum { variants, .. } => {
            let chosen = match raw.get(path)? {
                Jv::String(s) => s.clone(),
                _ => return None,
            };
            let variant = variants.iter().find(|v| v.constructor == chosen)?;
            if all_nullary(variants) {
                // Enum: the bare constructor string.
                return Some(Jv::String(chosen));
            }
            let child = child_path(path, &variant.constructor);
            let mut out = Map::new();
            out.insert("tag".to_string(), Jv::String(chosen));
            match &variant.shape {
                FormShape::Product { fields, .. } => {
                    for field in fields {
                        let fchild = child_path(&child, &field.key);
                        let value = collect_form_json(&field.shape, &fchild, raw)?;
                        out.insert(field.key.clone(), value);
                    }
                }
                // GForm derives constructor payloads as Product shapes.
                _ => return None,
            }
            Some(Jv::Object(out))
        }
    }
}

#[cfg(test)]
impl AppState {
    /// Test-only convenience: the id of the first (oldest) PENDING ask on
    /// `node_id`, if any.
    fn first_ask_id(&self, node_id: &str) -> Option<u64> {
        self.pending_ids(node_id).first().copied()
    }

    /// Test-only: every pending ask id on `node_id`, in publish order.
    fn pending_ids(&self, node_id: &str) -> Vec<u64> {
        self.registry.lock().nodes[node_id]
            .timeline
            .iter()
            .filter_map(|item| match item {
                TimelineItem::Ask {
                    id,
                    state: AskState::PendingForm { .. } | AskState::PendingContinue { .. },
                } => Some(*id),
                _ => None,
            })
            .collect()
    }

    fn ask_count(&self, node_id: &str) -> usize {
        self.pending_ids(node_id).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_harness::selfharness::operator::FieldShape;

    fn spec() -> FormShape {
        FormShape::Product {
            type_key: "Sample".into(),
            constructor: "Sample".into(),
            fields: vec![
                FieldShape {
                    key: "mood".into(),
                    shape: FormShape::String,
                    doc: None,
                },
                FieldShape {
                    key: "count".into(),
                    shape: FormShape::Int,
                    doc: None,
                },
            ],
            doc: None,
        }
    }

    /// The full gate round trip: `present_form` blocks on a worker thread
    /// while the test resolves it through the same path `POST /submit` uses.
    #[test]
    fn present_form_blocks_until_submitted() {
        let st = AppState::new();
        let gate = st.register_node("n1");
        let handle = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.present_form(&spec()))
        };

        while st.first_ask_id("n1").is_none() {
            std::thread::yield_now();
        }
        let interaction = st.first_ask_id("n1").unwrap();

        // Wire submission: flat, dotted, rooted at ROOT_BIND_PATH — the same
        // shape the browser/form-api collectors produce.
        let wire = json!({"answer.mood": "calm", "answer.count": 3});
        st.resolve_form("n1", interaction, wire.as_object().unwrap().clone())
            .unwrap();

        let got = handle.join().unwrap();
        assert_eq!(got, json!({"mood": "calm", "count": 3}));
    }

    /// The regression this transport exists for: a unit-shaped form
    /// (`askUser @()`) answers as `null`.
    /// The old object-only transport coerced non-object answers to `{}`,
    /// which the decode rejects — an infinite re-prompt.
    #[test]
    fn unit_shaped_answer_survives_reassembly() {
        assert_eq!(answer_value(&FormShape::Unit, Map::new()), Jv::Null);
    }

    /// An incomplete shape submission still degrades to `{}` (reject and
    /// re-present), not a panic and not a partial answer.
    #[test]
    fn incomplete_shape_submission_degrades_to_the_rejectable_empty_object() {
        assert_eq!(answer_value(&FormShape::String, Map::new()), json!({}));
    }

    #[test]
    fn await_continue_blocks_until_resolved() {
        let st = AppState::new();
        let gate = st.register_node("n1");
        let handle = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.await_continue())
        };

        while st.first_ask_id("n1").is_none() {
            std::thread::yield_now();
        }
        let interaction = st.first_ask_id("n1").unwrap();
        st.resolve_continue("n1", interaction, ContinueSignal::Continue)
            .unwrap();
        handle.join().unwrap();
    }

    /// The timeline is append-only: an answered ask stays IN PLACE with its
    /// answer (rendered read-only), and re-addressing its id is the stale
    /// case, not a second resolution.
    #[test]
    fn answered_ask_persists_in_place_with_its_answer() {
        let st = AppState::new();
        let gate = st.register_node("n1");
        gate.post_note("about to ask");
        let handle = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.present_form(&spec()))
        };
        while st.first_ask_id("n1").is_none() {
            std::thread::yield_now();
        }
        let interaction = st.first_ask_id("n1").unwrap();
        let wire = json!({"answer.mood": "calm", "answer.count": 3});
        st.resolve_form("n1", interaction, wire.as_object().unwrap().clone())
            .unwrap();
        handle.join().unwrap();

        assert_eq!(st.ask_count("n1"), 0, "nothing pending anymore");
        let html = st.node_panel_html("n1").unwrap();
        assert!(html.contains("about to ask"), "the note stays: {html}");
        assert!(
            html.contains(&format!("id=\"ask-n1-{interaction}\"")),
            "the answered ask stays at its id: {html}"
        );
        assert!(html.contains("calm"), "the answer shows: {html}");
        assert!(
            !html.contains(&format!("@post('/node/n1/submit/{interaction}')")),
            "no live form controls on an answered ask: {html}"
        );

        let err = st
            .resolve_form("n1", interaction, Map::new())
            .expect_err("re-resolving an answered ask is stale");
        assert_eq!(err, ResolveError::NoSuchInteraction);
    }

    /// Notes survive the between-loops gate — the timeline is the node's
    /// history, and a continue click must not erase the context above it.
    #[test]
    fn notes_persist_across_continue() {
        let st = AppState::new();
        let gate = st.register_node("n1");
        gate.post_note("turn 1 narration");
        let handle = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.await_continue())
        };
        while st.first_ask_id("n1").is_none() {
            std::thread::yield_now();
        }
        let interaction = st.first_ask_id("n1").unwrap();
        st.resolve_continue(
            "n1",
            interaction,
            ContinueSignal::ContinueWithInput("steer".to_string()),
        )
        .unwrap();
        handle.join().unwrap();

        let html = st.node_panel_html("n1").unwrap();
        assert!(
            html.contains("turn 1 narration"),
            "notes persist across continue: {html}"
        );
        assert!(
            html.contains("steer"),
            "the operator's continue message stays visible: {html}"
        );
    }

    /// The node-lifecycle events append to the timeline, bump the revision,
    /// and show up in the rendered panel with the derived status.
    #[test]
    fn seed_final_and_failure_store_and_render() {
        let st = AppState::new();
        let gate = st.register_node("root");
        let child = gate.node_gate("root/1-x").expect("child gate");
        drop(child);

        let rev0 = extract_rev(&st.node_panel_html("root/1-x").unwrap()).to_string();
        gate.node_seeded("root/1-x", "NODE root/1 — DISCOVER the thing");
        let html = st.node_panel_html("root/1-x").unwrap();
        assert!(html.contains("DISCOVER the thing"), "{html}");
        let rev1 = extract_rev(&html).to_string();
        assert_ne!(rev0, rev1, "seed bumps rev");

        gate.retire_node("root/1-x");
        gate.node_finalized("root/1-x", "{\"tag\":\"FinishLayer\"}");
        let html = st.node_panel_html("root/1-x").unwrap();
        assert!(html.contains("Final value"), "{html}");
        assert!(html.contains("FinishLayer"), "{html}");
        assert!(html.contains(">done</span>"), "{html}");

        gate.node_failed("root/1-x", "round exhaustion");
        let html = st.node_panel_html("root/1-x").unwrap();
        assert!(html.contains("round exhaustion"), "{html}");
        assert!(html.contains(">failed</span>"), "{html}");
    }

    /// The unified-root lifecycle: a retired node re-registered under the
    /// same label REVIVES (done clears, timeline keeps every chapter), and a
    /// pending ask on a done node outranks its done-ness — the root is done
    /// at every fold while its between-turns gate is pending.
    #[test]
    fn re_registering_a_done_label_revives_the_node() {
        let st = AppState::new();
        let gate = st.register_node("root");

        // Turn 1's window: seed, finalize, retire.
        let _w1 = gate.node_gate("root").expect("root window gate");
        gate.node_seeded("root", "turn 1 brief");
        gate.retire_node("root");
        gate.node_finalized("root", "\"turn 1 answer\"");
        let html = st.node_panel_html("root").unwrap();
        assert!(html.contains(">done</span>"), "{html}");

        // The between-turns gate arrives on the SAME (done) node: needs you.
        let g = gate.clone();
        let handle = std::thread::spawn(move || g.await_continue());
        while st.first_ask_id("root").is_none() {
            std::thread::yield_now();
        }
        let html = st.node_panel_html("root").unwrap();
        assert!(
            html.contains(">needs you</span>"),
            "a pending gate outranks done: {html}"
        );
        let interaction = st.first_ask_id("root").unwrap();
        st.resolve_continue("root", interaction, ContinueSignal::Continue)
            .unwrap();
        handle.join().unwrap();

        // Turn 2's window re-registers the label: the node revives...
        let _w2 = gate.node_gate("root").expect("revived root window gate");
        gate.node_seeded("root", "turn 2 brief");
        let html = st.node_panel_html("root").unwrap();
        assert!(html.contains(">running</span>"), "revived: {html}");
        // ...with every chapter still on the timeline, in order.
        let t1 = html.find("turn 1 brief").expect("turn 1 seed kept");
        let a1 = html.find("turn 1 answer").expect("turn 1 value kept");
        let t2 = html.find("turn 2 brief").expect("turn 2 seed present");
        assert!(t1 < a1 && a1 < t2, "{html}");
        // Exactly one outline entry for the label throughout.
        assert_eq!(
            st.node_ids().iter().filter(|id| *id == "root").count(),
            1,
            "revival never duplicates the node"
        );
    }

    /// The concurrency invariant the whole generalization exists for: two
    /// asks published on the SAME node coexist (neither supersedes the
    /// other) and each resolves independently, in either order.
    #[test]
    fn two_concurrent_asks_on_one_node_both_render_and_resolve_independently() {
        let st = AppState::new();
        let gate = st.register_node("n1");

        let g1 = gate.clone();
        let h1 = std::thread::spawn(move || g1.present_form(&spec()));
        let g2 = gate.clone();
        let h2 = std::thread::spawn(move || g2.present_form(&spec()));

        while st.ask_count("n1") < 2 {
            std::thread::yield_now();
        }
        let ids = st.pending_ids("n1");
        assert_eq!(ids.len(), 2, "both asks coexist, neither dropped");
        assert_ne!(ids[0], ids[1], "each ask has its own nonce");

        // Resolve in REVERSE order — the second-published ask first. Wire
        // submissions are flat, dotted, rooted at ROOT_BIND_PATH.
        st.resolve_form(
            "n1",
            ids[1],
            json!({"answer.mood": "b", "answer.count": 2})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        assert_eq!(
            st.ask_count("n1"),
            1,
            "only the resolved ask leaves pending"
        );
        st.resolve_form(
            "n1",
            ids[0],
            json!({"answer.mood": "a", "answer.count": 1})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        assert_eq!(st.ask_count("n1"), 0);

        // Which internal `present_form` call happened to land at `ids[0]` vs
        // `ids[1]` is a scheduling detail; compare the two results as a SET.
        let mut got = vec![h1.join().unwrap(), h2.join().unwrap()];
        got.sort_by_key(|v| v["count"].as_i64().unwrap());
        assert_eq!(
            got,
            vec![
                json!({"mood": "a", "count": 1}),
                json!({"mood": "b", "count": 2}),
            ]
        );
    }

    /// A stale/unknown interaction id is rejected without touching whatever
    /// else is pending.
    #[test]
    fn resolve_with_unknown_interaction_is_rejected() {
        let st = AppState::new();
        let _gate = st.register_node("n1");
        let err = st
            .resolve_form("n1", 999, Map::new())
            .expect_err("no such interaction");
        assert_eq!(err, ResolveError::NoSuchInteraction);
    }

    /// A wrong-kind resolution (submitting to a Continue-gate interaction) is
    /// rejected and leaves the interaction pending.
    #[test]
    fn resolve_form_on_a_continue_interaction_is_rejected_and_preserved() {
        let st = AppState::new();
        let gate = st.register_node("n1");
        let handle = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.await_continue())
        };
        while st.first_ask_id("n1").is_none() {
            std::thread::yield_now();
        }
        let interaction = st.first_ask_id("n1").unwrap();

        let err = st
            .resolve_form("n1", interaction, Map::new())
            .expect_err("wrong kind");
        assert_eq!(err, ResolveError::WrongKind);
        assert_eq!(st.ask_count("n1"), 1, "the continue gate is still pending");

        st.resolve_continue("n1", interaction, ContinueSignal::Continue)
            .unwrap();
        handle.join().unwrap();
    }

    fn extract_rev(html: &str) -> &str {
        let after = html
            .split("data-rev=\"")
            .nth(1)
            .expect("panel html carries data-rev");
        after.split('"').next().unwrap()
    }

    /// `node_gate` registers (idempotently) a node for the label and hands
    /// back its own gate; `retire_node` marks it done, which bumps the
    /// panel's revision but leaves the slot (and its whole timeline) in
    /// place — retiring an already-done node is a no-op ping.
    #[test]
    fn node_gate_registers_and_retire_node_marks_done() {
        let st = AppState::new();
        let default_gate = st.register_node("root");

        let _child_gate = default_gate
            .node_gate("root/1-x")
            .expect("node_gate registers a gate");
        let _child_gate_again = default_gate
            .node_gate("root/1-x")
            .expect("re-registering the same label is idempotent");
        // `register_node` is idempotent on the underlying STATE (a fresh
        // `WebGate` wrapper each call, same node) — re-resolving the same
        // label must not duplicate the outline entry.
        assert_eq!(
            st.node_ids().iter().filter(|id| *id == "root/1-x").count(),
            1,
            "re-resolving the same label must not register a second node"
        );

        let rev0 = extract_rev(&st.node_panel_html("root/1-x").unwrap()).to_string();
        default_gate.retire_node("root/1-x");
        let rev1 = extract_rev(&st.node_panel_html("root/1-x").unwrap()).to_string();
        assert_ne!(rev0, rev1, "retire_node must bump the revision");

        default_gate.retire_node("root/1-x");
        let rev2 = extract_rev(&st.node_panel_html("root/1-x").unwrap()).to_string();
        assert_eq!(rev1, rev2, "retiring an already-done node is a no-op");
    }

    /// Display order: the default (root) node first — even ahead of ids that
    /// sort before it lexicographically — then path-lexicographic, so
    /// children group under their parents regardless of registration order.
    #[test]
    fn node_ids_sort_default_first_then_by_path() {
        let st = AppState::new();
        let _b = st.register_node("root/2-y");
        let _a = st.register_node("alpha");
        let _d = st.register_node(crate::DEFAULT_NODE_ID);
        let _c = st.register_node("root/1-x");
        assert_eq!(
            st.node_ids(),
            vec![
                crate::DEFAULT_NODE_ID.to_string(),
                "alpha".to_string(),
                "root/1-x".to_string(),
                "root/2-y".to_string(),
            ]
        );
    }

    /// F10 (generalized per-node): every mutation to a node's state bumps
    /// the revision stamped into that node's rendered panel root.
    #[test]
    fn panel_html_data_rev_bumps_on_every_mutation() {
        let st = AppState::new();
        let gate = st.register_node("n1");
        let rev0 = extract_rev(&st.node_panel_html("n1").unwrap()).to_string();

        let handle = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.await_continue())
        };
        while st.first_ask_id("n1").is_none() {
            std::thread::yield_now();
        }
        let rev1 = extract_rev(&st.node_panel_html("n1").unwrap()).to_string();
        assert_ne!(rev0, rev1, "publish must bump the revision");

        let interaction = st.first_ask_id("n1").unwrap();
        st.resolve_continue("n1", interaction, ContinueSignal::Continue)
            .unwrap();
        let rev2 = extract_rev(&st.node_panel_html("n1").unwrap()).to_string();
        assert_ne!(rev1, rev2, "resolving must bump the revision");
        handle.join().unwrap();
    }

    // ---- collect_form_json ---------------------------------------------------

    use tidepool_harness::selfharness::operator::VariantShape;

    fn ssh_shape() -> FormShape {
        FormShape::Product {
            type_key: "Ssh".to_string(),
            constructor: "Ssh".to_string(),
            fields: vec![
                FieldShape {
                    key: "host".to_string(),
                    shape: FormShape::String,
                    doc: None,
                },
                FieldShape {
                    key: "port".to_string(),
                    shape: FormShape::Int,
                    doc: None,
                },
            ],
            doc: None,
        }
    }

    fn destination_shape() -> FormShape {
        FormShape::Sum {
            type_key: "Destination".to_string(),
            variants: vec![
                VariantShape {
                    constructor: "LocalHost".to_string(),
                    shape: empty_product("Destination", "LocalHost"),
                },
                VariantShape {
                    constructor: "Ssh".to_string(),
                    shape: ssh_shape(),
                },
            ],
            doc: None,
        }
    }

    fn deploy_request_shape() -> FormShape {
        FormShape::Product {
            type_key: "DeployRequest".to_string(),
            constructor: "DeployRequest".to_string(),
            fields: vec![
                FieldShape {
                    key: "service".to_string(),
                    shape: FormShape::String,
                    doc: None,
                },
                FieldShape {
                    key: "destination".to_string(),
                    shape: destination_shape(),
                    doc: None,
                },
                FieldShape {
                    key: "releaseNote".to_string(),
                    shape: FormShape::Optional(Box::new(FormShape::String)),
                    doc: None,
                },
            ],
            doc: None,
        }
    }

    fn obj(v: Jv) -> Map<String, Jv> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn collect_form_json_reassembles_nested_product_of_sum() {
        let raw = obj(json!({
            "service": "api",
            "destination": "Ssh",
            "destination.Ssh.host": "example.com",
            "destination.Ssh.port": 22,
            "releaseNote#present": true,
            "releaseNote": "hotfix",
        }));
        assert_eq!(
            collect_form_json(&deploy_request_shape(), "", &raw),
            Some(json!({
                "service": "api",
                "destination": {"tag": "Ssh", "host": "example.com", "port": 22},
                "releaseNote": "hotfix"
            }))
        );
    }

    /// The LIVE recursive path, both halves from the same root: a spec
    /// carrying a `shape` renders through the panel at [`ROOT_BIND_PATH`],
    /// and [`submit`] collects from that same root.
    ///
    /// A root SUM (what `choose` and `askUser @<enum>` produce) is the case
    /// that pins why the root is NON-EMPTY: its radios are grouped by `name`,
    /// and HTML does not group radios sharing an empty one — an empty root
    /// would let the operator check two branches of one choice.
    #[test]
    fn root_bind_path_renders_and_collects_a_root_sum() {
        let shape = destination_shape();
        let th = VecDeque::new();
        let view = NodeView {
            node_id: "n1",
            timeline: vec![TimelineEntry::PendingForm {
                id: 0,
                shape: &shape,
            }],
            done: false,
            turn_history: &th,
            rev: 1,
        };
        let html = crate::render::node_panel(&view).into_string();
        assert!(
            html.contains(&format!("name=\"{ROOT_BIND_PATH}\"")),
            "a root sum's radio group must be named at the non-empty root bind path, got:\n{html}"
        );

        let mut raw = Map::new();
        raw.insert(ROOT_BIND_PATH.to_string(), json!("LocalHost"));
        // Destination is a MIXED sum (Ssh carries fields), so even the
        // nullary branch answers as a tag-only object, not a bare string.
        assert_eq!(
            collect_form_json(&destination_shape(), ROOT_BIND_PATH, &raw),
            Some(json!({"tag": "LocalHost"}))
        );
    }

    /// A payload-bearing sum always renders every branch's inputs at once
    /// (see `render.rs`); the collector must read only the CHOSEN branch and
    /// ignore stray values left over from the unselected one(s).
    #[test]
    fn collect_form_json_ignores_unselected_branch_fields() {
        let raw = obj(json!({
            "service": "api",
            "destination": "LocalHost",
            "destination.Ssh.host": "example.com",
            "destination.Ssh.port": 22,
            "releaseNote#present": false,
            "releaseNote": "ignored because presence is false",
        }));
        assert_eq!(
            collect_form_json(&deploy_request_shape(), "", &raw),
            Some(json!({
                "service": "api",
                "destination": {"tag": "LocalHost"},
                "releaseNote": null
            }))
        );
    }

    /// An ALL-nullary sum (an enum) answers as the bare constructor string —
    /// the wire the generic decode reads for enum types.
    #[test]
    fn collect_form_json_enum_answers_as_bare_string() {
        let shape = FormShape::Sum {
            type_key: "Env".to_string(),
            variants: vec![
                VariantShape {
                    constructor: "Dev".to_string(),
                    shape: empty_product("Env", "Dev"),
                },
                VariantShape {
                    constructor: "Prod".to_string(),
                    shape: empty_product("Env", "Prod"),
                },
            ],
            doc: None,
        };
        let mut raw = Map::new();
        raw.insert(ROOT_BIND_PATH.to_string(), json!("Prod"));
        assert_eq!(
            collect_form_json(&shape, ROOT_BIND_PATH, &raw),
            Some(json!("Prod"))
        );
    }

    fn empty_product(type_key: &str, constructor: &str) -> FormShape {
        FormShape::Product {
            type_key: type_key.to_string(),
            constructor: constructor.to_string(),
            fields: vec![],
            doc: None,
        }
    }

    #[test]
    fn collect_form_json_rejects_missing_field() {
        let raw = obj(json!({ "service": "api" }));
        assert_eq!(collect_form_json(&deploy_request_shape(), "", &raw), None);
    }

    #[test]
    fn collect_form_json_rejects_unknown_constructor() {
        let raw = obj(json!({
            "service": "api",
            "destination": "Nope",
            "releaseNote#present": false,
        }));
        assert_eq!(collect_form_json(&deploy_request_shape(), "", &raw), None);
    }

    /// DONE criterion: the display-humanization / exact-key split, asserted
    /// end to end. Render the shape, scrape the exact bind paths back out of
    /// the HTML (not a hand-written guess at what the renderer emits), build
    /// a submission at exactly those paths, and confirm
    /// `collect_form_json` reconstructs the same exact keys — while the
    /// rendered HTML shows only humanized label text, never the raw key.
    #[test]
    fn render_and_collect_round_trip_exact_keys_while_labels_humanize() {
        use crate::render::generic_shape;

        let shape = deploy_request_shape();
        let html = generic_shape("", &shape).into_string();

        assert!(html.contains("data-bind=\"service\""));
        assert!(html.contains("data-bind=\"destination\""));
        assert!(html.contains("data-bind=\"releaseNote#present\""));
        assert!(html.contains("data-bind=\"releaseNote\""));
        assert!(html.contains("data-bind=\"destination.Ssh.host\""));
        assert!(html.contains("data-bind=\"destination.Ssh.port\""));
        assert!(html.contains("Release note"));
        assert!(!html.contains(">releaseNote<"));

        let raw = obj(json!({
            "service": "api",
            "destination": "Ssh",
            "destination.Ssh.host": "example.com",
            "destination.Ssh.port": 22,
            "releaseNote#present": true,
            "releaseNote": "hotfix",
        }));
        let answer = collect_form_json(&shape, "", &raw).expect("collects");
        let obj = answer.as_object().expect("a record collects to an object");
        let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["destination", "releaseNote", "service"]);
    }
}
