//! HTTP-level integration tests for the testing-convenience form API
//! (`tidepool_web::formapi`): `GET`/`POST /node/{node}/api/form` mounted
//! alongside the browser routes, gated by an explicit `enabled` bool.
//!
//! Same shape as `tests/operator_gate.rs`: boot the real router on an
//! ephemeral loopback port, drive it with a real client, prove the round
//! trip against a mock gate — no live model calls.

use std::net::SocketAddr;
use std::time::Duration;

use reqwest::Client;
use serde_json::{json, Value};
use tidepool_harness::selfharness::operator::{FieldShape, FormShape, OperatorGate};
use tidepool_web::{router_with_form_api, AppState};
use tokio::net::TcpListener;

fn sample_spec() -> FormShape {
    FormShape::Product {
        type_key: "Sample".into(),
        constructor: "Sample".into(),
        fields: vec![
            FieldShape {
                key: "mood".into(),
                shape: FormShape::String,
                doc: None,
            },
            FieldShape {
                key: "count".into(),
                shape: FormShape::Int,
                doc: None,
            },
        ],
        doc: None,
    }
}

/// Boot the real router (form API per `enabled`) on an ephemeral loopback
/// port; returns the address and the [`AppState`] used to register nodes
/// against it.
async fn boot(enabled: bool) -> (SocketAddr, AppState) {
    let state = AppState::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router_with_form_api(state.clone(), enabled);
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
    let (addr, state) = boot(false).await;
    let base = format!("http://{addr}");
    let client = Client::new();
    let _gate = state.register_node("n1");

    let resp = client
        .get(format!("{base}/node/n1/api/form"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    let resp = client
        .post(format!("{base}/node/n1/api/form"))
        .json(&json!({"interaction": 0, "answer": {}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // The browser surface is unaffected either way.
    let resp = client.get(&base).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

/// Full happy path: GET observes the pending form + its interaction id
/// (the nonce), POST echoes it back with an answer, and the SAME
/// `present_form` call the browser path would resolve unparks with exactly
/// that submission.
#[tokio::test(flavor = "multi_thread")]
async fn get_post_roundtrip() {
    let (addr, state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = state.register_node("n1");
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&sample_spec()));

    let body = wait_for(&client, &format!("{base}/node/n1/api/form"), |b| {
        b.contains("\"pending\":true")
    })
    .await;
    let v: Value = serde_json::from_str(&body).unwrap();
    assert!(v["test_only"].as_str().is_some());
    let forms = v["forms"].as_array().unwrap();
    assert_eq!(forms.len(), 1);
    assert_eq!(
        forms[0]["form"]["product"]["fields"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let interaction = forms[0]["interaction"]
        .as_u64()
        .expect("interaction id present while a form is pending");

    let resp = client
        .post(format!("{base}/node/n1/api/form"))
        .json(&json!({"interaction": interaction, "answer": {"answer.mood": "calm", "answer.count": 3}}))
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
        .get(format!("{base}/node/n1/api/form"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["pending"], json!(false));
    assert_eq!(after["forms"], json!([]));
}

/// The interaction id is not optional decoration: a missing id, a wrong id,
/// and a stale id (from a form that has since been resolved) are all
/// rejected with the pending form left untouched — only the exact current
/// id resolves it. An operator answer is authority regardless of the door it
/// came through, so a caller must prove it actually observed the CURRENT
/// pending occurrence via `GET` first.
#[tokio::test(flavor = "multi_thread")]
async fn interaction_id_required() {
    let (addr, state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = state.register_node("n1");
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&sample_spec()));

    let body = wait_for(&client, &format!("{base}/node/n1/api/form"), |b| {
        b.contains("\"pending\":true")
    })
    .await;
    let v: Value = serde_json::from_str(&body).unwrap();
    let interaction = v["forms"][0]["interaction"].as_u64().unwrap();

    // Missing interaction id.
    let resp = client
        .post(format!("{base}/node/n1/api/form"))
        .json(&json!({"answer": {"answer.mood": "calm", "answer.count": 1}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert!(v["error"].as_str().unwrap().contains("interaction"));

    // Wrong interaction id.
    let resp = client
        .post(format!("{base}/node/n1/api/form"))
        .json(&json!({"interaction": interaction + 999, "answer": {"answer.mood": "calm", "answer.count": 1}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("no such pending interaction"));

    // The form is still pending, still the same interaction id (a rejected
    // attempt does not consume or perturb it).
    let still: Value = client
        .get(format!("{base}/node/n1/api/form"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(still["pending"], json!(true));
    assert_eq!(still["forms"][0]["interaction"], json!(interaction));

    // The correct id resolves it.
    let resp = client
        .post(format!("{base}/node/n1/api/form"))
        .json(&json!({"interaction": interaction, "answer": {"answer.mood": "calm", "answer.count": 1}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let got = handle.await.unwrap();
    assert_eq!(got, json!({"mood": "calm", "count": 1}));
}

/// A GET with nothing pending reports `pending: false` and an empty forms
/// list.
#[tokio::test(flavor = "multi_thread")]
async fn get_reports_idle_with_no_forms() {
    let (addr, state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();
    let _gate = state.register_node("n1");

    let v: Value = client
        .get(format!("{base}/node/n1/api/form"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["pending"], json!(false));
    assert_eq!(v["forms"], json!([]));
}

/// GET against a node that was never registered is rejected — never a 200
/// that pretends nothing is pending.
#[tokio::test(flavor = "multi_thread")]
async fn get_against_unknown_node_is_rejected() {
    let (addr, _state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let resp = client
        .get(format!("{base}/node/ghost/api/form"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert!(v["error"].as_str().unwrap().contains("unknown node"));
}

/// TWO nodes, each with a pending form via the form-api: each node's GET
/// only ever lists its OWN pending form, and resolving one never disturbs
/// the other's.
#[tokio::test(flavor = "multi_thread")]
async fn form_api_is_scoped_per_node() {
    let (addr, state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate_a = state.register_node("alpha");
    let gate_b = state.register_node("beta");
    let handle_a = tokio::task::spawn_blocking(move || gate_a.present_form(&sample_spec()));
    let handle_b = tokio::task::spawn_blocking(move || gate_b.present_form(&sample_spec()));

    let body_a = wait_for(&client, &format!("{base}/node/alpha/api/form"), |b| {
        b.contains("\"pending\":true")
    })
    .await;
    let body_b = wait_for(&client, &format!("{base}/node/beta/api/form"), |b| {
        b.contains("\"pending\":true")
    })
    .await;
    let interaction_a: Value = serde_json::from_str(&body_a).unwrap();
    let interaction_b: Value = serde_json::from_str(&body_b).unwrap();
    let interaction_a = interaction_a["forms"][0]["interaction"].as_u64().unwrap();
    let interaction_b = interaction_b["forms"][0]["interaction"].as_u64().unwrap();

    let resp = client
        .post(format!("{base}/node/beta/api/form"))
        .json(&json!({"interaction": interaction_b, "answer": {"answer.mood": "b", "answer.count": 2}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(handle_b.await.unwrap(), json!({"mood": "b", "count": 2}));

    // alpha's pending form is untouched by beta's resolution.
    let still: Value = client
        .get(format!("{base}/node/alpha/api/form"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(still["pending"], json!(true));
    assert_eq!(still["forms"][0]["interaction"], json!(interaction_a));

    let resp = client
        .post(format!("{base}/node/alpha/api/form"))
        .json(&json!({"interaction": interaction_a, "answer": {"answer.mood": "a", "answer.count": 1}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(handle_a.await.unwrap(), json!({"mood": "a", "count": 1}));
}

/// TWO concurrent asks on ONE node both show up in one `GET`, each under its
/// own interaction id, and each resolves independently via the form-api.
#[tokio::test(flavor = "multi_thread")]
async fn form_api_lists_and_resolves_stacked_asks_independently() {
    let (addr, state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = state.register_node("n1");
    let g1 = gate.clone();
    let handle1 = tokio::task::spawn_blocking(move || g1.present_form(&sample_spec()));
    let g2 = gate.clone();
    let handle2 = tokio::task::spawn_blocking(move || g2.present_form(&sample_spec()));

    let body = wait_for(&client, &format!("{base}/node/n1/api/form"), |b| {
        let v: Value = serde_json::from_str(b).unwrap_or(json!({}));
        v["forms"].as_array().map(|a| a.len()) == Some(2)
    })
    .await;
    let v: Value = serde_json::from_str(&body).unwrap();
    let ids: Vec<u64> = v["forms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["interaction"].as_u64().unwrap())
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "each stacked ask has its own nonce");

    let resp = client
        .post(format!("{base}/node/n1/api/form"))
        .json(&json!({"interaction": ids[0], "answer": {"answer.mood": "x", "answer.count": 100}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let mid: Value = client
        .get(format!("{base}/node/n1/api/form"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        mid["forms"].as_array().unwrap().len(),
        1,
        "the sibling ask survives"
    );
    assert_eq!(mid["forms"][0]["interaction"], json!(ids[1]));

    let resp = client
        .post(format!("{base}/node/n1/api/form"))
        .json(&json!({"interaction": ids[1], "answer": {"answer.mood": "y", "answer.count": 200}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let mut got = vec![handle1.await.unwrap(), handle2.await.unwrap()];
    got.sort_by_key(|v| v["count"].as_i64().unwrap());
    assert_eq!(
        got,
        vec![
            json!({"mood": "x", "count": 100}),
            json!({"mood": "y", "count": 200}),
        ]
    );
}

/// A pending form whose shape carries docs returns them through `GET
/// /node/{node}/api/form` — the form-api is a second front door onto the
/// same `FormShape`, so a doc-carrying shape passes through automatically,
/// with no special-casing.
#[tokio::test(flavor = "multi_thread")]
async fn pending_form_docs_pass_through_get() {
    let (addr, state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let doc_spec = FormShape::Product {
        type_key: "Sample".into(),
        constructor: "Sample".into(),
        fields: vec![FieldShape {
            key: "mood".into(),
            shape: FormShape::String,
            doc: Some("How you're feeling right now.".into()),
        }],
        doc: Some("A quick check-in.".into()),
    };

    let gate = state.register_node("n1");
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&doc_spec));

    let body = wait_for(&client, &format!("{base}/node/n1/api/form"), |b| {
        b.contains("\"pending\":true")
    })
    .await;
    let v: Value = serde_json::from_str(&body).unwrap();
    let form = &v["forms"][0]["form"];
    assert_eq!(form["product"]["doc"], json!("A quick check-in."));
    assert_eq!(
        form["product"]["fields"][0]["doc"],
        json!("How you're feeling right now.")
    );

    let interaction = v["forms"][0]["interaction"].as_u64().unwrap();
    client
        .post(format!("{base}/node/n1/api/form"))
        .json(&json!({"interaction": interaction, "answer": {"answer.mood": "calm"}}))
        .send()
        .await
        .unwrap();
    handle.await.unwrap();
}

/// Parity with the browser `/submit` verb: an `answer` object that doesn't
/// decode against the pending shape is rejected with a 400 naming the
/// expected paths, and the pending ask survives — never a silent
/// `{"ok":true}`. THE live repro: a bare `seedQuestion` key.
#[tokio::test(flavor = "multi_thread")]
async fn submit_form_rejects_invalid_answer_and_preserves_pending_ask() {
    let (addr, state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let shape = FormShape::Product {
        type_key: "Brief".into(),
        constructor: "Brief".into(),
        fields: vec![FieldShape {
            key: "seedQuestion".into(),
            shape: FormShape::String,
            doc: None,
        }],
        doc: None,
    };

    let gate = state.register_node("n1");
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&shape));

    let body = wait_for(&client, &format!("{base}/node/n1/api/form"), |b| {
        b.contains("\"pending\":true")
    })
    .await;
    let v: Value = serde_json::from_str(&body).unwrap();
    let interaction = v["forms"][0]["interaction"].as_u64().unwrap();

    let resp = client
        .post(format!("{base}/node/n1/api/form"))
        .json(&json!({"interaction": interaction, "answer": {"seedQuestion": "what next?"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], json!(false));
    assert!(v["test_only"].as_str().is_some());
    assert!(
        v["error"].as_str().unwrap().contains("answer.seedQuestion"),
        "must name the expected path: {v}"
    );
    assert_eq!(v["missing_keys"], json!(["answer.seedQuestion"]));

    // Still pending, same interaction id — untouched by the rejection.
    let still: Value = client
        .get(format!("{base}/node/n1/api/form"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(still["pending"], json!(true));
    assert_eq!(still["forms"][0]["interaction"], json!(interaction));

    let resp = client
        .post(format!("{base}/node/n1/api/form"))
        .json(&json!({"interaction": interaction, "answer": {"answer.seedQuestion": "what next?"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(handle.await.unwrap(), json!({"seedQuestion": "what next?"}));
}

/// An unparseable/absent body is rejected before the `{"interaction": ...}`
/// wire shape is even inspected — parity with the browser verb.
#[tokio::test(flavor = "multi_thread")]
async fn submit_form_rejects_absent_body() {
    let (addr, state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();
    let _gate = state.register_node("n1");

    let resp = client
        .post(format!("{base}/node/n1/api/form"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], json!(false));
    assert!(v["test_only"].as_str().is_some());
}

/// Every response — success or error — self-describes as test-only.
#[tokio::test(flavor = "multi_thread")]
async fn responses_self_describe_as_test_only() {
    let (addr, state) = boot(true).await;
    let base = format!("http://{addr}");
    let client = Client::new();
    let _gate = state.register_node("n1");

    let v: Value = client
        .get(format!("{base}/node/n1/api/form"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let note = v["test_only"].as_str().unwrap();
    assert!(note.contains("testing"), "{note}");

    let resp = client
        .post(format!("{base}/node/n1/api/form"))
        .json(&json!({"interaction": 0, "answer": {}}))
        .send()
        .await
        .unwrap();
    let v: Value = resp.json().await.unwrap();
    assert!(v["test_only"].as_str().unwrap().contains("testing"));
}
