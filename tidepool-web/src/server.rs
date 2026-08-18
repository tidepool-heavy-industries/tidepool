//! The operator server: axum routes + the SSE stream + [`WebGate`], the
//! [`OperatorGate`] implementation the harness driver blocks on — now
//! generalized to N REGISTERED NODES (opaque `node_id` strings, convention =
//! branch name), each rendered as its own tab, each able to carry SEVERAL
//! concurrently pending asks (an operator gate never hides a question by
//! superseding an unanswered one).
//!
//! Loopback bind only: reachability is the authorization boundary.
//!
//! # Verbs
//!
//! - `GET  /` — the operator page: a tab strip across every registered node
//!   plus every node's panel.
//! - `GET  /sse` — Datastar `patch-elements` stream, one frame per node-scoped
//!   state change, each replacing that node's `#panel-<node>` in place.
//! - `POST /node/{node}/submit/{interaction}` — resolve one pending form
//!   (identified by its own interaction id, the "nonce" a stacked ask is
//!   addressed by) with a flat `{key: scalar}` body; unparks the matching
//!   `present_form` call.
//! - `POST /node/{node}/continue/{interaction}` — resolve one pending
//!   between-loops gate; unparks the matching `await_continue` call.
//!
//! [`router_with_form_api`] additionally mounts `GET`/`POST
//! /node/{node}/api/form` — a disabled-by-default testing-convenience surface
//! over the SAME pending state; see [`crate::formapi`] for the wire shape and
//! hardening properties.
//!
//! # The gate
//!
//! [`OperatorGate`] is SYNC-BLOCKING by contract (the driver calls it from a
//! blocking context). A [`WebGate`] is bound to exactly one `node_id`
//! ([`AppState::register_node`] mints it); [`WebGate::present_form`] and
//! [`WebGate::await_continue`] publish a NEW ask onto that node's stack
//! (pinging the SSE tick for that node), then block the calling thread on a
//! `oneshot` receiver resolved from an HTTP handler. No async runtime is
//! entered on the driver's side (`blocking_recv`), so this composes with the
//! driver's `block_in_place`/`block_on` turn driving.
//!
//! Publishing NEVER supersedes an existing pending ask on the same node — the
//! whole point of the stack is that concurrent cognition windows (fanout/fork
//! `RunLLMTurn`) each get their own slot and coexist until answered.
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
use crate::render::{self, Ask};
use crate::shell;

/// An opaque node identity — convention is the node's branch name. Never
/// model-authored: every `node_id` a caller registers traces to a substrate
/// identifier, same discipline as a form's field labels (see the crate's
/// loopback trust model docs).
pub type NodeId = String;

/// What one pending ask resolves to. Carries the resolution channel;
/// [`OperatorGate::present_form`]/`await_continue`'s answer value shape.
enum Pending {
    /// A form awaiting submission; `resolve` unparks `present_form`.
    Form {
        shape: FormShape,
        resolve: oneshot::Sender<Jv>,
    },
    /// The between-loops gate; `resolve` unparks `await_continue`.
    Continue {
        resolve: oneshot::Sender<ContinueSignal>,
    },
}

/// One entry in a node's ask stack: `id` is BOTH its identity and its
/// `data-rev` nonce — assigned once from the node's monotonic counter and
/// never reused, so it never changes for the ask's lifetime (a re-render
/// triggered by a sibling ask or a note update leaves an untouched ask's own
/// revision — and the client's focus-preserving skip signal — stable).
struct AskEntry {
    id: u64,
    pending: Pending,
}

/// How many compiled turn sources the history pane retains per node — enough
/// to scroll back through a working session's recent loops without letting a
/// long-lived process grow the page without bound.
const TURN_HISTORY_CAP: usize = 50;

/// One registered node's state. Every currently pending ask, in publish
/// order (never superseded — resolved asks are removed, nothing else is);
/// `next_ask_id` mints the next ask's id/nonce; `notes`/`turn_history` are
/// node-scoped exactly as the single-node crate had them; `rev` is this
/// node's AGGREGATE revision, bumped under the SAME lock as every mutation
/// below (F10, now per-node) — the panel-root `data-rev` the client's
/// focus-preserving skip keys off.
#[derive(Default)]
struct NodeSlot {
    asks: Vec<AskEntry>,
    next_ask_id: u64,
    notes: Vec<String>,
    turn_history: VecDeque<String>,
    rev: u64,
}

impl NodeSlot {
    /// Borrowed ask views for rendering — `(interaction id, kind)` in
    /// publish order.
    fn ask_views(&self) -> Vec<(u64, Ask<'_>)> {
        self.asks
            .iter()
            .map(|a| {
                let view = match &a.pending {
                    Pending::Form { shape, .. } => Ask::Form(shape),
                    Pending::Continue { .. } => Ask::Continue,
                };
                (a.id, view)
            })
            .collect()
    }
}

/// Every registered node, plus registration order (tab display order) —
/// ONE lock over the whole registry (not one per node): with the small
/// node/ask counts this GUI ever holds, a single lock is simpler and
/// structurally rules out cross-node races, at no real concurrency cost.
#[derive(Default)]
struct Registry {
    order: Vec<NodeId>,
    nodes: HashMap<NodeId, NodeSlot>,
}

