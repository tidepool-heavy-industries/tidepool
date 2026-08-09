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
//!
//! # Nested submissions — [`collect_form_answer`]
//!
//! The four verbs above and [`WebGate`] serve the FLAT `FormSpec`/
//! `Submission` path — unchanged, still what the live `askUser` effect
//! drives today. [`collect_form_answer`] is the RECURSIVE counterpart: it
//! takes the flat `{"<dotted.path>": <scalar>}` object `render::generic_shape`'s
//! markup produces via `shell::JS`'s ordinary flat collector (see
//! `render.rs`'s module docs) and reassembles it into a structural
//! `FormAnswer`, guided by the same `FormShape` the form was rendered from —
//! so a nested product-of-sum submission comes back nested, not flattened.
//! It is not yet wired into a route: no `Pending`/route emits a `FormShape`
//! today (the Haskell `askUser` swap is a later step), so this is a pure,
//! independently testable function ahead of that wiring.

use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Map, Value as Jv};
use tidepool_harness::selfharness::operator::{
    child_path, FormAnswer, FormShape, FormSpec, OperatorGate, Submission,
};
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

/// The pending interaction plus a revision counter, both behind ONE lock
/// (F10) — `rev` must never be read/bumped out of step with `pending`, or a
/// client could observe a `data-rev` that doesn't actually correspond to the
/// panel content it was stamped on.
struct Slot {
    pending: Pending,
    /// Bumped on every [`AppState::publish`]/[`AppState::take`] — a NEW
    /// pending interaction (or its resolution back to `Idle`) always gets a
    /// fresh revision, so the client's focus-preserving skip rule (`shell.rs`)
    /// never mistakes "the operator submitted, and a new form/gate replaced
    /// it" for "the same form re-rendered".
    rev: u64,
}

