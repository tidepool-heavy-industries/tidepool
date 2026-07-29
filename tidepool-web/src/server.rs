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
//! - `POST /steer/:node`  — resolve a node's rung-2 escalation (the
//!                         escalation ladder's operator popup): a child
//!                         answerer that exhausted its auto-retry parks here
//!                         awaiting an operator decision. Body
//!                         `{"turns": <n>, "steer": <text?>}` grants `turns`
//!                         more turns (optionally injecting `steer` as the
//!                         answerer's next corrective user turn first), or
//!                         `{"abort": true}` cancels the stuck answerer and
//!                         surfaces a typed error to the fan while the
//!                         parent stays suspended, re-answerable.
//! - `POST /cancel/:node`— cancel a node.
//! - `POST /splice/:node` — interject an operator message into `node`'s OWN
//!                         transcript (F2's `turn_spliced` kind), landing at
//!                         its current turn position so it is visible in
//!                         `node`'s next prompt assembly. Body `{content}`.
//!                         Requires `node` to be `Running` or `Suspended`.
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
use tidepool_harness::provider::Role;
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
    /// The node the operator has focused (clicked in the tree), if any. A
    /// view concern, not harness state: when set, the transcript shows just
    /// that node's conversation and the tree highlights it; when `None`, the
    /// transcript shows every node. Single-operator (loopback), so one shared
    /// selection is correct.
    pub selected: Arc<std::sync::Mutex<Option<NodeId>>>,
}

