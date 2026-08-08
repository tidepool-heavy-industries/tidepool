//! The operator server: axum routes + the SSE stream + [`WebGate`], the
//! [`OperatorGate`] implementation the harness driver blocks on.
//!
//! Loopback bind only: reachability is the authorization boundary.
//!
//! # Verbs
//!
//! - `GET  /` — the operator page (shell + the current panel).
//! - `GET  /sse` — Datastar `patch-elements` stream re-rendering `#panel` on
//!   every state change (broadcast tick).
//! - `POST /submit` — resolve the pending form with a flat `{key: scalar}`
//!   body; unparks `present_form`.
//! - `POST /continue` — resolve the between-loops gate; unparks
//!   `await_continue`.
//!
//! # The gate
//!
//! [`OperatorGate`] is SYNC-BLOCKING by contract (the driver calls it from a
//! blocking context). [`WebGate`] therefore publishes the pending state,
//! pings the SSE tick, and blocks the calling thread on a `oneshot` receiver
//! resolved from an HTTP handler — no async runtime is entered on the driver's
//! side (`blocking_recv`), so this composes with the driver's
//! `block_in_place`/`block_on` turn driving.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value as Jv};
use tidepool_harness::selfharness::operator::{FormSpec, OperatorGate, Submission};
use tokio::sync::{broadcast, oneshot};

use crate::render::{panel, View};
use crate::shell;

/// What the operator is currently being asked for. The single piece of shared
/// state the page renders; every transition pings the SSE tick.
enum Pending {
    /// Nothing pending.
    Idle,
    /// A form is awaiting submission; `resolve` unparks `present_form`.
    Form {
        spec: FormSpec,
        resolve: oneshot::Sender<Submission>,
    },
    /// The between-loops gate; `resolve` unparks `await_continue`.
    Continue { resolve: oneshot::Sender<()> },
}

impl Pending {
    fn view(&self) -> View<'_> {
        match self {
            Pending::Idle => View::Idle,
            Pending::Form { spec, .. } => View::Form(spec),
            Pending::Continue { .. } => View::Continue,
        }
    }
}

/// Shared server state: the pending operator interaction + the re-render tick.
#[derive(Clone)]
pub struct AppState {
    pending: Arc<Mutex<Pending>>,
    /// Broadcast of "the panel changed" — the gate pings this after publishing
    /// a pending interaction so every open SSE stream re-renders promptly.
    tick: broadcast::Sender<()>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        let (tick, _) = broadcast::channel(16);
        AppState {
            pending: Arc::new(Mutex::new(Pending::Idle)),
            tick,
        }
    }

    fn ping(&self) {
        let _ = self.tick.send(());
    }

    /// Render the current panel fragment.
    fn panel_html(&self) -> String {
        let pending = self.pending.lock().unwrap();
        panel(&pending.view()).into_string()
    }

    /// Publish a pending interaction, replacing whatever was there, and ping.
    fn publish(&self, next: Pending) {
        *self.pending.lock().unwrap() = next;
        self.ping();
    }

    /// Take the pending interaction, leaving `Idle`, and ping.
    fn take(&self) -> Pending {
        let taken = std::mem::replace(&mut *self.pending.lock().unwrap(), Pending::Idle);
        self.ping();
        taken
    }
}

/// The web [`OperatorGate`]: publishes the pending interaction for the page to
/// render, then BLOCKS the calling (driver) thread until an HTTP handler
/// resolves it. See the module docs on why this is sync-blocking.
pub struct WebGate {
    state: AppState,
}

impl WebGate {
    pub fn new(state: AppState) -> Self {
        WebGate { state }
    }
}

impl OperatorGate for WebGate {
    fn present_form(&self, spec: &FormSpec) -> Submission {
        let (resolve, wait) = oneshot::channel();
        self.state.publish(Pending::Form {
            spec: spec.clone(),
            resolve,
        });
        // The sender is dropped only if the pending slot is replaced (a newer
        // interaction supersedes this one); an empty submission then re-prompts
        // via the Haskell-side decode retry rather than deadlocking the driver.
        wait.blocking_recv().unwrap_or_default()
    }

    fn await_continue(&self) {
        let (resolve, wait) = oneshot::channel();
        self.state.publish(Pending::Continue { resolve });
        let _ = wait.blocking_recv();
    }
}

/// Build the axum router over the app state.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/sse", get(sse))
        .route("/submit", post(submit))
        .route("/continue", post(continue_loop))
        .with_state(state)
}

async fn page(State(st): State<AppState>) -> Html<String> {
    let pending = st.pending.lock().unwrap();
    Html(shell::page(panel(&pending.view())).into_string())
}

