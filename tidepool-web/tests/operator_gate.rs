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
use tidepool_harness::selfharness::operator::{FieldShape, FormShape, OperatorGate};
use tidepool_web::{router, AppState, WebGate};
use tokio::net::TcpListener;

fn sample_spec() -> FormShape {
    FormShape::Product {
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

    assert_eq!(
        handle.await.unwrap(),
        tidepool_harness::ContinueSignal::Continue,
        "a bodiless click is a bare continue"
    );
}

/// The continue gate is the `ContinueSignal` SUM rendered through the
/// generic machinery: the page carries both variants' radio options and the
/// payload branch's text field; a flat tagged submission reassembles into
/// `ContinueWithInput` and reaches `await_continue`.
#[tokio::test(flavor = "multi_thread")]
async fn continue_with_input_carries_the_operator_message() {
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
    // Both variants render (the sum form, not a bespoke pane).
    assert!(html.contains(r#"value="Continue""#));
    assert!(html.contains(r#"value="ContinueWithInput""#));

    let resp = client
        .post(format!("{base}/continue"))
        .json(&json!({
            "answer": "ContinueWithInput",
            "answer.ContinueWithInput.input": "hello companion",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    assert_eq!(
        handle.await.unwrap(),
        tidepool_harness::ContinueSignal::ContinueWithInput("hello companion".to_string()),
        "the chosen payload variant's field reaches await_continue"
    );
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

/// `post_note` does not block (unlike `present_form`/`await_continue`), so a
/// driver can post narration and then immediately present the form it
/// explains. This proves the ordering survives the real HTTP round trip:
/// notes render in POST order, and ABOVE the pending form — never after it.
#[tokio::test(flavor = "multi_thread")]
async fn post_note_appears_above_the_pending_form_in_post_order() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = Arc::new(WebGate::new(state));
    gate.post_note("first note");
    gate.post_note("second note");

    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&sample_spec()));

    let html = wait_for(&client, &format!("{base}/"), |b| {
        b.contains("data-bind=\"answer.mood\"")
    })
    .await;

    let first_pos = html.find("first note").expect("first note rendered");
    let second_pos = html.find("second note").expect("second note rendered");
    let form_pos = html
        .find("data-bind=\"answer.mood\"")
        .expect("form rendered");
    assert!(
        first_pos < second_pos,
        "notes must render in post order:\n{html}"
    );
    assert!(
        second_pos < form_pos,
        "notes must render ABOVE the pending form:\n{html}"
    );

    // Resolve the form so the spawned blocking task doesn't leak.
    let body = json!({"answer.mood": "calm", "answer.count": 0});
    client
        .post(format!("{base}/submit"))
        .json(&body)
        .send()
        .await
        .unwrap();
    handle.await.unwrap();
}

/// The note feed clears at a loop boundary (`await_continue`), so the next
/// loop's page doesn't still show the prior loop's narration.
#[tokio::test(flavor = "multi_thread")]
async fn await_continue_clears_the_note_feed() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = Arc::new(WebGate::new(state));
    gate.post_note("prior loop's note");

    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.await_continue());
    wait_for(&client, &format!("{base}/"), |b| {
        b.contains("@post('/continue')")
    })
    .await;

    let resp = client
        .post(format!("{base}/continue"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    handle.await.unwrap();

    let html = client
        .get(format!("{base}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !html.contains("prior loop's note"),
        "the note feed must clear at the loop boundary:\n{html}"
    );
}

/// `post_turn_source` ACCUMULATES a turn history: every posted source stays
/// on the page (newest first in the pane), rendered regardless of what else
/// is pending — the operator scrolls back through past turns.
#[tokio::test(flavor = "multi_thread")]
async fn post_turn_source_accumulates_a_history() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = WebGate::new(state);
    gate.post_turn_source("finalize @Decision Approve");
    gate.post_turn_source("finalize @Decision Reject");

    let html = client
        .get(format!("{base}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("finalize @Decision Reject"), "{html}");
    assert!(html.contains("finalize @Decision Approve"), "{html}");
    assert!(html.contains("Haskell turns (2)"), "{html}");
    // Newest first in the pane.
    let reject = html.find("finalize @Decision Reject").unwrap();
    let approve = html.find("finalize @Decision Approve").unwrap();
    assert!(reject < approve, "newest turn renders first:\n{html}");
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
