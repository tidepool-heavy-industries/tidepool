//! HTTP-level integration test for `POST /settings` — the operator's
//! model/reasoning-effort dial (60-model-dial). Boots the real axum
//! [`router`] on an ephemeral port, same pattern as `tests/operator_gate.rs`.
//!
//! Covers: the masthead renders the CURRENT dial values; a valid POST
//! mutates the shared handle (and persists to disk — a fresh handle loading
//! from the same path sees the change); a non-allowlisted model or an
//! unrecognized effort is rejected with the pending value left untouched;
//! and — when no live-settings handle was ever wired onto the `AppState`
//! (the demo binary / replay / api-key shape) — the route 404s rather than
//! silently accepting a change nothing will ever read.

use std::net::SocketAddr;

use reqwest::Client;
use tidepool_harness::provider::oauth::ReasoningEffort;
use tidepool_harness::provider::settings::{ModelSettings, SharedModelSettings};
use tidepool_web::{router, AppState};
use tokio::net::TcpListener;

/// Boot the real router on an ephemeral loopback port.
async fn boot(state: AppState) -> (SocketAddr, AppState) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
}

fn settings_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "tp-web-settings-route-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

/// The masthead renders the dial with the CURRENT values pre-selected — a
/// live-state view, never a write-only form — and a valid POST mutates the
/// shared handle, persisting so a FRESH handle loading from the same path
/// (simulating a restart) sees the dialed value, not the original default.
#[tokio::test]
async fn render_shows_current_and_post_mutates_and_persists() {
    let path = settings_path();
    let live = SharedModelSettings::load_or(
        path.clone(),
        ModelSettings::new("gpt-5.6-terra", ReasoningEffort::Medium),
    );
    let state = AppState::new();
    state.set_model_settings(live);
    let (addr, _state) = boot(state).await;
    let client = Client::new();
    let base = format!("http://{addr}");

    let page = client
        .get(format!("{base}/legacy"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("class=\"model-dial\""), "{page}");
    assert!(
        page.contains("value=\"gpt-5.6-terra\" selected"),
        "current model must be pre-selected: {page}"
    );
    assert!(
        page.contains("value=\"medium\" selected"),
        "current effort must be pre-selected: {page}"
    );

    let resp = client
        .post(format!("{base}/settings"))
        .json(&serde_json::json!({"model": "gpt-5.6-sol", "effort": "high"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true, "{body}");

    let page = client
        .get(format!("{base}/legacy"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        page.contains("value=\"gpt-5.6-sol\" selected"),
        "the dial must reflect the just-posted model: {page}"
    );
    assert!(
        page.contains("value=\"high\" selected"),
        "the dial must reflect the just-posted effort: {page}"
    );

    // Restart simulation: a FRESH handle loading from the same path sees
    // the persisted dial choice, not the original construction-time default.
    let reloaded = SharedModelSettings::load_or(
        path,
        ModelSettings::new("gpt-5.6-terra", ReasoningEffort::Medium),
    );
    assert_eq!(
        reloaded.get(),
        ModelSettings::new("gpt-5.6-sol", ReasoningEffort::High)
    );
}

/// A model outside the fixed allowlist is rejected with a 400 naming the
/// problem, and the pending settings are left untouched.
#[tokio::test]
async fn post_rejects_a_non_allowlisted_model() {
    let live = SharedModelSettings::load_or(
        settings_path(),
        ModelSettings::new("gpt-5.6-terra", ReasoningEffort::Medium),
    );
    let state = AppState::new();
    state.set_model_settings(live.clone());
    let (addr, _state) = boot(state).await;
    let client = Client::new();

    let resp = client
        .post(format!("http://{addr}/settings"))
        .json(&serde_json::json!({"model": "gpt-4o-mini", "effort": "high"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("gpt-4o-mini"),
        "{body}"
    );

    assert_eq!(
        live.get(),
        ModelSettings::new("gpt-5.6-terra", ReasoningEffort::Medium),
        "a rejected submission must not mutate the pending settings"
    );
}

/// An unrecognized effort value is rejected the same way — never accepted as
/// free text.
#[tokio::test]
async fn post_rejects_an_unknown_effort() {
    let live = SharedModelSettings::load_or(
        settings_path(),
        ModelSettings::new("gpt-5.6-terra", ReasoningEffort::Medium),
    );
    let state = AppState::new();
    state.set_model_settings(live.clone());
    let (addr, _state) = boot(state).await;
    let client = Client::new();

    let resp = client
        .post(format!("http://{addr}/settings"))
        .json(&serde_json::json!({"model": "gpt-5.6-sol", "effort": "ludicrous"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], false, "{body}");

    assert_eq!(
        live.get(),
        ModelSettings::new("gpt-5.6-terra", ReasoningEffort::Medium),
        "a rejected submission must not mutate the pending settings"
    );
}

/// No live-settings handle ever wired (`AppState::new()` alone — the demo
/// binary / replay / api-key shape): the masthead renders no dial, and the
/// settings route 404s rather than silently accepting a change nothing will
/// ever read.
#[tokio::test]
async fn settings_route_404s_and_masthead_omits_the_dial_when_nothing_is_wired() {
    let (addr, _state) = boot(AppState::new()).await;
    let client = Client::new();
    let base = format!("http://{addr}");

    let page = client
        .get(format!("{base}/legacy"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!page.contains("class=\"model-dial\""), "{page}");

    let resp = client
        .post(format!("{base}/settings"))
        .json(&serde_json::json!({"model": "gpt-5.6-sol", "effort": "high"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