/// Shared server state: every registered node's pending asks + the re-render
/// tick.
#[derive(Clone)]
pub struct AppState {
    registry: Arc<Mutex<Registry>>,
    /// Broadcast of "this node's panel changed" — a gate pings this after
    /// publishing/resolving an ask or updating notes/turn-history so every
    /// open SSE stream re-renders that one node's panel promptly.
    tick: broadcast::Sender<NodeId>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
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
    /// exactly as before, now scoped to the node it registered.
    pub fn register_node(&self, node_id: impl Into<String>) -> Arc<WebGate> {
        let node_id = node_id.into();
        {
            let mut reg = self.registry.lock();
            if !reg.nodes.contains_key(&node_id) {
                reg.order.push(node_id.clone());
                reg.nodes.insert(node_id.clone(), NodeSlot::default());
            }
        }
        Arc::new(WebGate {
            state: self.clone(),
            node_id,
        })
    }

    /// Registered node ids in registration order — the tab strip's order.
    pub fn node_ids(&self) -> Vec<NodeId> {
        self.registry.lock().order.clone()
    }

    /// Append a new ask onto `node_id`'s stack (NEVER supersedes an existing
    /// one), bump that node's revision, and ping. Returns the new ask's id
    /// (its `data-rev` nonce).
    fn publish_ask(&self, node_id: &str, pending: Pending) -> u64 {
        let mut reg = self.registry.lock();
        let slot = reg
            .nodes
            .get_mut(node_id)
            .expect("WebGate only holds ids from register_node, which always inserts one");
        let id = slot.next_ask_id;
        slot.next_ask_id += 1;
        slot.asks.push(AskEntry { id, pending });
        slot.rev += 1;
        drop(reg);
        self.ping(node_id.to_string());
        id
    }

    /// Push a `note` onto `node_id`'s feed, bump its revision, and ping.
    fn push_note(&self, node_id: &str, text: String) {
        let mut reg = self.registry.lock();
        let slot = reg.nodes.get_mut(node_id).expect("registered node");
        slot.notes.push(text);
        slot.rev += 1;
        drop(reg);
        self.ping(node_id.to_string());
    }

    /// Clear `node_id`'s note feed at a loop boundary. A no-op ping when the
    /// feed was already empty would still be harmless, but skip it so an
    /// already-quiet feed doesn't force a redundant re-render.
    fn clear_notes(&self, node_id: &str) {
        let mut reg = self.registry.lock();
        let slot = reg.nodes.get_mut(node_id).expect("registered node");
        if slot.notes.is_empty() {
            return;
        }
        slot.notes.clear();
        slot.rev += 1;
        drop(reg);
        self.ping(node_id.to_string());
    }

    /// Append to `node_id`'s turn history (dropping the oldest past
    /// [`TURN_HISTORY_CAP`]), bump its revision, and ping.
    fn push_turn_source(&self, node_id: &str, source: String) {
        let mut reg = self.registry.lock();
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
        Some(
            render::node_panel(
                node_id,
                &slot.ask_views(),
                &slot.notes,
                &slot.turn_history,
                slot.rev,
            )
            .into_string(),
        )
    }

    /// The full page: tab strip + every registered node's panel, in
    /// registration order.
    fn page_markup(&self) -> maud::Markup {
        let reg = self.registry.lock();
        let panels: Vec<(NodeId, maud::Markup)> = reg
            .order
            .iter()
            .map(|id| {
                let slot = &reg.nodes[id];
                (
                    id.clone(),
                    render::node_panel(
                        id,
                        &slot.ask_views(),
                        &slot.notes,
                        &slot.turn_history,
                        slot.rev,
                    ),
                )
            })
            .collect();
        drop(reg);
        shell::page(panels)
    }

    /// Take the ask at `(node_id, interaction)` iff it exists AND `extract`
    /// accepts its [`Pending`] variant; puts it back untouched — same
    /// position — on a type mismatch (F10: a failure never drops a pending
    /// interaction, even mid-lookup).
    fn resolve_ask<T>(
        &self,
        node_id: &str,
        interaction: u64,
        extract: impl FnOnce(Pending) -> Result<T, Pending>,
    ) -> Result<T, ResolveError> {
        let mut reg = self.registry.lock();
        let slot = reg
            .nodes
            .get_mut(node_id)
            .ok_or(ResolveError::UnknownNode)?;
        let pos = slot
            .asks
            .iter()
            .position(|a| a.id == interaction)
            .ok_or(ResolveError::NoSuchInteraction)?;
        let entry = slot.asks.remove(pos);
        match extract(entry.pending) {
            Ok(value) => {
                slot.rev += 1;
                drop(reg);
                self.ping(node_id.to_string());
                Ok(value)
            }
            Err(pending) => {
                slot.asks.insert(
                    pos,
                    AskEntry {
                        id: interaction,
                        pending,
                    },
                );
                Err(ResolveError::WrongKind)
            }
        }
    }

