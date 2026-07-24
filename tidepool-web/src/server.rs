//! The protocol server (E1): axum + SSE over the harness. Everything the
//! observatory does is a documented HTTP verb — the UI is a client of the
//! protocol, no private back-channel. Loopback bind only: reachability
//! (localhost / tailnet) is the authorization boundary (see the crate doc).
//!
//! # Verbs
//!
//! - `GET  /`            — the observatory page shell.
//! - `GET  /sse`         — the log-tail SSE stream (Datastar patch-elements
//!                         frames re-rendering the tree + appending log lines).
//! - `POST /force/:node` — force a thunk node (operator consent).
//! - `POST /answer/:node`         — submit a form/prose answer to a Dialog hole.
//! - `POST /answer/:node/:key`    — a mechanical option-key answer (D6).
//! - `POST /fork/:node`  — force + drive the fork answerer for a fork hole.
//! - `POST /cancel/:node`— cancel a node.
//! - `POST /auth/start`  — begin OAuth sign-in (returns the URL + port-forward
//!                         hint); `GET /auth/status` reports sign-in state.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use maud::{html, Markup};
use serde_json::{json, Value as Jv};
use tidepool_harness::log::{Event as LogEvent, LogReader};
use tidepool_harness::provider::oauth::{self, OauthConfig};
use tidepool_harness::tree::{NodeId, NodeState};
use tidepool_harness::Harness;
use tokio::sync::broadcast;

use crate::render::render_with_answer_url;
use crate::shell;

/// Shared server state: the harness, the log path (for the SSE follower), and
/// the OAuth config (for the auth verbs).
#[derive(Clone)]
pub struct AppState {
    pub harness: Arc<Harness>,
    pub log_path: std::path::PathBuf,
    pub oauth: OauthConfig,
    /// Broadcast of "the tree changed" ticks — a verb handler pings this after
    /// mutating the harness so the SSE stream re-renders promptly (the log
    /// follower is the durable source; this is the low-latency nudge).
    pub tick: broadcast::Sender<()>,
}

impl AppState {
    pub fn new(harness: Arc<Harness>, log_path: std::path::PathBuf, oauth: OauthConfig) -> Self {
        let (tick, _) = broadcast::channel(64);
        AppState {
            harness,
            log_path,
            oauth,
            tick,
        }
    }

    fn ping(&self) {
        let _ = self.tick.send(());
    }
}

/// Build the axum router over the app state.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/sse", get(sse))
        .route("/create", post(create))
        .route("/force/{node}", post(force))
        .route("/fork/{node}", post(fork))
        .route("/answer/{node}", post(answer))
        .route("/answer/{node}/{key}", post(answer_key))
        .route("/cancel/{node}", post(cancel))
        .route("/auth/start", post(auth_start))
        .route("/auth/status", get(auth_status))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Page + fragments
// ---------------------------------------------------------------------------

async fn page(State(st): State<AppState>) -> Html<String> {
    let signed_in = matches!(
        oauth::login_status(&st.oauth),
        oauth::LoginStatus::SignedIn
    );
    let tree = tree_fragment(&st);
    let inspector = inspector_fragment(&st);
    let log = log_fragment(&st);
    Html(shell::page(signed_in, tree, inspector, log).into_string())
}

/// The tree pane markup (id="tree" so SSE patches replace it wholesale).
fn tree_fragment(st: &AppState) -> Markup {
    let nodes = st.harness.tree_snapshot();
    html! {
        div id="tree" {
            @if nodes.is_empty() {
                div class="empty" { "No nodes yet." }
            }
            @for n in &nodes {
                (node_row(n))
            }
        }
    }
}

