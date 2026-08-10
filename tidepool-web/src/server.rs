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
//! [`router_with_form_api`] additionally mounts `GET`/`POST /api/form` — a
//! disabled-by-default testing-convenience surface over the SAME pending
//! form; see [`crate::formapi`] for the wire shape and hardening properties.
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
//! # Shape-guided submissions — [`collect_form_json`]
//!
//! [`collect_form_json`] takes the
//! flat `{"<dotted.path>": <scalar>}` object `render::generic_shape`'s
//! markup produces via `shell::JS`'s ordinary flat collector (see
//! `render.rs`'s module docs) and reassembles it into the PLAIN JSON the
//! answer type's generic `FromJSON` decode reads — record objects, tagged
//! record sums, bare strings for enums, `null` for absent optionals. There
//! is no intermediate answer language.
//!
//! Both browser and form-API submissions use this one conversion.

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
    child_path, FormShape, FormSpec, OperatorGate, ROOT_BIND_PATH,
};
use tokio::sync::{broadcast, oneshot};

use crate::formapi::{FormApiConfig, FormApiSubmitError};
use crate::render::{panel, View};
use crate::shell;

/// What the operator is currently being asked for. The single piece of shared
/// state the page renders; every transition pings the SSE tick.
enum Pending {
    /// Nothing pending.
    Idle,
    /// A form is awaiting submission; `resolve` unparks `present_form`.
    /// Carries the ANSWER VALUE (see `OperatorGate::present_form`), which may
    /// be an object, scalar, or `null` depending on the form shape.
    Form {
        spec: FormSpec,
        resolve: oneshot::Sender<Jv>,
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

    /// The form-api `GET` view: a clone of the pending form's spec plus its
    /// nonce (the current revision), or `None` when nothing is pending or the
    /// pending interaction is a Continue gate rather than a form.
    pub fn pending_form(&self) -> Option<(FormSpec, u64)> {
        let slot = self.slot.lock().unwrap();
        match &slot.pending {
            Pending::Form { spec, .. } => Some((spec.clone(), slot.rev)),
            _ => None,
        }
    }

    /// The form-api `POST` resolution: resolve the pending form iff `nonce`
    /// matches its current revision. Same resolution as a browser `/submit`
    /// (take, bump the revision, ping SSE, send on the oneshot) — a second
    /// front door onto the same gate, not a second gate.
    pub fn submit_form(
        &self,
        nonce: u64,
        submission: Map<String, Jv>,
    ) -> Result<(), FormApiSubmitError> {
        let mut slot = self.slot.lock().unwrap();
        match &slot.pending {
            Pending::Form { .. } if slot.rev == nonce => {
                let taken = std::mem::replace(&mut slot.pending, Pending::Idle);
                slot.rev += 1;
                drop(slot);
                self.ping();
                let Pending::Form { spec, resolve } = taken else {
                    unreachable!("matched Pending::Form above")
                };
                // Same reassembly as the browser `/submit` — this path used
                // to forward the raw flat map, so a shape-carrying form
                // submitted via the API was never decodable Haskell-side.
                let _ = resolve.send(answer_value(&spec, submission));
                Ok(())
            }
            Pending::Form { .. } => Err(FormApiSubmitError::NonceMismatch { current: slot.rev }),
            _ => Err(FormApiSubmitError::NoFormPending),
        }
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
    fn present_form(&self, spec: &FormSpec) -> Jv {
        let (resolve, wait) = oneshot::channel();
        self.state.publish(Pending::Form {
            spec: spec.clone(),
            resolve,
        });
        // The sender is dropped only if the pending slot is replaced (a newer
        // interaction supersedes this one); an empty submission then re-prompts
        // via the Haskell-side decode retry rather than deadlocking the driver.
        wait.blocking_recv().unwrap_or_else(|_| json!({}))
    }

    fn await_continue(&self) {
        let (resolve, wait) = oneshot::channel();
        self.state.publish(Pending::Continue { resolve });
        let _ = wait.blocking_recv();
    }
}

/// Build the axum router over the app state. The form-api testing surface is
/// disabled — equivalent to `router_with_form_api(state, FormApiConfig::default())`.
pub fn router(state: AppState) -> Router {
    router_with_form_api(state, FormApiConfig::default())
}

/// Build the axum router, optionally mounting the form-api testing surface
/// (`crate::formapi`) alongside the four browser verbs. See that module's
/// docs for the hardening properties `form_api` gates.
pub fn router_with_form_api(state: AppState, form_api: FormApiConfig) -> Router {
    let base: Router<AppState> = Router::new()
        .route("/", get(page))
        .route("/sse", get(sse))
        .route("/submit", post(submit))
        .route("/continue", post(continue_loop));
    crate::formapi::merge(base, form_api).with_state(state)
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

/// Resolve the pending form. The client posts a flat dotted-path object; the
/// shape-guided collector below validates and reassembles it.
async fn submit(State(st): State<AppState>, body: Option<Json<Jv>>) -> Response {
    let raw = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let Jv::Object(submission) = raw else {
        return err_json("submission must be a flat JSON object".to_string());
    };
    match st.take() {
        Pending::Form { spec, resolve } => {
            let _ = resolve.send(answer_value(&spec, submission));
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
// Nested submissions — flat wire object -> the plain JSON answer.
// -------------------------------------------------------------------------

/// Convert a client's flat POST to the answer returned by the gate. The
/// browser `/submit` and form API share this path. It reassembles dotted bind
/// paths into the ordinary JSON the answer type's generic `FromJSON` reads;
/// there is no intermediate answer language. An incomplete or wrong-typed
/// submission resolves to `{}`, which that decode rejects and re-presents —
/// the same path every malformed submission takes.
fn answer_value(spec: &FormSpec, submission: Map<String, Jv>) -> Jv {
    collect_form_json(&spec.shape, ROOT_BIND_PATH, &submission).unwrap_or_else(|| json!({}))
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
mod tests {
    use super::*;
    use tidepool_harness::selfharness::operator::FieldShape;

    fn spec() -> FormSpec {
        FormSpec {
            shape: FormShape::Product {
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
            },
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
        let body = json!({"mood": "calm", "count": 3});
        match st.take() {
            Pending::Form { resolve, spec } => {
                assert!(matches!(spec.shape, FormShape::Product { .. }));
                resolve.send(body.clone()).unwrap();
            }
            _ => panic!("expected a pending form"),
        }

        let got = handle.join().unwrap();
        assert_eq!(got, body);
    }

    /// The regression this transport exists for: a unit-shaped form
    /// (`askUser @()`) answers as `null`.
    /// The old object-only transport coerced non-object answers to `{}`,
    /// which the decode rejects — an infinite re-prompt.
    #[test]
    fn unit_shaped_answer_survives_reassembly() {
        let spec = FormSpec {
            shape: FormShape::Unit,
        };
        assert_eq!(answer_value(&spec, Map::new()), Jv::Null);
    }

    /// An incomplete shape submission still degrades to `{}` (reject and
    /// re-present), not a panic and not a partial answer.
    #[test]
    fn incomplete_shape_submission_degrades_to_the_rejectable_empty_object() {
        let spec = FormSpec {
            shape: FormShape::String,
        };
        assert_eq!(answer_value(&spec, Map::new()), json!({}));
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

    /// The wire round trip preserves the form shape.
    #[test]
    fn form_spec_round_trips_through_serde() {
        let s = spec();
        let wire = serde_json::to_string(&s).unwrap();
        let back: FormSpec = serde_json::from_str(&wire).unwrap();
        assert_eq!(s, back);
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
        let spec = FormSpec {
            shape: destination_shape(),
        };
        let html = crate::render::panel(&crate::render::View::Form(&spec), 1).into_string();
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
