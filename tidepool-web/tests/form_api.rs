//! HTTP-level integration tests for the testing-convenience form API
//! (`tidepool_web::formapi`): `GET`/`POST /api/form` mounted alongside the
//! browser routes, gated by an explicit [`FormApiConfig`].
//!
//! Same shape as `tests/operator_gate.rs`: boot the real router on an
//! ephemeral loopback port, drive it with a real client, prove the round
//! trip against a mock gate — no live model calls.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use serde_json::{json, Value};
use tidepool_harness::selfharness::operator::{FieldShape, FormShape, FormSpec, OperatorGate};
use tidepool_web::{router_with_form_api, AppState, FormApiConfig, WebGate};
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

/// Boot the real router (form API per `enabled`) on an ephemeral loopback
/// port; returns the address and the [`AppState`] used to build a
/// [`WebGate`] against it.
async fn boot(enabled: bool) -> (SocketAddr, AppState) {
    let state = AppState::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_form_api(state.clone(), FormApiConfig { enabled });
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

/// The form-api routes simply don't exist unless explicitly enabled — a
/// caller with no config, no env var, gets a 404 on both verbs, and the
/// existing browser routes are untouched.
#[tokio::test(flavor = "multi_thread")]
async fn disabled_by_default() {
    let (addr, _state) = boot(false).await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let resp = client.get(format!("{base}/api/form")).send().await.unwrap();
    assert_eq!(resp.status(), 404);

    let resp = client
        .post(format!("{base}/api/form"))
        .json(&json!({"nonce": 0, "answer": {}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // The browser surface is unaffected either way.
    let resp = client.get(&base).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

/// Full happy path: GET observes the pending form + a nonce, POST echoes it
/// back with an answer, and the SAME `present_form` call the browser path
/// would resolve unparks with exactly that submission.
#[tokio::test(flavor = "multi_thread")]
async fn get_post_roundtrip() {
    let (addr, state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = Arc::new(WebGate::new(state));
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&sample_spec()));

    let body = wait_for(&client, &format!("{base}/api/form"), |b| {
        b.contains("\"pending\":true")
    })
    .await;
    let v: Value = serde_json::from_str(&body).unwrap();
    assert!(v["test_only"].as_str().is_some());
    assert_eq!(
        v["form"]["shape"]["product"]["fields"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let nonce = v["nonce"]
        .as_u64()
        .expect("nonce present while a form is pending");

    let resp = client
        .post(format!("{base}/api/form"))
        .json(&json!({"nonce": nonce, "answer": {"answer.mood": "calm", "answer.count": 3}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], json!(true));
    assert!(v["test_only"].as_str().is_some());

    let got = handle.await.unwrap();
    assert_eq!(got, json!({"mood": "calm", "count": 3}));

    // Resolved — nothing pending anymore.
    let after: Value = client
        .get(format!("{base}/api/form"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["pending"], json!(false));
    assert_eq!(after["nonce"], Value::Null);
}

/// The nonce is not optional decoration: a missing nonce, a wrong nonce, and
/// a stale nonce (from a form that has since been superseded) are all
/// rejected with the pending form left untouched — only the exact current
/// nonce resolves it. An operator answer is authority regardless of the
/// door it came through, so a caller must prove it actually observed the
/// CURRENT pending occurrence via `GET` first.
#[tokio::test(flavor = "multi_thread")]
async fn nonce_required() {
    let (addr, state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = Arc::new(WebGate::new(state));
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&sample_spec()));

    let body = wait_for(&client, &format!("{base}/api/form"), |b| {
        b.contains("\"pending\":true")
    })
    .await;
    let v: Value = serde_json::from_str(&body).unwrap();
    let nonce = v["nonce"].as_u64().unwrap();

    // Missing nonce.
    let resp = client
        .post(format!("{base}/api/form"))
        .json(&json!({"answer": {"answer.mood": "calm", "answer.count": 1}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert!(v["error"].as_str().unwrap().contains("nonce"));

    // Wrong nonce.
    let resp = client
        .post(format!("{base}/api/form"))
        .json(&json!({"nonce": nonce + 999, "answer": {"answer.mood": "calm", "answer.count": 1}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert!(v["error"].as_str().unwrap().contains("nonce mismatch"));

    // The form is still pending, still the same nonce (a rejected attempt
    // does not consume or perturb it).
    let still: Value = client
        .get(format!("{base}/api/form"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(still["pending"], json!(true));
    assert_eq!(still["nonce"], json!(nonce));

    // The correct nonce resolves it.
    let resp = client
        .post(format!("{base}/api/form"))
        .json(&json!({"nonce": nonce, "answer": {"answer.mood": "calm", "answer.count": 1}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let got = handle.await.unwrap();
    assert_eq!(got, json!({"mood": "calm", "count": 1}));
}

/// A GET with nothing pending reports `pending: false` and no nonce.
#[tokio::test(flavor = "multi_thread")]
async fn get_reports_idle_with_no_nonce() {
    let (addr, _state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let v: Value = client
        .get(format!("{base}/api/form"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["pending"], json!(false));
    assert_eq!(v["nonce"], Value::Null);
    assert_eq!(v["form"], Value::Null);
}

/// Every response — success or error — self-describes as test-only.
#[tokio::test(flavor = "multi_thread")]
async fn responses_self_describe_as_test_only() {
    let (addr, _state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let v: Value = client
        .get(format!("{base}/api/form"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let note = v["test_only"].as_str().unwrap();
    assert!(note.contains("testing"), "{note}");

    let resp = client
        .post(format!("{base}/api/form"))
        .json(&json!({"nonce": 0, "answer": {}}))
        .send()
        .await
        .unwrap();
    let v: Value = resp.json().await.unwrap();
    assert!(v["test_only"].as_str().unwrap().contains("testing"));
}