/// Shared server state: the pending operator interaction + the re-render tick.
#[derive(Clone)]
pub struct AppState {
    slot: Arc<Mutex<Slot>>,
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
            slot: Arc::new(Mutex::new(Slot {
                pending: Pending::Idle,
                rev: 0,
            })),
            tick,
        }
    }

    fn ping(&self) {
        let _ = self.tick.send(());
    }

    /// Render the current panel fragment.
    fn panel_html(&self) -> String {
        let slot = self.slot.lock().unwrap();
        panel(&slot.pending.view(), slot.rev).into_string()
    }

    /// Publish a pending interaction, replacing whatever was there, bump the
    /// revision, and ping.
    fn publish(&self, next: Pending) {
        let mut slot = self.slot.lock().unwrap();
        slot.pending = next;
        slot.rev += 1;
        drop(slot);
        self.ping();
    }

    /// Take the pending interaction, leaving `Idle`, bump the revision, and
    /// ping.
    fn take(&self) -> Pending {
        let mut slot = self.slot.lock().unwrap();
        let taken = std::mem::replace(&mut slot.pending, Pending::Idle);
        slot.rev += 1;
        drop(slot);
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
    let slot = st.slot.lock().unwrap();
    Html(shell::page(panel(&slot.pending.view(), slot.rev)).into_string())
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

// -------------------------------------------------------------------------
// Nested submissions — flat wire object -> structural FormAnswer.
// -------------------------------------------------------------------------

/// Reassemble a flat `{"<dotted.path>": <scalar>}` submission (exactly what
/// `render::generic_shape`'s markup, collected by `shell::JS`'s ordinary
/// flat `[data-bind]` walk, produces) into a structural [`FormAnswer`],
/// guided by the [`FormShape`] the form was rendered from. `path` is the
/// root bind path used at render time (`""` for a form rendered at the
/// root).
///
/// Rejects rather than guesses, mirroring `uiof::resume_expr_from_submission`:
/// a missing leaf, a wrong-typed JSON scalar, or an unrecognized sum
/// constructor all yield `None`. For a payload-bearing sum, only the CHOSEN
/// variant's fields are read — the other variants' inputs are present in
/// `raw` (they're always rendered) but their keys are never looked at, so a
/// non-chosen branch's leftover/unfilled values never leak into the answer.
#[must_use]
pub fn collect_form_answer(shape: &FormShape, path: &str, raw: &Map<String, Jv>) -> Option<FormAnswer> {
    match shape {
        FormShape::String => match raw.get(path)? {
            Jv::String(s) => Some(FormAnswer::String(s.clone())),
            _ => None,
        },
        FormShape::Int => match raw.get(path)? {
            Jv::Number(n) => n.as_i64().map(FormAnswer::Int),
            _ => None,
        },
        FormShape::Number => match raw.get(path)? {
            Jv::Number(n) => n.as_f64().map(FormAnswer::Number),
            _ => None,
        },
        FormShape::Bool => match raw.get(path)? {
            Jv::Bool(b) => Some(FormAnswer::Bool(*b)),
            _ => None,
        },
        FormShape::Unit => Some(FormAnswer::Unit),
        FormShape::Optional(inner) => {
            let present_key = format!("{path}.__present");
            let present = matches!(raw.get(&present_key), Some(Jv::Bool(true)));
            if present {
                collect_form_answer(inner, path, raw).map(|a| FormAnswer::Optional(Some(Box::new(a))))
            } else {
                Some(FormAnswer::Optional(None))
            }
        }
        FormShape::Product { fields, .. } => {
            let mut out = Vec::with_capacity(fields.len());
            for field in fields {
                let child = child_path(path, &field.key);
                let value = collect_form_answer(&field.shape, &child, raw)?;
                out.push((field.key.clone(), value));
            }
            Some(FormAnswer::Product(out))
        }
        FormShape::Sum { variants, .. } => {
            let chosen = match raw.get(path)? {
                Jv::String(s) => s.clone(),
                _ => return None,
            };
            let variant = variants.iter().find(|v| v.constructor == chosen)?;
            let child = child_path(path, &variant.constructor);
            let payload = collect_form_answer(&variant.shape, &child, raw)?;
            Some(FormAnswer::Sum {
                constructor: chosen,
                payload: Box::new(payload),
            })
        }
    }
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
        while !matches!(st.slot.lock().unwrap().pending, Pending::Form { .. }) {
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

        while !matches!(st.slot.lock().unwrap().pending, Pending::Continue { .. }) {
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

    fn extract_rev(html: &str) -> &str {
        let after = html
            .split("data-rev=\"")
            .nth(1)
            .expect("panel html carries data-rev");
        after.split('"').next().unwrap()
    }

    /// F10: every [`AppState::publish`]/[`AppState::take`] bumps the revision
    /// stamped into the rendered panel — the signal the client uses to tell a
    /// genuinely new pending interaction apart from a same-interaction
    /// re-render.
    #[test]
    fn panel_html_data_rev_bumps_on_every_publish_and_take() {
        let st = AppState::new();
        let rev0 = extract_rev(&st.panel_html()).to_string();

        st.publish(Pending::Continue {
            resolve: oneshot::channel().0,
        });
        let rev1 = extract_rev(&st.panel_html()).to_string();
        assert_ne!(rev0, rev1, "publish must bump the revision");

        st.take();
        let rev2 = extract_rev(&st.panel_html()).to_string();
        assert_ne!(rev1, rev2, "take must bump the revision");
    }

    // ---- collect_form_answer -------------------------------------------------

    use tidepool_harness::selfharness::operator::{FieldShape, VariantShape};

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
                    shape: FormShape::Unit,
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

    fn field<'a>(fields: &'a [(String, FormAnswer)], key: &str) -> Option<&'a FormAnswer> {
        fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    #[test]
    fn collect_form_answer_reassembles_nested_product_of_sum() {
        let raw = obj(json!({
            "service": "api",
            "destination": "Ssh",
            "destination.Ssh.host": "example.com",
            "destination.Ssh.port": 22,
            "releaseNote.__present": true,
            "releaseNote": "hotfix",
        }));
        let answer = collect_form_answer(&deploy_request_shape(), "", &raw).expect("collects");
        let FormAnswer::Product(fields) = &answer else {
            panic!("expected a Product answer");
        };
        assert_eq!(field(fields, "service"), Some(&FormAnswer::String("api".to_string())));
        assert_eq!(
            field(fields, "destination"),
            Some(&FormAnswer::Sum {
                constructor: "Ssh".to_string(),
                payload: Box::new(FormAnswer::Product(vec![
                    ("host".to_string(), FormAnswer::String("example.com".to_string())),
                    ("port".to_string(), FormAnswer::Int(22)),
                ])),
            })
        );
        assert_eq!(
            field(fields, "releaseNote"),
            Some(&FormAnswer::Optional(Some(Box::new(FormAnswer::String("hotfix".to_string())))))
        );
    }

    /// A payload-bearing sum always renders every branch's inputs at once
    /// (see `render.rs`); the collector must read only the CHOSEN branch and
    /// ignore stray values left over from the unselected one(s).
    #[test]
    fn collect_form_answer_ignores_unselected_branch_fields() {
        let raw = obj(json!({
            "service": "api",
            "destination": "LocalHost",
            "destination.Ssh.host": "example.com",
            "destination.Ssh.port": 22,
            "releaseNote.__present": false,
            "releaseNote": "ignored because __present is false",
        }));
        let answer = collect_form_answer(&deploy_request_shape(), "", &raw).expect("collects");
        let FormAnswer::Product(fields) = &answer else {
            panic!("expected a Product answer");
        };
        assert_eq!(
            field(fields, "destination"),
            Some(&FormAnswer::Sum {
                constructor: "LocalHost".to_string(),
                payload: Box::new(FormAnswer::Unit),
            })
        );
        assert_eq!(field(fields, "releaseNote"), Some(&FormAnswer::Optional(None)));
    }

    #[test]
    fn collect_form_answer_rejects_missing_field() {
        let raw = obj(json!({ "service": "api" }));
        assert_eq!(collect_form_answer(&deploy_request_shape(), "", &raw), None);
    }

    #[test]
    fn collect_form_answer_rejects_unknown_constructor() {
        let raw = obj(json!({
            "service": "api",
            "destination": "Nope",
            "releaseNote.__present": false,
        }));
        assert_eq!(collect_form_answer(&deploy_request_shape(), "", &raw), None);
    }

    /// DONE criterion: the display-humanization / exact-key split, asserted
    /// end to end. Render the shape, scrape the exact bind paths back out of
    /// the HTML (not a hand-written guess at what the renderer emits), build
    /// a submission at exactly those paths, and confirm
    /// `collect_form_answer` reconstructs the same exact keys — while the
    /// rendered HTML shows only humanized label text, never the raw key.
    #[test]
    fn render_and_collect_round_trip_exact_keys_while_labels_humanize() {
        use crate::render::generic_shape;

        let shape = deploy_request_shape();
        let html = generic_shape("", &shape).into_string();

        assert!(html.contains("data-bind=\"service\""));
        assert!(html.contains("data-bind=\"destination\""));
        assert!(html.contains("data-bind=\"releaseNote.__present\""));
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
            "releaseNote.__present": true,
            "releaseNote": "hotfix",
        }));
        let answer = collect_form_answer(&shape, "", &raw).expect("collects");
        let FormAnswer::Product(fields) = &answer else {
            panic!("expected a Product answer");
        };
        let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["service", "destination", "releaseNote"]);
    }
}
