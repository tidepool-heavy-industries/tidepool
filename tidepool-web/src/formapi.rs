//! Testing-convenience HTTP surface on the operator gate: `GET /api/form`
//! returns the pending form as JSON (the same [`FormSpec`] the web renderer
//! consumes) plus a nonce; `POST /api/form` echoes that nonce back with an
//! answer to resolve the SAME pending gate a browser `/submit` would — a
//! second front door onto [`crate::server::WebGate`], not a second gate.
//!
//! HARDENING IS PART OF THE SPEC, not optional:
//! - **Disabled by default.** These routes are mounted only when
//!   [`FormApiConfig::enabled`] is true, itself sourced from
//!   `TIDEPOOL_FORM_API=1` via [`FormApiConfig::from_env`] — nothing else
//!   turns it on. A disabled config makes [`merge`] return the input router
//!   UNCHANGED: the routes do not exist on the resulting `Router`, not
//!   merely 404 behind a runtime flag check.
//! - **Loopback only**, inherited structurally rather than re-implemented:
//!   these routes mount onto the SAME router [`crate::spawn_operator_server`]
//!   serves from its one hardcoded `127.0.0.1` listener. This module never
//!   binds a socket of its own.
//! - **Per-prompt nonce.** Every pending form has a nonce — the shared
//!   [`crate::server::AppState`] revision counter, already bumped exactly
//!   once per publish/take under F10's single-lock discipline — returned by
//!   `GET` and REQUIRED on `POST`. A submission naming a stale nonce (the
//!   pending form already changed underneath it) is rejected rather than
//!   silently resolving whatever happens to be pending now.
//! - **Self-describing.** Every response carries a `test_only` note.
//!
//! An operator answer is authority regardless of which door it came through
//! — this surface does not get to be "just for testing" about that.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Map, Value as Jv};

use crate::server::AppState;

const TEST_ONLY_NOTE: &str = "tidepool-web form-api: testing convenience only, not a browser \
     surface — gated on TIDEPOOL_FORM_API=1, loopback bind only";

/// Whether the form-api routes are mounted. Disabled unless explicitly
/// turned on — see module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FormApiConfig {
    pub enabled: bool,
}

impl FormApiConfig {
    /// `TIDEPOOL_FORM_API=1` enables; anything else — unset, empty, `"true"`,
    /// `"0"` — disables. The parsing itself is a pure function
    /// ([`env_flag_enabled`]) so it's tested without touching process env.
    pub fn from_env() -> Self {
        FormApiConfig {
            enabled: env_flag_enabled(std::env::var("TIDEPOOL_FORM_API").ok().as_deref()),
        }
    }
}

fn env_flag_enabled(v: Option<&str>) -> bool {
    v == Some("1")
}

/// Why a form-api `POST` was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormApiSubmitError {
    /// Nothing is pending (idle, or a Continue gate rather than a form).
    NoFormPending,
    /// The nonce didn't match the pending form's current revision — either
    /// stale (the form changed since it was last GET'd) or never observed.
    NonceMismatch { current: u64 },
}

/// Mount `GET`/`POST /api/form` onto `router` iff `config.enabled`; returns
/// `router` unchanged when disabled — the route table itself differs, not
/// just its runtime behavior.
pub fn merge(router: Router<AppState>, config: FormApiConfig) -> Router<AppState> {
    if !config.enabled {
        return router;
    }
    router.route("/api/form", get(get_form).post(submit_form))
}

/// `{"test_only": ..., "pending": bool, "form": FormSpec | null, "nonce": u64 | null}`.
async fn get_form(State(st): State<AppState>) -> Json<Jv> {
    Json(match st.pending_form() {
        Some((spec, nonce)) => json!({
            "test_only": TEST_ONLY_NOTE,
            "pending": true,
            "form": spec,
            "nonce": nonce,
        }),
        None => json!({
            "test_only": TEST_ONLY_NOTE,
            "pending": false,
            "form": null,
            "nonce": null,
        }),
    })
}

/// Resolve the pending form. Body: `{"nonce": <n>, "answer": {<key>: <scalar>,
/// ...}}` — `nonce` is the value the matching `GET` returned, `answer` is the
/// flat dotted-path object taken verbatim, same as a browser `/submit`.
async fn submit_form(State(st): State<AppState>, body: Option<Json<Jv>>) -> Response {
    let raw = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let (nonce, answer) = match parse_submission(raw) {
        Ok(pair) => pair,
        Err(msg) => return err(msg),
    };
    match st.submit_form(nonce, answer) {
        Ok(()) => Json(json!({"test_only": TEST_ONLY_NOTE, "ok": true})).into_response(),
        Err(FormApiSubmitError::NoFormPending) => err("no form is pending".to_string()),
        Err(FormApiSubmitError::NonceMismatch { current }) => err(format!(
            "nonce mismatch: the pending form's current nonce is {current} — GET /api/form again before submitting"
        )),
    }
}

fn parse_submission(raw: Jv) -> Result<(u64, Map<String, Jv>), String> {
    let Jv::Object(mut obj) = raw else {
        return Err("body must be a JSON object: {\"nonce\": <n>, \"answer\": {...}}".to_string());
    };
    let nonce = match obj.remove("nonce") {
        Some(Jv::Number(n)) if n.as_u64().is_some() => n.as_u64().unwrap(),
        _ => {
            return Err(
                "missing/invalid \"nonce\" — must be the integer GET /api/form returned"
                    .to_string(),
            )
        }
    };
    let answer = match obj.remove("answer") {
        Some(Jv::Object(m)) => m,
        _ => return Err("missing/invalid \"answer\" — must be a flat JSON object".to_string()),
    };
    Ok((nonce, answer))
}

fn err(msg: String) -> Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(json!({"test_only": TEST_ONLY_NOTE, "ok": false, "error": msg})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The config's own default (no env consulted) is disabled — the
    /// zero-config, zero-env-var state exposes nothing.
    #[test]
    fn disabled_by_default() {
        assert!(!FormApiConfig::default().enabled);
        assert!(!env_flag_enabled(None));
    }

    #[test]
    fn env_flag_requires_exact_string_one() {
        assert!(!env_flag_enabled(Some("true")));
        assert!(!env_flag_enabled(Some("0")));
        assert!(!env_flag_enabled(Some("")));
        assert!(env_flag_enabled(Some("1")));
    }
}