impl AppState {
    pub fn new(harness: Arc<Harness>, log_path: std::path::PathBuf, oauth: OauthConfig) -> Self {
        let (tick, _) = broadcast::channel(64);
        AppState {
            harness,
            log_path,
            oauth,
            tick,
            selected: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn ping(&self) {
        let _ = self.tick.send(());
    }

    fn selected(&self) -> Option<NodeId> {
        *self.selected.lock().unwrap()
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
        .route("/steer/{node}", post(steer))
        .route("/steer/{node}/abort", post(steer_abort))
        .route("/cancel/{node}", post(cancel))
        .route("/select/{node}", post(select))
        .route("/followup/{node}", post(followup))
        .route("/splice/{node}", post(splice))
        .route("/eval_in_binding/{node}", post(eval_in_binding))
        .route("/snapshot", get(snapshot))
        .route("/signin", get(signin_page))
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
    let heap = heap_fragment(&st);
    let transcript = transcript_fragment(&st);
    let log = log_fragment(&st);
    Html(
        shell::page(signed_in, tree, inspector, meters, trace, heap, transcript, log)
            .into_string(),
    )
}

/// The tree pane markup (id="tree" so SSE patches replace it wholesale).
fn tree_fragment(st: &AppState) -> Markup {
    let nodes = st.harness.tree_snapshot();
    let selected = st.selected();
    html! {
        div id="tree" {
            @if nodes.is_empty() {
                div class="empty" { "No nodes yet." }
            }
            @for n in &nodes {
                (node_row(n, selected == Some(n.node)))
            }
        }
    }
}

fn node_row(n: &tidepool_harness::NodeSummary, is_selected: bool) -> Markup {
    let (state_class, state_label) = match &n.state {
        NodeState::Thunk => ("thunk", "not started"),
        NodeState::Running => ("running", "running"),
        NodeState::Suspended { .. } => ("suspended", "suspended"),
        NodeState::Done => ("done", "done"),
        NodeState::Cancelled { .. } => ("cancelled", "cancelled"),
    };
    let is_child = n.parent.is_some();
    let suspended = matches!(n.state, NodeState::Suspended { .. });
    html! {
        div class={ "node" @if is_child { " child" } @if suspended { " suspended" } @if is_selected { " selected" } }
            data-on-click=(format!("@post('/select/{}')", n.node.0)) {
            div class="teaser" { "node " (n.node.0) }
            div class="meta" {
                span class={ "chip state-" (state_class) } { (state_label) }
                @if n.is_fork_hole { span class="chip fork" { "fork" } }
                @if n.awaiting_operator { span class="chip escalated" { "awaiting operator" } }
            }
            @if let Some(prompt) = &n.hole_prompt {
                div class="meta" { span class="chip" { (truncate(prompt, 48)) } }
            }
            div class="actions" {
                @match &n.state {
                    NodeState::Thunk => {
                        button data-on-click=(format!("@post('/force/{}')", n.node.0)) { "start" }
                    }
                    NodeState::Suspended { .. } if n.is_fork_hole => {
                        button data-on-click=(format!("@post('/fork/{}')", n.node.0)) { "answer via fork" }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// The inspector pane markup (id="inspector"). A parked rung-2 ESCALATION
/// (the stuck-node popup) takes top priority — it blocks a whole fan's
/// progress and has no pending hole of its own to otherwise surface it.
/// Otherwise renders the pending Dialog hole's `Ui` form, or an empty
/// placeholder.
fn inspector_fragment(st: &AppState) -> Markup {
    let inner = if let Some(node) = st.harness.first_escalated_node() {
        match st.harness.escalation_of(node) {
            Some(esc) => render_escalation(node, &esc),
            None => shell::inspector_empty(),
        }
    } else if let Some(node) = st.harness.first_operator_hole() {
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

/// The stuck-node popup (escalation ladder rung 2): `node`'s identity, why it
/// escalated, and a short preview of its own transcript (model-authored —
/// interpolated through maud's `(expr)`, which HTML-escapes by default, the
/// same neutralization discipline every other model-visible text in this
/// crate relies on; no raw-render path is introduced here). Two controls:
/// a form (allocate more turns, optionally WITH a steering message) POSTing
/// to `/steer/:node`, and a plain click button (abort the fan) POSTing to
/// `/steer/:node/abort` — split into two routes rather than one body-shaped
/// endpoint because the vendored client (`shell::OBSERVATORY_JS`) only ever
/// sends a body for `data-on-submit` forms (gathered from `data-bind`
/// fields); a `data-on-click` button always POSTs empty — same discipline
/// `/answer/:node/:key` already uses for a mechanical option click vs.
/// `/answer/:node`'s form submit.
fn render_escalation(node: NodeId, esc: &tidepool_harness::Escalation) -> Markup {
    let allocate_url = format!("/steer/{}", node.0);
    let abort_url = format!("/steer/{}/abort", node.0);
    html! {
        div class="ui-card escalation" {
            h3 class="ui-card-title" { "Node " (node.0) " is awaiting you" }
            p class="escalation-reason" { (esc.reason) }
            pre class="ui-code" { code { (esc.transcript_preview) } }
            form class="escalation-allocate" data-on-submit=(format!("@post('{allocate_url}')")) {
                label for="escalation-turns" { "Allocate more turns:" }
                input id="escalation-turns" type="text" name="turns" data-bind="turns" value="3";
                label for="escalation-steer" { "Optional steering message:" }
                textarea id="escalation-steer" name="steer" data-bind="steer" rows="3" {}
                button type="submit" { "Allocate + continue" }
            }
            button class="ghost escalation-abort" data-on-click=(format!("@post('{abort_url}')")) {
                "Abort fan"
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

/// One node's live heap/GC snapshot, for the heap pane.
struct HeapRow {
    node: u64,
    nursery_bytes: usize,
    live_bytes: usize,
    gc_count: u64,
}

/// Live heap/GC snapshot for every node with a resident session. Unlike
/// `fold_usage`/`fold_effects`, this does NOT fold the log — `Harness::heap_stats`
/// is a straight-through getter over the resident `JitEffectMachine`'s existing
/// counters, so this reads LIVE state directly (a node with no live session,
/// e.g. thunk/done/cancelled, is filtered out by `heap_stats` returning `None`).
fn heap_rows(st: &AppState) -> Vec<HeapRow> {
    st.harness
        .tree_snapshot()
        .iter()
        .filter_map(|n| {
            st.harness.heap_stats(n.node).map(|s| HeapRow {
                node: n.node.0,
                nursery_bytes: s.nursery_bytes,
                live_bytes: s.live_bytes,
                gc_count: s.gc_count,
            })
        })
        .collect()
}

/// Pure render of a heap snapshot (split from `heap_fragment` so it's
/// testable without an `AppState`/live `Harness`).
fn render_heap(rows: &[HeapRow]) -> Markup {
    html! {
        div id="heap" {
            @if rows.is_empty() {
                div class="empty" { "No live sessions yet." }
            } @else {
                table class="heap-table" {
                    thead { tr { th { "node" } th { "nursery" } th { "live" } th { "gc" } } }
                    tbody {
                        @for r in rows {
                            tr {
                                td { "n" (r.node) }
                                td { (r.nursery_bytes) }
                                td { (r.live_bytes) }
                                td { (r.gc_count) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The heap pane markup (id="heap"): per-node live heap/GC snapshot (nursery
/// capacity, session live-bytes high-water mark, GC-generation count).
fn heap_fragment(st: &AppState) -> Markup {
    render_heap(&heap_rows(st))
}

/// The log pane markup (id="log"). Renders the last N events.
/// The conversation transcript (id="transcript") — the full turn-by-turn view
/// the raw event log can't give: grouped by node, no truncation, rendering the
/// actual message CONTENT (the operator's prompt, the Haskell the model wrote
/// with its token cost, the eval result, suspensions, failures). Each turn is
/// an expandable `<details>` (open by default). Folded from the same log the
/// event feed reads, but as a conversation rather than a flat event stream.
/// Live: included in `render_frames`, unlike the raw log pane.
fn transcript_fragment(st: &AppState) -> Markup {
    let by_node = fold_transcript(&st.log_path);
    let selected = st.selected();
    // When a node is focused, show only its conversation; otherwise all.
    let shown: Vec<&(u64, Vec<Markup>)> = by_node
        .iter()
        .filter(|(node, _)| selected.is_none_or(|s| s.0 == *node))
        .collect();
    html! {
        div id="transcript" {
            @if let Some(s) = selected {
                div class="tx-focus" {
                    "focused on node " (s.0) " — click it again in the tree to show all"
                }
            }
            @if shown.is_empty() {
                div class="empty" {
                    @if selected.is_some() {
                        "This node has no turns yet."
                    } @else {
                        "No turns yet. Force a node to begin — its prompt, the Haskell it writes, "
                        "and the result appear here as they happen."
                    }
                }
            }
            @for (node, rows) in &shown {
                div class="tx-node" {
                    div class="tx-node-hdr" { "node " (node) }
                    @for row in rows.iter() { (row) }
                    @if let Some(live) = live_turn_row(st, *node) { (live) }
                }
            }
            // Follow-up composer: continue a focused, completed node's
            // conversation. Shown only for a single Done node (a Suspended node
            // has a hole to answer in the inspector instead).
            @if let Some(sel) = selected {
                @if node_is_done(st, sel) {
                    form class="tx-followup" data-on-submit=(format!("@post('/followup/{}')", sel.0)) {
                        textarea name="message" data-bind="message" rows="2"
                            placeholder=(format!("follow up on node {} — continue this conversation…", sel.0)) {}
                        button type="submit" { "send follow-up ↵" }
                    }
                }
            }
        }
    }
}

/// Whether `node` is currently in the `Done` state (eligible for a follow-up).
fn node_is_done(st: &AppState, node: NodeId) -> bool {
    st.harness
        .tree_snapshot()
        .iter()
        .any(|n| n.node == node && matches!(n.state, NodeState::Done))
}

/// A collapsible "thinking" block (the model's reasoning summary). Collapsed by
/// default for completed turns, open while a turn is live so the operator sees
/// reasoning as it streams.
fn thinking_block(text: &str, open: bool) -> Markup {
    html! {
        details class="tx-think" open[open] {
            summary { "🧠 thinking" }
            pre class="tx-think-body" { code { (text) } }
        }
    }
}

/// The node's in-progress streaming turn, rendered live from the harness's
/// live-turn buffer (answer text growing token-by-token, plus thinking as it
/// arrives). `None` when the node isn't currently streaming.
fn live_turn_row(st: &AppState, node: u64) -> Option<Markup> {
    let live = st.harness.live_turn(NodeId(node))?;
    if live.text.is_empty() && live.reasoning.is_empty() {
        return None;
    }
    Some(html! {
        div class="tx-turn tx-assistant tx-live" {
            div class="tx-live-hdr" { span class="tx-role" { "assistant" } " " span class="tx-streaming" { "streaming…" } }
            @if !live.reasoning.is_empty() { (thinking_block(&live.reasoning, true)) }
            @if !live.text.is_empty() {
                pre class="tx-body" { code { (live.text) } span class="tx-cursor" { "▍" } }
            }
        }
    })
}

/// Fold the log into per-node ordered transcript rows (BTreeMap keeps nodes in
/// id order; each node's rows stay in log `seq` order).
fn fold_transcript(path: &std::path::Path) -> Vec<(u64, Vec<Markup>)> {
    let Ok((_h, iter)) = LogReader::open(path) else {
        return Vec::new();
    };
    let mut by_node: BTreeMap<u64, Vec<Markup>> = BTreeMap::new();
    // `i` is the event's position in the append-only log: a STABLE id per row
    // across renders, so the client can preserve each turn's open/closed state.
    for (i, rec) in iter.flatten().enumerate() {
        if let Some((node, row)) = transcript_row(&rec.event, i as u64) {
            by_node.entry(node).or_default().push(row);
        }
    }
    by_node.into_iter().collect()
}

/// One log event → one transcript row (or `None` for events that aren't turn
/// content: forcing, hole plumbing, fork refs). Full content, never truncated.
/// `row_id` is a stable per-row id so the client preserves `<details>` state.
fn transcript_row(ev: &LogEvent, row_id: u64) -> Option<(u64, Markup)> {
    match ev {
        LogEvent::TurnDelta {
            node,
            role,
            content,
            usage,
            reasoning,
            ..
        } => {
            let (cls, label) = match role {
                Role::Assistant => ("assistant", "assistant"),
                Role::User => ("user", "prompt"),
                Role::System => ("system", "system"),
            };
            let toks = usage
                .as_ref()
                .map(|u| format!("{} in / {} out", u.input_tokens, u.output_tokens))
                .unwrap_or_default();
            Some((
                node.0,
                html! {
                    details id=(format!("txr{row_id}")) class={ "tx-turn tx-" (cls) } open {
                        summary {
                            span class="tx-role" { (label) }
                            @if !toks.is_empty() { " " span class="tx-toks" { (toks) } }
                        }
                        @if let Some(think) = reasoning { (thinking_block(think, false)) }
                        pre class="tx-body" { code { (content) } }
                    }
                },
            ))
        }
        LogEvent::TurnSpliced { node, content, .. } => Some((
            node.0,
            html! {
                details id=(format!("txr{row_id}")) class="tx-turn tx-user" open {
                    summary { span class="tx-role" { "operator" } }
                    pre class="tx-body" { code { (content) } }
                }
            },
        )),
        LogEvent::NodeDone {
            node,
            result_rendered,
        } => Some((
            node.0,
            html! {
                div class="tx-result" {
                    span class="tx-arrow" { "→ result" }
                    pre class="tx-body" { code { (result_rendered) } }
                }
            },
        )),
        LogEvent::HolePublished {
            node,
            prompt,
            ty,
            fork,
            ..
        } => Some((
            node.0,
            html! {
                div class="tx-hole" {
                    span class="tx-arrow" { (if *fork { "⑂ fork hole" } else { "⊙ suspended" }) }
                    @if let Some(t) = ty { " :: " span class="tx-type" { (t) } }
                    div class="tx-hole-prompt" { (prompt) }
                }
            },
        )),
        LogEvent::NodeCancelled { node, reason } => Some((
            node.0,
            html! {
                div class="tx-error" {
                    span class="tx-arrow" { "✕ cancelled" } " " (reason)
                }
            },
        )),
        _ => None,
    }
}

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
        LogEvent::TurnSpliced { node, content, .. } => {
            ("turn_spliced", node.0, content.clone())
        }
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

    /// Render every live pane as a single Datastar patch-elements frame — the
    /// client applies each `[id]` element. Includes the transcript (the
    /// conversation view) and the raw event log, so both update as turns land
    /// rather than only at page load.
    fn render_frames(st: &AppState) -> Event {
        use datastar::prelude::PatchElements;
        let tree = super::tree_fragment(st).into_string();
        let inspector = super::inspector_fragment(st).into_string();
        let meters = super::meters_fragment(st).into_string();
        let trace = super::trace_fragment(st).into_string();
        let heap = super::heap_fragment(st).into_string();
        let transcript = super::transcript_fragment(st).into_string();
        let log = super::log_fragment(st).into_string();
        let combined = format!("{tree}{inspector}{transcript}{meters}{trace}{heap}{log}");
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

/// Continue a completed node's conversation with a new operator message
/// (multi-turn follow-up). Body `{message}`. Fires the turn off the request
/// task (like `force`); the node reopens, streams, and lands at a new
/// hole/done. A failed follow-up reverts the node to `Done` (harness side) so
/// the conversation is preserved.
async fn followup(State(st): State<AppState>, Path(node): Path<u64>, body: Option<Json<Jv>>) -> Response {
    let node = NodeId(node);
    let message = body
        .and_then(|Json(v)| v.get("message").and_then(Jv::as_str).map(str::to_string))
        .unwrap_or_default();
    if message.trim().is_empty() {
        return err_json("follow-up message is empty".to_string());
    }
    let harness = st.harness.clone();
    let st2 = st.clone();
    tokio::spawn(async move {
        if let Err(e) = harness.follow_up(node, &message).await {
            eprintln!("[followup] node {} failed: {e}", node.0);
        }
        st2.ping();
    });
    st.ping();
    Json(json!({"ok": true})).into_response()
}

/// Focus a node (or toggle it off if it's already selected). Pure view state —
/// no harness mutation — so it's synchronous and just re-renders.
async fn select(State(st): State<AppState>, Path(node): Path<u64>) -> Response {
    let node = NodeId(node);
    {
        let mut sel = st.selected.lock().unwrap();
        *sel = if *sel == Some(node) { None } else { Some(node) };
    }
    st.ping();
    Json(json!({"ok": true, "selected": node.0})).into_response()
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

/// Resolve a node's rung-2 escalation with an ALLOCATE-MORE decision: grants
/// `turns` more turns, optionally injecting `steer` as the answerer's next
/// corrective user turn first. Body: `{"turns": <n>, "steer": <text?>}`
/// (`turns` accepts either a JSON number or the string a form field submits;
/// missing/unparseable defaults to 0 — an operator asking for zero more turns
/// is a de-facto abort-by-inaction, not an error). Fires the oneshot the
/// parked `drive_answerer_to_value` await is waiting on; that task resumes
/// and continues asynchronously, so this handler itself returns immediately
/// (same fire-and-ping shape as `fork`/`answer`, minus the spawn — sending
/// on the channel does not block on the answerer's next turn).
async fn steer(
    State(st): State<AppState>,
    Path(node): Path<u64>,
    body: Option<Json<Jv>>,
) -> Response {
    let node = NodeId(node);
    let raw = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let turns = raw
        .get("turns")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
        })
        .unwrap_or(0) as u32;
    let steer = raw
        .get("steer")
        .and_then(Jv::as_str)
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty());
    let decision = tidepool_harness::OperatorDecision::AllocateMore { turns, steer };
    match st.harness.resolve_escalation(node, decision) {
        Ok(()) => {
            st.ping();
            Json(json!({"ok": true, "turns": turns})).into_response()
        }
        Err(e) => err_json(e.to_string()),
    }
}

/// Resolve a node's rung-2 escalation with an ABORT decision: the stuck
/// answerer is cancelled (never left `Running`) and a typed error surfaces
/// to the fan; the parent stays suspended, re-answerable. No body — a plain
/// click, same discipline as `/fork/:node`.
async fn steer_abort(State(st): State<AppState>, Path(node): Path<u64>) -> Response {
    let node = NodeId(node);
    match st
        .harness
        .resolve_escalation(node, tidepool_harness::OperatorDecision::Abort)
    {
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

/// Interject an operator message into `node`'s own transcript (F2's
/// `turn_spliced` kind) — synchronous (a plain transcript append + log
/// write, no session/GHC work), unlike `answer`/`fork`/`confirm`. Body:
/// `{content}`.
async fn splice(
    State(st): State<AppState>,
    Path(node): Path<u64>,
    body: Option<Json<Jv>>,
) -> Response {
    let node = NodeId(node);
    let raw = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let Some(content) = raw.get("content").and_then(Jv::as_str) else {
        return err_json("missing \"content\" (the message to splice in)".to_string());
    };
    match st.harness.splice(node, content) {
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
    /// Escalation ladder rung 2: this node is mid-`drive_answerer_to_value`,
    /// parked awaiting an operator `/steer/:node` decision. Independent of
    /// `state` (still `"running"` — no hole was published).
    awaiting_operator: bool,
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
        awaiting_operator: n.awaiting_operator,
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
        Ok(start) => {
            // Spawn the loopback callback server (:1455) that catches the
            // browser redirect, exchanges the code for a token, and persists
            // it. Without this the authorization URL dead-ends — nothing
            // serves the redirect_uri. `start` carries the state + PKCE
            // verifier that secure THIS attempt; hand the whole LoginStart to
            // complete_login. The task ends when the round-trip completes (or
            // errors); `/auth/status` reflects the persisted token.
            let oauth_cfg = st.oauth.clone();
            let flow = start.clone();
            tokio::spawn(async move {
                if let Err(e) = oauth::complete_login(&oauth_cfg, &flow).await {
                    eprintln!("[auth] callback/login failed: {e}");
                }
            });
            Json(json!({
                "authorization_url": start.authorization_url,
                "port_forward_hint": start.port_forward_hint,
            }))
            .into_response()
        }
        Err(e) => err_json(e.to_string()),
    }
}

/// Human sign-in entry point — the observatory has no auth affordance, and
/// handing the operator a 400-char URL to paste is fragile: terminal/line-wrap
/// whitespace splits a scope token (`email` → `emai l`) and the server rejects
/// it as `invalid_scope`. Serving the URL as an anchor `href` can't be
/// corrupted. Reachable over the same SSH forward as the rest of the
/// observatory (`-L 4600:localhost:4600`); the callback still lands on :1455.
/// Like `auth_start`, this spawns the callback server for THIS attempt —
/// reload only to start a fresh one.
async fn signin_page(State(st): State<AppState>) -> Response {
    match oauth::start_login(&st.oauth).await {
        Ok(start) => {
            let oauth_cfg = st.oauth.clone();
            let flow = start.clone();
            tokio::spawn(async move {
                if let Err(e) = oauth::complete_login(&oauth_cfg, &flow).await {
                    eprintln!("[auth] callback/login failed: {e}");
                }
            });
            let url = start.authorization_url;
            let markup = html! {
                (maud::DOCTYPE)
                html lang="en" {
                    head { meta charset="utf-8"; title { "tidepool harness — sign in" } }
                    body style="font-family:system-ui;background:#0f1117;color:#e6e9f0;padding:3rem;line-height:1.6" {
                        h2 { "Sign in to OpenAI" }
                        p { "Click to authorize with your paid ChatGPT account. No copy-paste — the link can't be mangled." }
                        p {
                            a href=(url) style="display:inline-block;padding:.8rem 1.4rem;background:#10a37f;color:#fff;border-radius:8px;text-decoration:none;font-weight:600" {
                                "Authorize tidepool-harness →"
                            }
                        }
                        hr style="border-color:#262b3a;margin:2rem 0";
                        p style="color:#8b93a7;font-size:.85rem" {
                            "The callback lands on localhost:1455 (your SSH -L forward). When it reports success, return to the terminal."
                        }
                    }
                }
            };
            Html(markup.into_string()).into_response()
        }
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
            reasoning: None,
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
            reasoning: None,
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
            reasoning: None,
            usage: None,
        })
        .unwrap();
        w.append(LogEvent::TurnDelta {
            node: NodeId(1),
            turn: 1,
            role: Role::Assistant,
            content: "ok".into(),
            reasoning: None,
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

    #[test]
    fn snapshot_heap_empty() {
        let rendered = render_heap(&[]).into_string();
        assert_eq!(
            rendered,
            "<div id=\"heap\"><div class=\"empty\">No live sessions yet.</div></div>"
        );
    }

    #[test]
    fn snapshot_heap_with_rows() {
        let rows = vec![
            HeapRow {
                node: 0,
                nursery_bytes: 1 << 20,
                live_bytes: 4096,
                gc_count: 2,
            },
            HeapRow {
                node: 3,
                nursery_bytes: 1 << 20,
                live_bytes: 0,
                gc_count: 0,
            },
        ];
        let rendered = render_heap(&rows).into_string();
        assert_eq!(
            rendered,
            "<div id=\"heap\">\
             <table class=\"heap-table\">\
             <thead><tr><th>node</th><th>nursery</th><th>live</th><th>gc</th></tr></thead>\
             <tbody>\
             <tr><td>n0</td><td>1048576</td><td>4096</td><td>2</td></tr>\
             <tr><td>n3</td><td>1048576</td><td>0</td><td>0</td></tr>\
             </tbody>\
             </table>\
             </div>"
        );
    }
}
