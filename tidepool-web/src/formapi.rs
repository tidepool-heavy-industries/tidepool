//! Testing-convenience HTTP surface on the operator gate:
//! `GET /node/{node}/api/form` returns every currently pending FORM ask for
//! `node`, each with its interaction id (the nonce); `POST
//! /node/{node}/api/form` echoes that nonce back with an answer to resolve
//! the SAME pending ask a browser `/node/{node}/submit/{interaction}` would
//! — a second front door onto [`crate::server::AppState`]'s node-scoped
//! resolution, not a second gate.
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
//! - **Per-ask nonce.** Every pending form has a nonce — its own interaction
//!   id, assigned once when it's published and never reused — returned by
//!   `GET` and REQUIRED on `POST`. A submission naming an id that no longer
//!   names a pending form (already resolved, wrong node, or never existed)
//!   is rejected rather than silently resolving whatever else happens to be
//!   pending.
//! - **Self-describing.** Every response carries a `test_only` note.
//!
//! An operator answer is authority regardless of which door it came through
//! — this surface does not get to be "just for testing" about that.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Map, Value as Jv};

use crate::server::{parse_json_body, resolve_error_json, AppState};

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

/// Mount `GET`/`POST /node/{node}/api/form` onto `router` iff
/// `config.enabled`; returns `router` unchanged when disabled — the route
/// table itself differs, not just its runtime behavior.
pub fn merge(router: Router<AppState>, config: FormApiConfig) -> Router<AppState> {
    if !config.enabled {
        return router;
    }
    router.route("/node/{node}/api/form", get(get_form).post(submit_form))
}

/// `{"test_only": ..., "pending": bool, "forms": [{"interaction": u64, "form": FormShape}, ...]}`,
/// or a 400 naming an unregistered node.
async fn get_form(State(st): State<AppState>, Path(node): Path<String>) -> Response {
    match st.pending_forms(&node) {
        Ok(forms) => {
            let items: Vec<Jv> = forms
                .into_iter()
                .map(|(interaction, shape)| json!({"interaction": interaction, "form": shape}))
                .collect();
            Json(json!({
                "test_only": TEST_ONLY_NOTE,
                "pending": !items.is_empty(),
                "forms": items,
            }))
            .into_response()
        }
        Err(()) => err(format!("unknown node {node:?}")),
    }
}

/// Resolve one pending form. Body: `{"interaction": <n>, "answer": {<key>:
/// <scalar>, ...}}` — `interaction` is one of the ids the matching `GET`
/// returned, `answer` is the flat dotted-path object taken verbatim, same as
/// a browser `/node/{node}/submit/{interaction}`. Same rejection discipline
/// as the browser verb: an unparseable/absent body, or an `answer` that
/// doesn't decode against the pending form's shape, is a 400 that leaves the
/// pending ask untouched — never a silent `{}`.
async fn submit_form(
    State(st): State<AppState>,
    Path(node): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let raw = match parse_json_body(&headers, &body) {
        Ok(v) => v,
        Err(msg) => return err(msg),
    };
    let (interaction, answer) = match parse_submission(raw) {
        Ok(pair) => pair,
        Err(msg) => return err(msg),
    };
    match st.resolve_form(&node, interaction, answer) {
        Ok(()) => Json(json!({"test_only": TEST_ONLY_NOTE, "ok": true})).into_response(),
        Err(e) => {
            let mut body = resolve_error_json(&node, &e, "form");
            if let Jv::Object(ref mut map) = body {
                map.insert("test_only".to_string(), json!(TEST_ONLY_NOTE));
            }
            (axum::http::StatusCode::BAD_REQUEST, Json(body)).into_response()
        }
    }
}

fn parse_submission(raw: Jv) -> Result<(u64, Map<String, Jv>), String> {
    let Jv::Object(mut obj) = raw else {
        return Err(
            "body must be a JSON object: {\"interaction\": <n>, \"answer\": {...}}".to_string(),
        );
    };
    let interaction = match obj.remove("interaction") {
        #[allow(clippy::unwrap_used, reason = "match guard already confirmed is_some()")]
        Some(Jv::Number(n)) if n.as_u64().is_some() => n.as_u64().unwrap(),
        _ => {
            return Err(
                "missing/invalid \"interaction\" — must be one of the ids GET /node/{node}/api/form returned"
                    .to_string(),
            )
        }
    };
    let answer = match obj.remove("answer") {
        Some(Jv::Object(m)) => m,
        _ => return Err("missing/invalid \"answer\" — must be a flat JSON object".to_string()),
    };
    Ok((interaction, answer))
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