/// The SSE stream: one Datastar `patch-elements` frame per tick, each carrying
/// the freshly rendered `#panel`. An initial frame goes out immediately so a
/// page opened mid-interaction is correct without waiting for a tick.
async fn sse(
    State(st): State<AppState>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    use tokio_stream::wrappers::BroadcastStream;
    use tokio_stream::StreamExt;

    let initial = tokio_stream::iter(std::iter::once(Ok(frame(&st))));
    let st_stream = st.clone();
    let updates = BroadcastStream::new(st.tick.subscribe())
        .filter_map(move |r| r.ok().map(|()| Ok(frame(&st_stream))));

    Sse::new(initial.chain(updates)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

/// One patch-elements frame carrying the current `#panel`.
fn frame(st: &AppState) -> Event {
    use datastar::prelude::PatchElements;
    PatchElements::new(st.panel_html()).write_as_axum_sse_event()
}

/// Resolve the pending form. Body: a FLAT `{ <key>: <scalar> }` object — the
/// canonical [`Submission`] shape, taken verbatim (no coercion here; the client
/// already typed each value by the field's `data-kind`).
async fn submit(State(st): State<AppState>, body: Option<Json<Jv>>) -> Response {
    let raw = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let Jv::Object(submission) = raw else {
        return err_json("submission must be a flat JSON object".to_string());
    };
    match st.take() {
        Pending::Form { resolve, .. } => {
            let _ = resolve.send(submission);
            Json(json!({"ok": true})).into_response()
        }
        other => {
            // Nothing was awaiting a form — put back what we took.
            st.publish(other);
            err_json("no form is pending".to_string())
        }
    }
}

/// Resolve the between-loops gate. No body — a plain click.
async fn continue_loop(State(st): State<AppState>) -> Response {
    match st.take() {
        Pending::Continue { resolve } => {
            let _ = resolve.send(());
            Json(json!({"ok": true})).into_response()
        }
        other => {
            st.publish(other);
            err_json("no continue gate is pending".to_string())
        }
    }
}

fn err_json(msg: String) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(json!({"ok": false, "error": msg})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_harness::selfharness::operator::{EnumOption, Field, FieldKind};

    fn spec() -> FormSpec {
        FormSpec {
            fields: vec![
                Field {
                    key: "mood".into(),
                    label: "Mood".into(),
                    kind: FieldKind::Enum {
                        options: vec![EnumOption {
                            label: "Calm".into(),
                            tag: "calm".into(),
                        }],
                    },
                },
                Field {
                    key: "count".into(),
                    label: "Count".into(),
                    kind: FieldKind::Int,
                },
            ],
        }
    }

    /// The full gate round trip: `present_form` blocks on a worker thread while
    /// the test resolves it through the same path `POST /submit` uses.
    #[test]
    fn present_form_blocks_until_submitted() {
        let st = AppState::new();
        let gate = WebGate::new(st.clone());
        let handle = std::thread::spawn(move || gate.present_form(&spec()));

        // Wait for the form to be published, then resolve it.
        while !matches!(&*st.pending.lock().unwrap(), Pending::Form { .. }) {
            std::thread::yield_now();
        }
        let mut body = Submission::new();
        body.insert("mood".into(), json!("calm"));
        body.insert("count".into(), json!(3));
        match st.take() {
            Pending::Form { resolve, spec } => {
                assert_eq!(spec.fields.len(), 2);
                resolve.send(body.clone()).unwrap();
            }
            _ => panic!("expected a pending form"),
        }

        let got = handle.join().unwrap();
        assert_eq!(got, body);
    }

    #[test]
    fn await_continue_blocks_until_resolved() {
        let st = AppState::new();
        let gate = WebGate::new(st.clone());
        let handle = std::thread::spawn(move || gate.await_continue());

        while !matches!(&*st.pending.lock().unwrap(), Pending::Continue { .. }) {
            std::thread::yield_now();
        }
        match st.take() {
            Pending::Continue { resolve } => resolve.send(()).unwrap(),
            _ => panic!("expected a pending continue"),
        }
        handle.join().unwrap();
    }

    /// The wire round trip: a `FormSpec` serialized and back yields the same
    /// field set the renderer binds against.
    #[test]
    fn form_spec_round_trips_through_serde() {
        let s = spec();
        let wire = serde_json::to_string(&s).unwrap();
        let back: FormSpec = serde_json::from_str(&wire).unwrap();
        assert_eq!(s, back);
        let keys: Vec<&str> = back.fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, vec!["mood", "count"]);
    }
}