fn node_row(n: &tidepool_harness::NodeSummary) -> Markup {
    let (state_class, state_label) = match &n.state {
        NodeState::Thunk => ("thunk", "thunk"),
        NodeState::Running => ("running", "running"),
        NodeState::Suspended { .. } => ("suspended", "suspended"),
        NodeState::Done => ("done", "done"),
        NodeState::Cancelled { .. } => ("cancelled", "cancelled"),
    };
    let is_child = n.parent.is_some();
    let suspended = matches!(n.state, NodeState::Suspended { .. });
    html! {
        div class={ "node" @if is_child { " child" } @if suspended { " suspended" } } {
            div class="teaser" { "node " (n.node.0) }
            div class="meta" {
                span class={ "chip state-" (state_class) } { (state_label) }
                @if n.is_fork_hole { span class="chip fork" { "fork" } }
            }
            @if let Some(prompt) = &n.hole_prompt {
                div class="meta" { span class="chip" { (truncate(prompt, 48)) } }
            }
            div class="actions" {
                @match &n.state {
                    NodeState::Thunk => {
                        button data-on-click=(format!("@post('/force/{}')", n.node.0)) { "force" }
                    }
                    NodeState::Suspended { .. } if n.is_fork_hole => {
                        button data-on-click=(format!("@post('/fork/{}')", n.node.0)) { "force fork answerer" }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// The inspector pane markup (id="inspector"). Renders the pending operator
/// (Dialog) hole's `Ui` form, or an empty placeholder.
fn inspector_fragment(st: &AppState) -> Markup {
    let inner = if let Some(node) = st.harness.first_operator_hole() {
        if let Some(ui) = st.harness.pending_dialog_ui(node) {
            let answer_url = format!("/answer/{}", node.0);
            render_with_answer_url(&ui, &answer_url)
        } else {
            shell::inspector_empty()
        }
    } else {
        shell::inspector_empty()
    };
    html! { div id="inspector" { (inner) } }
}

/// The log pane markup (id="log"). Renders the last N events.
fn log_fragment(st: &AppState) -> Markup {
    let lines = recent_log_lines(&st.log_path, 200);
    html! {
        div id="log" {
            @for line in &lines { (line) }
        }
    }
}

fn recent_log_lines(path: &std::path::Path, max: usize) -> Vec<Markup> {
    let Ok((_h, iter)) = LogReader::open(path) else {
        return Vec::new();
    };
    let mut all: Vec<Markup> = Vec::new();
    for rec in iter.flatten() {
        all.push(log_line(&rec.event));
    }
    if all.len() > max {
        all.split_off(all.len() - max)
    } else {
        all
    }
}

fn log_line(ev: &LogEvent) -> Markup {
    let (kind, node, detail) = summarize(ev);
    html! {
        div class="log-line" {
            span class="ev" { (kind) }
            " "
            span class="node-id" { "n" (node) }
            " "
            (truncate(&detail, 90))
        }
    }
}

fn summarize(ev: &LogEvent) -> (&'static str, u64, String) {
    match ev {
        LogEvent::NodeCreated { node, teaser, .. } => ("node_created", node.0, teaser.clone()),
        LogEvent::Forced { node, actor } => ("forced", node.0, format!("{actor:?}")),
        LogEvent::TurnStart { node, source, .. } => ("turn_start", node.0, source.clone()),
        LogEvent::Effect { node, tag, .. } => ("effect", node.0, tag.clone()),
        LogEvent::HolePublished {
            node, prompt, fork, ..
        } => (
            "hole_published",
            node.0,
            format!("{}{}", if *fork { "[fork] " } else { "" }, prompt),
        ),
        LogEvent::HoleAnswerAttempt { node, outcome, .. } => {
            ("hole_answer", node.0, format!("{outcome:?}"))
        }
        LogEvent::HoleConsumed { node, .. } => ("hole_consumed", node.0, String::new()),
        LogEvent::NodeDone {
            node,
            result_rendered,
        } => ("node_done", node.0, result_rendered.clone()),
        LogEvent::NodeCancelled { node, reason } => ("node_cancelled", node.0, reason.clone()),
        LogEvent::TurnDelta {
            node, role, content, ..
        } => ("turn", node.0, format!("{role:?}: {content}")),
        LogEvent::TurnForked {
            node,
            parent,
            parent_turn,
        } => (
            "turn_forked",
            node.0,
            format!("from n{} @turn {}", parent.0, parent_turn),
        ),
    }
}

// ---------------------------------------------------------------------------
// SSE
// ---------------------------------------------------------------------------

/// The SSE stream: on every log event (or tick nudge), re-render the tree +
/// inspector panes and emit them as Datastar patch-elements frames, plus one
/// appended log line. The log follower is the durable trigger; the broadcast
/// tick is the low-latency nudge after a verb mutates the harness.
async fn sse(State(st): State<AppState>) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream_placeholder(st);
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

/// The SSE body stream. Kept as a manual `async_stream`-free generator using a
/// channel + spawned follower task, so the crate needs no extra stream-macro
/// dep: a blocking log follower runs on a blocking task and forwards each event
/// through an mpsc channel; the response stream re-renders on each.
mod async_stream {
    use super::*;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::ReceiverStream;
    use tokio_stream::StreamExt;

    pub fn stream_placeholder(
        st: AppState,
    ) -> impl futures_core::Stream<Item = Result<Event, Infallible>> {
        let (tx, rx) = mpsc::channel::<()>(64);

        // Follower task: block on the log tail, forward a tick per event.
        let log_path = st.log_path.clone();
        let tx_follow = tx.clone();
        tokio::task::spawn_blocking(move || {
            if let Ok((_h, mut follower)) =
                LogReader::follow(&log_path, Duration::from_millis(100))
            {
                loop {
                    match follower.next_event() {
                        Ok(_) => {
                            if tx_follow.blocking_send(()).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        });

        // Tick task: forward broadcast nudges too.
        let mut ticks = st.tick.subscribe();
        let tx_tick = tx.clone();
        tokio::spawn(async move {
            while ticks.recv().await.is_ok() {
                if tx_tick.send(()).await.is_err() {
                    break;
                }
            }
        });

        // Emit an initial frame immediately, then one per trigger.
        let initial = futures_util_once(render_frames(&st));
        let st_stream = st.clone();
        let updates = ReceiverStream::new(rx).map(move |_| Ok(render_frames(&st_stream)));

        initial.chain(updates)
    }

    /// A one-item stream (avoids a futures-util dep for `stream::once`).
    fn futures_util_once(
        first: Event,
    ) -> impl futures_core::Stream<Item = Result<Event, Infallible>> {
        tokio_stream::iter(std::iter::once(Ok(first)))
    }

    /// Render the tree + inspector as a single Datastar patch-elements frame.
    /// (Both panes in one frame: the client applies each `[id]` element.)
    fn render_frames(st: &AppState) -> Event {
        use datastar::prelude::PatchElements;
        let tree = super::tree_fragment(st).into_string();
        let inspector = super::inspector_fragment(st).into_string();
        let combined = format!("{tree}{inspector}");
        PatchElements::new(combined).write_as_axum_sse_event()
    }
}

// ---------------------------------------------------------------------------
// Verbs
// ---------------------------------------------------------------------------

/// Create a root node (thunk). Body: `{title, prompt}`. Returns the node id.
/// The operator then forces it (a separate consent step — the root is a thunk,
/// no work until forced).
async fn create(State(st): State<AppState>, body: Option<Json<Jv>>) -> Response {
    let raw = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let title = raw.get("title").and_then(Jv::as_str).unwrap_or("root");
    let prompt = raw
        .get("prompt")
        .and_then(Jv::as_str)
        .unwrap_or("Begin.");
    match st.harness.create_root(title, prompt) {
        Ok(node) => {
            st.ping();
            Json(json!({"ok": true, "node": node.0})).into_response()
        }
        Err(e) => err_json(e.to_string()),
    }
}

async fn force(State(st): State<AppState>, Path(node): Path<u64>) -> Response {
    let node = NodeId(node);
    match st.harness.force(node, tidepool_harness::log::Actor::Operator) {
        Ok(()) => {
            // Drive the forced node's turn loop to its first hole/done.
            let harness = st.harness.clone();
            let st2 = st.clone();
            tokio::spawn(async move {
                if let Err(e) = harness.run_to_hole_or_done(node).await {
                    eprintln!("[drive] node {} failed: {e}", node.0);
                    let _ = harness.cancel(node, &format!("drive failed: {e}"));
                }
                st2.ping();
            });
            st.ping();
            Json(json!({"ok": true, "forced": node.0})).into_response()
        }
        Err(e) => err_json(e.to_string()),
    }
}

async fn fork(State(st): State<AppState>, Path(node): Path<u64>) -> Response {
    let node = NodeId(node);
    let harness = st.harness.clone();
    let st2 = st.clone();
    tokio::spawn(async move {
        let _ = harness
            .answer_fork(node, tidepool_harness::log::Actor::Operator)
            .await;
        st2.ping();
    });
    st.ping();
    Json(json!({"ok": true, "forking": node.0})).into_response()
}

async fn answer(
    State(st): State<AppState>,
    Path(node): Path<u64>,
    body: Option<Json<Jv>>,
) -> Response {
    let node = NodeId(node);
    let submission = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    // Normalize {prose} / {value} into a {values, prose} submission (F1 draft).
    let submission = normalize_submission(submission);
    let harness = st.harness.clone();
    let st2 = st.clone();
    tokio::spawn(async move {
        let _ = harness.answer_dialog(node, submission).await;
        st2.ping();
    });
    st.ping();
    Json(json!({"ok": true})).into_response()
}

async fn answer_key(
    State(st): State<AppState>,
    Path((node, key)): Path<(u64, String)>,
) -> Response {
    let node = NodeId(node);
    // A mechanical option-key answer (D6): {values: {key: true}, prose: ""}.
    let submission = json!({ "values": { &key: true }, "prose": "" });
    let harness = st.harness.clone();
    let st2 = st.clone();
    tokio::spawn(async move {
        let _ = harness.answer_dialog(node, submission).await;
        st2.ping();
    });
    st.ping();
    Json(json!({"ok": true, "key": key})).into_response()
}

async fn cancel(State(st): State<AppState>, Path(node): Path<u64>) -> Response {
    let node = NodeId(node);
    match st.harness.cancel(node, "operator cancel") {
        Ok(()) => {
            st.ping();
            Json(json!({"ok": true})).into_response()
        }
        Err(e) => err_json(e.to_string()),
    }
}

async fn auth_start(State(st): State<AppState>) -> Response {
    match oauth::start_login(&st.oauth).await {
        Ok(start) => Json(json!({
            "authorization_url": start.authorization_url,
            "port_forward_hint": start.port_forward_hint,
        }))
        .into_response(),
        Err(e) => err_json(e.to_string()),
    }
}

async fn auth_status(State(st): State<AppState>) -> Response {
    let signed_in = matches!(
        oauth::login_status(&st.oauth),
        oauth::LoginStatus::SignedIn
    );
    Json(json!({"signed_in": signed_in})).into_response()
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Normalize a raw form POST into the F1 submission shape `{values, prose}`.
/// The client's prose-escape posts `{prose}`; a TextIn posts `{value}`; a
/// mechanical key posts `{values, prose}` already.
fn normalize_submission(raw: Jv) -> Jv {
    if raw.get("values").is_some() {
        return raw;
    }
    let prose = raw
        .get("prose")
        .and_then(Jv::as_str)
        .or_else(|| raw.get("value").and_then(Jv::as_str))
        .unwrap_or("")
        .to_string();
    json!({ "values": {}, "prose": prose })
}

fn err_json(msg: String) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(json!({"ok": false, "error": msg})),
    )
        .into_response()
}

fn truncate(s: &str, max: usize) -> String {
    let one_line: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if one_line.chars().count() > max {
        let cut: String = one_line.chars().take(max).collect();
        format!("{cut}…")
    } else {
        one_line
    }
}