    /// Resolve a pending FORM at `(node_id, interaction)`. Shared by the
    /// browser `/submit` verb and the form-api `POST` — one resolution path,
    /// two front doors.
    pub(crate) fn resolve_form(
        &self,
        node_id: &str,
        interaction: u64,
        submission: Map<String, Jv>,
    ) -> Result<(), ResolveError> {
        let (shape, resolve) = self.resolve_ask(node_id, interaction, |p| match p {
            Pending::Form { shape, resolve } => Ok((shape, resolve)),
            other => Err(other),
        })?;
        let _ = resolve.send(answer_value(&shape, submission));
        Ok(())
    }

    /// Resolve a pending CONTINUE gate at `(node_id, interaction)`.
    fn resolve_continue(
        &self,
        node_id: &str,
        interaction: u64,
        signal: ContinueSignal,
    ) -> Result<(), ResolveError> {
        let resolve = self.resolve_ask(node_id, interaction, |p| match p {
            Pending::Continue { resolve } => Ok(resolve),
            other => Err(other),
        })?;
        let _ = resolve.send(signal);
        Ok(())
    }

    /// The form-api `GET` view: every currently pending FORM ask (a Continue
    /// gate is never surfaced here — same restriction the single-node form
    /// api had) for `node_id`, each paired with its interaction id (the
    /// nonce `POST` must echo back). `Err(())` if `node_id` isn't
    /// registered.
    pub(crate) fn pending_forms(&self, node_id: &str) -> Result<Vec<(u64, FormShape)>, ()> {
        let reg = self.registry.lock();
        let slot = reg.nodes.get(node_id).ok_or(())?;
        Ok(slot
            .asks
            .iter()
            .filter_map(|a| match &a.pending {
                Pending::Form { shape, .. } => Some((a.id, shape.clone())),
                Pending::Continue { .. } => None,
            })
            .collect())
    }
}

/// Why resolving a specific `(node_id, interaction)` failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolveError {
    /// No such registered node.
    UnknownNode,
    /// Nothing pending at that interaction id — already resolved by someone
    /// else, or never existed (a stale/guessed nonce).
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
/// Publishes a NEW ask onto that node's stack, then BLOCKS the calling
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
            Pending::Form {
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
        // A loop boundary: the next loop's notes start from an empty feed.
        self.state.clear_notes(&self.node_id);
        let (resolve, wait) = oneshot::channel();
        self.state
            .publish_ask(&self.node_id, Pending::Continue { resolve });
        wait.blocking_recv().unwrap_or(ContinueSignal::Continue)
    }

    fn post_note(&self, text: &str) {
        self.state.push_note(&self.node_id, text.to_string());
    }

    fn post_turn_source(&self, source: &str) {
        self.state
            .push_turn_source(&self.node_id, source.to_string());
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
        .route("/node/{node}/continue/{interaction}", post(continue_loop));
    crate::formapi::merge(base, form_api).with_state(state)
}

async fn page(State(st): State<AppState>) -> Html<String> {
    Html(st.page_markup().into_string())
}

/// The SSE stream: one Datastar `patch-elements` frame per node-scoped tick,
/// each carrying that node's freshly rendered panel. Initial frames go out
/// immediately for every currently registered node so a page opened
/// mid-interaction is correct without waiting for a tick.
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
    /// Test-only convenience: the id of the first (oldest) pending ask on
    /// `node_id`, if any.
    fn first_ask_id(&self, node_id: &str) -> Option<u64> {
        self.registry
            .lock()
            .nodes
            .get(node_id)?
            .asks
            .first()
            .map(|a| a.id)
    }

    fn ask_count(&self, node_id: &str) -> usize {
        self.registry.lock().nodes[node_id].asks.len()
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
                },
                FieldShape {
                    key: "count".into(),
                    shape: FormShape::Int,
                },
            ],
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
        let ids: Vec<u64> = {
            let reg = st.registry.lock();
            reg.nodes["n1"].asks.iter().map(|a| a.id).collect()
        };
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
        assert_eq!(st.ask_count("n1"), 1, "only the resolved ask is removed");
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
                },
                FieldShape {
                    key: "port".to_string(),
                    shape: FormShape::Int,
                },
            ],
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
                },
                FieldShape {
                    key: "destination".to_string(),
                    shape: destination_shape(),
                },
                FieldShape {
                    key: "releaseNote".to_string(),
                    shape: FormShape::Optional(Box::new(FormShape::String)),
                },
            ],
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
    /// carrying a `shape` renders through `render::form` at
    /// [`ROOT_BIND_PATH`], and [`submit`] collects from that same root.
    ///
    /// A root SUM (what `choose` and `askUser @<enum>` produce) is the case
    /// that pins why the root is NON-EMPTY: its radios are grouped by `name`,
    /// and HTML does not group radios sharing an empty one — an empty root
    /// would let the operator check two branches of one choice.
    #[test]
    fn root_bind_path_renders_and_collects_a_root_sum() {
        let shape = destination_shape();
        let asks = vec![(0u64, Ask::Form(&shape))];
        let html = crate::render::node_panel("n1", &asks, &[], &VecDeque::new(), 1).into_string();
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
