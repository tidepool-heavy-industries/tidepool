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
//! - `POST /confirm/:node` — consume a staged elaborator proposal (B2): runs
//!                         the already GHC-validated `resume expr` and
//!                         resumes the hole.
//! - `POST /reject/:node`  — discard a staged elaborator proposal (B2): the
//!                         hole reopens untouched for a fresh answer.
//! - `POST /cancel/:node`— cancel a node.
//! - `POST /eval_in_binding/:node` — evaluate a plain `M a` expression against
//!                         a SUSPENDED node's live session heap; a
//!                         non-consuming heap-browser peek (D4), not an
//!                         answer. Body `{name, expr}`; `name` is a label only.
//! - `GET  /snapshot`    — cursor-paged flat node list: `?cursor=<id>&limit=<n>`
//!                         (both optional; `limit` default 50). Response
//!                         `{nodes: [...], next_cursor: <id>|null}`.
//! - `POST /auth/start`  — begin OAuth sign-in (returns the URL + port-forward
//!                         hint); `GET /auth/status` reports sign-in state.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
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
        .route("/confirm/{node}", post(confirm))
        .route("/reject/{node}", post(reject))
        .route("/cancel/{node}", post(cancel))
        .route("/eval_in_binding/{node}", post(eval_in_binding))
        .route("/snapshot", get(snapshot))
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
    let meters = meters_fragment(&st);
    let trace = trace_fragment(&st);
    let log = log_fragment(&st);
    Html(shell::page(signed_in, tree, inspector, meters, trace, log).into_string())
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

