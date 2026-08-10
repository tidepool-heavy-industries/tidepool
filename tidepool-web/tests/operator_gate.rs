//! HTTP-level integration test: boots the real axum [`router`] on an
//! ephemeral port and drives it with a real client, proving the
//! [`OperatorGate`] round trip end to end — no hand-wired handler calls.
//!
//! Assertions are scoped to the wire contract only (the `id="panel"` root,
//! `data-bind`/`data-kind`, `@post` targets, and JSON bodies) — never on
//! visual markup, since `render.rs`/`shell.rs` are under concurrent redesign.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use tidepool_harness::selfharness::operator::{FieldShape, FormShape, FormSpec, OperatorGate};
use tidepool_web::{router, AppState, WebGate};
use tokio::net::TcpListener;

fn sample_spec() -> FormSpec {
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

/// Boot the real router on an ephemeral loopback port; returns the address
/// and the [`AppState`] used to build a [`WebGate`] against it.
async fn boot() -> (SocketAddr, AppState) {
    let state = AppState::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state)
}

/// Poll `GET <url>` until `pred` matches the body, or panic after 5s.
async fn wait_for(client: &Client, url: &str, pred: impl Fn(&str) -> bool) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let body = client.get(url).send().await.unwrap().text().await.unwrap();
        if pred(&body) {
            return body;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for a condition on {url}; last body was:\n{body}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_resolves_present_form_with_exact_submission() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = Arc::new(WebGate::new(state));
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&sample_spec()));

    // The pending form shows up on the page, wired for the two fields.
    let html = wait_for(&client, &format!("{base}/"), |b| {
        b.contains("data-bind=\"answer.mood\"")
    })
    .await;
    assert!(html.contains("id=\"panel\""));
    assert!(html.contains("data-bind=\"answer.count\""));
    assert!(html.contains("data-kind=\"string\""));
    assert!(html.contains("data-kind=\"int\""));
    assert!(html.contains("@post('/submit')"));

    let body = json!({"answer.mood": "calm", "answer.count": 3});
    let resp = client
        .post(format!("{base}/submit"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v, json!({"ok": true}));

    let got = handle.await.unwrap();
    assert_eq!(got, json!({"mood": "calm", "count": 3}));
}

#[tokio::test(flavor = "multi_thread")]
async fn continue_resolves_await_continue() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = Arc::new(WebGate::new(state));
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.await_continue());

    let html = wait_for(&client, &format!("{base}/"), |b| {
        b.contains("@post('/continue')")
    })
    .await;
    assert!(html.contains("id=\"panel\""));

    let resp = client
        .post(format!("{base}/continue"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v, json!({"ok": true}));

    handle.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_without_pending_form_returns_400() {
    let (addr, _state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let resp = client
        .post(format!("{base}/submit"))
        .json(&json!({"anything": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], json!(false));
    assert!(v["error"].as_str().unwrap().contains("no form is pending"));
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_with_non_object_body_is_rejected() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = Arc::new(WebGate::new(state));
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&sample_spec()));
    wait_for(&client, &format!("{base}/"), |b| {
        b.contains("data-bind=\"answer.mood\"")
    })
    .await;

    let resp = client
        .post(format!("{base}/submit"))
        .json(&json!([1, 2, 3]))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], json!(false));

    // The rejected body never touched the pending slot — the form is still
    // there, and a well-formed submission resolves the still-blocked driver.
    let body = json!({"answer.mood": "calm", "answer.count": 0});
    let resp = client
        .post(format!("{base}/submit"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let got = handle.await.unwrap();
    assert_eq!(got, json!({"mood": "calm", "count": 0}));
}

#[tokio::test(flavor = "multi_thread")]
async fn mismatched_verb_preserves_pending_interaction() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = Arc::new(WebGate::new(state));
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&sample_spec()));
    wait_for(&client, &format!("{base}/"), |b| {
        b.contains("data-bind=\"answer.mood\"")
    })
    .await;

    // A form is pending, not a continue gate — /continue must be rejected...
    let resp = client
        .post(format!("{base}/continue"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], json!(false));
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("no continue gate is pending"));

    // ...and the original form must still be pending, not dropped.
    let html = client
        .get(format!("{base}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("data-bind=\"answer.mood\""));

    let body = json!({"answer.mood": "calm", "answer.count": 1});
    let resp = client
        .post(format!("{base}/submit"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let got = handle.await.unwrap();
    assert_eq!(got, json!({"mood": "calm", "count": 1}));
}

#[tokio::test(flavor = "multi_thread")]
async fn sse_first_frame_patches_panel() {
    let (addr, _state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let resp = client.get(format!("{base}/sse")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !buf.contains("\n\n") {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for an SSE frame");
        let chunk = tokio::time::timeout(remaining, stream.next())
            .await
            .expect("timed out waiting for an SSE frame")
            .expect("SSE stream ended before a frame arrived")
            .expect("SSE stream error");
        buf.push_str(&String::from_utf8_lossy(&chunk));
    }

    // Event name + the patched root only — never inner markup.
    assert!(buf.contains("event: datastar-patch-elements"), "got: {buf}");
    assert!(buf.contains("id=\"panel\""), "got: {buf}");
}