/// The inspector pane markup (id="inspector"). When the focused operator hole
/// has a staged elaborator proposal (B2), that takes priority — the operator
/// must confirm/reject it before the raw form is relevant again. Otherwise
/// renders the pending Dialog hole's `Ui` form, or an empty placeholder.
fn inspector_fragment(st: &AppState) -> Markup {
    let inner = if let Some(node) = st.harness.first_operator_hole() {
        if let Some(source) = st.harness.pending_proposal_source(node) {
            render_proposal(node, &source)
        } else if let Some(ui) = st.harness.pending_dialog_ui(node) {
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

/// The elaborator proposal card (B2): the GHC-valid `resume expr` the calling
/// model proposed for a non-mechanical dialog submission, SHOWN BEFORE
/// CONSUME — the operator confirms (runs it and resumes the hole) or rejects
/// (discards it; the hole reopens, submission preserved in the transcript).
fn render_proposal(node: NodeId, source: &str) -> Markup {
    html! {
        div class="ui-card" {
            h3 class="ui-card-title" { "Elaborated proposal" }
            pre class="ui-code" { code { (source) } }
            div class="actions" {
                button data-on-click=(format!("@post('/confirm/{}')", node.0)) { "confirm" }
                button class="ghost" data-on-click=(format!("@post('/reject/{}')", node.0)) { "reject" }
            }
        }
    }
}

/// One node's token-usage rollup, folded from `TurnDelta` events.
struct MeterRow {
    node: u64,
    input_tokens: u64,
    output_tokens: u64,
}

/// Fold every logged `TurnDelta`'s `usage` (present on assistant turns only,
/// per F2) into a per-node total plus a whole-run rollup. Node rows are in
/// ascending node-id order (a `BTreeMap` fold, not log order).
fn fold_usage(path: &std::path::Path) -> (Vec<MeterRow>, (u64, u64)) {
    let Ok((_h, iter)) = LogReader::open(path) else {
        return (Vec::new(), (0, 0));
    };
    let mut per_node: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
    for rec in iter.flatten() {
        if let LogEvent::TurnDelta {
            node,
            usage: Some(usage),
            ..
        } = rec.event
        {
            let entry = per_node.entry(node.0).or_insert((0, 0));
            entry.0 += usage.input_tokens;
            entry.1 += usage.output_tokens;
        }
    }
    let total = per_node
        .values()
        .fold((0u64, 0u64), |acc, (i, o)| (acc.0 + i, acc.1 + o));
    let rows = per_node
        .into_iter()
        .map(|(node, (input_tokens, output_tokens))| MeterRow {
            node,
            input_tokens,
            output_tokens,
        })
        .collect();
    (rows, total)
}

/// Pure render of a meters snapshot (split from `meters_fragment` so it's
/// testable without an `AppState`/live `Harness`).
fn render_meters(rows: &[MeterRow], total: (u64, u64)) -> Markup {
    html! {
        div id="meters" {
            div class="meter-rollup" {
                "rollup: " b { (total.0) } " in / " b { (total.1) } " out tokens"
            }
            @if rows.is_empty() {
                div class="empty" { "No assistant turns yet." }
            } @else {
                table class="meter-table" {
                    thead { tr { th { "node" } th { "in" } th { "out" } } }
                    tbody {
                        @for r in rows {
                            tr {
                                td { "n" (r.node) }
                                td { (r.input_tokens) }
                                td { (r.output_tokens) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The meters pane markup (id="meters"): per-node + rollup token usage.
fn meters_fragment(st: &AppState) -> Markup {
    let (rows, total) = fold_usage(&st.log_path);
    render_meters(&rows, total)
}

/// One logged effect req/resp pair, rendered for the trace pane.
struct EffectRow {
    seq: u64,
    tag: String,
    req: String,
    resp: String,
}

/// Fold every logged `Effect` event into a per-node tail (the last `tail`
/// entries per node, log order preserved within each node's list). Node keys
/// are in ascending node-id order.
fn fold_effects(path: &std::path::Path, tail: usize) -> Vec<(u64, Vec<EffectRow>)> {
    let Ok((_h, iter)) = LogReader::open(path) else {
        return Vec::new();
    };
    let mut per_node: BTreeMap<u64, Vec<EffectRow>> = BTreeMap::new();
    for rec in iter.flatten() {
        if let LogEvent::Effect {
            node,
            seq,
            tag,
            req,
            resp,
        } = rec.event
        {
            per_node.entry(node.0).or_default().push(EffectRow {
                seq,
                tag,
                req: req.to_string(),
                resp: resp.to_string(),
            });
        }
    }
    per_node
        .into_iter()
        .map(|(node, mut rows)| {
            if rows.len() > tail {
                rows = rows.split_off(rows.len() - tail);
            }
            (node, rows)
        })
        .collect()
}

/// Pure render of a trace snapshot (split from `trace_fragment` so it's
/// testable without an `AppState`/live `Harness`). Each node's tail is a
/// `<details>` element, collapsed by default.
fn render_trace(per_node: &[(u64, Vec<EffectRow>)]) -> Markup {
    html! {
        div id="trace" {
            @if per_node.is_empty() {
                div class="empty" { "No effects yet." }
            }
            @for (node, rows) in per_node {
                details class="trace-node" {
                    summary {
                        "node " (node) " · " (rows.len())
                        (if rows.len() == 1 { " effect" } else { " effects" })
                    }
                    @for r in rows {
                        div class="trace-row" {
                            span class="chip" { (r.tag) " #" (r.seq) }
                            div class="trace-req" { "→ " (truncate(&r.req, 200)) }
                            div class="trace-resp" { "← " (truncate(&r.resp, 200)) }
                        }
                    }
                }
            }
        }
    }
}

/// The trace pane markup (id="trace"): per-node effect req/resp tail,
/// collapsed by default (20 most recent effects per node).
fn trace_fragment(st: &AppState) -> Markup {
    let per_node = fold_effects(&st.log_path, 20);
    render_trace(&per_node)
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

    /// Render the tree + inspector + meters + trace as a single Datastar
    /// patch-elements frame. (All four panes in one frame: the client applies
    /// each `[id]` element. The log pane is initial-render only — it is not
    /// re-rendered here.)
    fn render_frames(st: &AppState) -> Event {
        use datastar::prelude::PatchElements;
        let tree = super::tree_fragment(st).into_string();
        let inspector = super::inspector_fragment(st).into_string();
        let meters = super::meters_fragment(st).into_string();
        let trace = super::trace_fragment(st).into_string();
        let combined = format!("{tree}{inspector}{meters}{trace}");
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

/// Confirm a staged elaborator proposal (B2): consumes the hole. Runs off the
/// request task (same fire-and-ping shape as `fork`/`answer`) since it drives
/// a `run_child` + resume through the blocking pool.
async fn confirm(State(st): State<AppState>, Path(node): Path<u64>) -> Response {
    let node = NodeId(node);
    let harness = st.harness.clone();
    let st2 = st.clone();
    tokio::spawn(async move {
        let _ = harness.confirm_proposal(node).await;
        st2.ping();
    });
    st.ping();
    Json(json!({"ok": true, "confirming": node.0})).into_response()
}

/// Reject a staged elaborator proposal (B2): discards it, the hole stays
/// suspended and open for a fresh answer. Synchronous (no session/GHC work).
async fn reject(State(st): State<AppState>, Path(node): Path<u64>) -> Response {
    let node = NodeId(node);
    match st.harness.reject_proposal(node) {
        Ok(()) => {
            st.ping();
            Json(json!({"ok": true})).into_response()
        }
        Err(e) => err_json(e.to_string()),
    }
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

/// Evaluate `expr` against `node`'s suspended session heap; a non-consuming
/// heap-browser peek (D4), NOT an answer — the pending hole is untouched, so
/// this does not `ping()` the tree/inspector. Body: `{name, expr}` (`name`
/// defaults to `"binding"` if omitted — it only labels the JIT fragment).
async fn eval_in_binding(
    State(st): State<AppState>,
    Path(node): Path<u64>,
    body: Option<Json<Jv>>,
) -> Response {
    let node = NodeId(node);
    let raw = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let name = raw
        .get("name")
        .and_then(Jv::as_str)
        .unwrap_or("binding")
        .to_string();
    let Some(expr) = raw.get("expr").and_then(Jv::as_str) else {
        return err_json("missing \"expr\" (a plain M a expression to evaluate)".to_string());
    };
    match st.harness.eval_in_binding(node, &name, expr).await {
        Ok(rendered) => Json(json!({"ok": true, "name": name, "rendered": rendered})).into_response(),
        Err(e) => err_json(e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct SnapshotQuery {
    cursor: Option<u64>,
    limit: Option<usize>,
}

/// A [`tidepool_harness::NodeSummary`] flattened for the wire — kept local
/// (rather than deriving `Serialize` on `NodeSummary` itself) so this leaf's
/// one wire shape lives entirely in `tidepool-web`.
#[derive(serde::Serialize)]
struct SnapshotNode {
    node: u64,
    parent: Option<u64>,
    state: &'static str,
    hole: Option<String>,
    is_fork_hole: bool,
    hole_prompt: Option<String>,
}

fn snapshot_node(n: &tidepool_harness::NodeSummary) -> SnapshotNode {
    let (state, hole) = match &n.state {
        NodeState::Thunk => ("thunk", None),
        NodeState::Running => ("running", None),
        NodeState::Suspended { hole } => ("suspended", Some(hole.0.clone())),
        NodeState::Done => ("done", None),
        NodeState::Cancelled { reason } => ("cancelled", Some(reason.clone())),
    };
    SnapshotNode {
        node: n.node.0,
        parent: n.parent.map(|p| p.0),
        state,
        hole,
        is_fork_hole: n.is_fork_hole,
        hole_prompt: n.hole_prompt.clone(),
    }
}

/// Cursor-paged flat node snapshot (E1/C4: "snapshot endpoints paginate, no
/// small-tree assumption"). `?cursor=<id>&limit=<n>`, both optional (`limit`
/// default 50, clamped to 500). Response: `{nodes: [...], next_cursor:
/// <id>|null}` — pass `next_cursor` back as `cursor` for the following page;
/// `null` means the snapshot is exhausted.
async fn snapshot(State(st): State<AppState>, Query(q): Query<SnapshotQuery>) -> Response {
    let cursor = q.cursor.map(NodeId);
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let (nodes, next) = st.harness.tree_snapshot_page(cursor, limit);
    Json(json!({
        "ok": true,
        "nodes": nodes.iter().map(snapshot_node).collect::<Vec<_>>(),
        "next_cursor": next.map(|n| n.0),
    }))
    .into_response()
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

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_harness::log::{Event as LogEvent, LogHeader, LogWriter};
    use tidepool_harness::provider::{Role, Usage};

    fn header() -> LogHeader {
        LogHeader {
            prelude_hash: "test-prelude".into(),
            extract_fingerprint: "test-extract".into(),
            harness_version: "test-harness".into(),
        }
    }

    /// A fixture log with usage on two nodes (one node with two assistant
    /// turns, one with none) and two effect events on node 0 — enough to
    /// exercise both fold functions' per-node grouping + rollup.
    fn write_fixture_log(path: &std::path::Path) {
        let mut w = LogWriter::create(path, &header()).unwrap();
        w.append(LogEvent::TurnDelta {
            node: NodeId(0),
            turn: 0,
            role: Role::Assistant,
            content: "hi".into(),
            usage: Some(Usage {
                input_tokens: 120,
                output_tokens: 40,
            }),
        })
        .unwrap();
        w.append(LogEvent::TurnDelta {
            node: NodeId(0),
            turn: 1,
            role: Role::Assistant,
            content: "again".into(),
            usage: Some(Usage {
                input_tokens: 30,
                output_tokens: 10,
            }),
        })
        .unwrap();
        // A user-role turn (no usage) must not pollute the rollup.
        w.append(LogEvent::TurnDelta {
            node: NodeId(1),
            turn: 0,
            role: Role::User,
            content: "seed".into(),
            usage: None,
        })
        .unwrap();
        w.append(LogEvent::TurnDelta {
            node: NodeId(1),
            turn: 1,
            role: Role::Assistant,
            content: "ok".into(),
            usage: Some(Usage {
                input_tokens: 8,
                output_tokens: 3,
            }),
        })
        .unwrap();
        w.append(LogEvent::Effect {
            node: NodeId(0),
            seq: 0,
            tag: "Fs".into(),
            req: json!({"op": "read", "path": "a.txt"}),
            resp: json!({"ok": true}),
        })
        .unwrap();
        w.append(LogEvent::Effect {
            node: NodeId(0),
            seq: 1,
            tag: "Exec".into(),
            req: json!({"cmd": "ls"}),
            resp: json!({"stdout": "a b"}),
        })
        .unwrap();
    }

    #[test]
    fn fold_usage_groups_per_node_and_rolls_up_assistant_usage_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.jsonl");
        write_fixture_log(&path);

        let (rows, total) = fold_usage(&path);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].node, 0);
        assert_eq!(rows[0].input_tokens, 150);
        assert_eq!(rows[0].output_tokens, 50);
        assert_eq!(rows[1].node, 1);
        assert_eq!(rows[1].input_tokens, 8);
        assert_eq!(rows[1].output_tokens, 3);
        assert_eq!(total, (158, 53));
    }

    #[test]
    fn fold_usage_missing_log_is_empty() {
        let (rows, total) = fold_usage(std::path::Path::new("/nonexistent/does-not-exist.jsonl"));
        assert!(rows.is_empty());
        assert_eq!(total, (0, 0));
    }

    #[test]
    fn fold_effects_groups_per_node_and_tails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.jsonl");
        write_fixture_log(&path);

        let per_node = fold_effects(&path, 20);
        assert_eq!(per_node.len(), 1, "only node 0 has effect events");
        let (node, rows) = &per_node[0];
        assert_eq!(*node, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].tag, "Fs");
        assert_eq!(rows[1].tag, "Exec");
    }

    #[test]
    fn fold_effects_tail_keeps_only_the_most_recent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.jsonl");
        let mut w = LogWriter::create(&path, &header()).unwrap();
        for i in 0..5u64 {
            w.append(LogEvent::Effect {
                node: NodeId(0),
                seq: i,
                tag: format!("Fs{i}"),
                req: Jv::Null,
                resp: Jv::Null,
            })
            .unwrap();
        }
        let per_node = fold_effects(&path, 2);
        assert_eq!(per_node.len(), 1);
        let (_, rows) = &per_node[0];
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].tag, "Fs3");
        assert_eq!(rows[1].tag, "Fs4");
    }

    #[test]
    fn snapshot_meters_empty() {
        let rendered = render_meters(&[], (0, 0)).into_string();
        assert_eq!(
            rendered,
            "<div id=\"meters\">\
             <div class=\"meter-rollup\">rollup: <b>0</b> in / <b>0</b> out tokens</div>\
             <div class=\"empty\">No assistant turns yet.</div>\
             </div>"
        );
    }

    #[test]
    fn snapshot_meters_with_rows() {
        let rows = vec![
            MeterRow {
                node: 0,
                input_tokens: 150,
                output_tokens: 50,
            },
            MeterRow {
                node: 2,
                input_tokens: 8,
                output_tokens: 3,
            },
        ];
        let rendered = render_meters(&rows, (158, 53)).into_string();
        assert_eq!(
            rendered,
            "<div id=\"meters\">\
             <div class=\"meter-rollup\">rollup: <b>158</b> in / <b>53</b> out tokens</div>\
             <table class=\"meter-table\">\
             <thead><tr><th>node</th><th>in</th><th>out</th></tr></thead>\
             <tbody>\
             <tr><td>n0</td><td>150</td><td>50</td></tr>\
             <tr><td>n2</td><td>8</td><td>3</td></tr>\
             </tbody>\
             </table>\
             </div>"
        );
    }

    #[test]
    fn snapshot_trace_empty() {
        let rendered = render_trace(&[]).into_string();
        assert_eq!(
            rendered,
            "<div id=\"trace\"><div class=\"empty\">No effects yet.</div></div>"
        );
    }

    #[test]
    fn snapshot_trace_collapsed_by_default_with_effect_rows() {
        let per_node = vec![(
            0u64,
            vec![
                EffectRow {
                    seq: 0,
                    tag: "Fs".into(),
                    req: "{\"op\":\"read\"}".into(),
                    resp: "{\"ok\":true}".into(),
                },
                EffectRow {
                    seq: 1,
                    tag: "Exec".into(),
                    req: "{\"cmd\":\"ls\"}".into(),
                    resp: "{\"stdout\":\"a b\"}".into(),
                },
            ],
        )];
        let rendered = render_trace(&per_node).into_string();
        assert_eq!(
            rendered,
            "<div id=\"trace\">\
             <details class=\"trace-node\">\
             <summary>node 0 · 2 effects</summary>\
             <div class=\"trace-row\">\
             <span class=\"chip\">Fs #0</span>\
             <div class=\"trace-req\">→ {&quot;op&quot;:&quot;read&quot;}</div>\
             <div class=\"trace-resp\">← {&quot;ok&quot;:true}</div>\
             </div>\
             <div class=\"trace-row\">\
             <span class=\"chip\">Exec #1</span>\
             <div class=\"trace-req\">→ {&quot;cmd&quot;:&quot;ls&quot;}</div>\
             <div class=\"trace-resp\">← {&quot;stdout&quot;:&quot;a b&quot;}</div>\
             </div>\
             </details>\
             </div>"
        );
        // A <details> element with no `open` attribute is collapsed by default.
        assert!(!rendered.contains("open"));
    }

    #[test]
    fn snapshot_trace_singular_effect_count_label() {
        let per_node = vec![(
            7u64,
            vec![EffectRow {
                seq: 0,
                tag: "Fs".into(),
                req: "{}".into(),
                resp: "{}".into(),
            }],
        )];
        let rendered = render_trace(&per_node).into_string();
        assert!(rendered.contains("<summary>node 7 · 1 effect</summary>"));
    }
}
