//! HTTP-level integration test: boots the real axum [`router`] on an
//! ephemeral port and drives it with a real client, proving the
//! [`OperatorGate`] round trip end to end for MULTIPLE registered nodes,
//! each able to carry SEVERAL concurrently pending asks — no hand-wired
//! handler calls.
//!
//! Assertions are scoped to the wire contract only (the `id="panel-<node>"`
//! root, `data-bind`/`data-kind`, `@post` targets, and JSON bodies) — never
//! on visual markup, since `render.rs`/`shell.rs` are under concurrent
//! redesign.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use tidepool_harness::selfharness::operator::{FieldShape, FormShape, OperatorGate};
use tidepool_web::{router, AppState};
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
/// and the [`AppState`] used to register nodes / build [`WebGate`]s against
/// it.
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

/// Every `@post('<prefix>...')` target found in `html`, in document order —
/// the exact URL(s) `data-on-submit` bakes in for a given verb prefix (e.g.
/// `/node/n1/submit/`). Multiple matches occur when several asks are
/// stacked.
fn all_post_urls(html: &str, prefix: &str) -> Vec<String> {
    let needle = format!("@post('{prefix}");
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(idx) = rest.find(&needle) {
        let after = &rest[idx + "@post('".len()..];
        let end = after.find('\'').expect("closing quote");
        out.push(after[..end].to_string());
        rest = &after[end..];
    }
    out
}

fn one_post_url(html: &str, prefix: &str) -> String {
    let urls = all_post_urls(html, prefix);
    assert_eq!(
        urls.len(),
        1,
        "expected exactly one match for {prefix:?} in:\n{html}"
    );
    urls.into_iter().next().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_resolves_present_form_with_exact_submission() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = state.register_node("n1");
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&sample_spec()));

    // The pending form shows up on the page, wired for the two fields.
    let html = wait_for(&client, &format!("{base}/"), |b| {
        b.contains("data-bind=\"answer.mood\"")
    })
    .await;
    assert!(html.contains("id=\"panel-n1\""));
    assert!(html.contains("data-bind=\"answer.count\""));
    assert!(html.contains("data-kind=\"string\""));
    assert!(html.contains("data-kind=\"int\""));
    let submit_url = one_post_url(&html, "/node/n1/submit/");

    let body = json!({"answer.mood": "calm", "answer.count": 3});
    let resp = client
        .post(format!("{base}{submit_url}"))
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

    let gate = state.register_node("n1");
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.await_continue());

    let html = wait_for(&client, &format!("{base}/"), |b| {
        b.contains("/node/n1/continue/")
    })
    .await;
    assert!(html.contains("id=\"panel-n1\""));
    let continue_url = one_post_url(&html, "/node/n1/continue/");

    let resp = client
        .post(format!("{base}{continue_url}"))
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

    let gate = state.register_node("n1");
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.await_continue());

    let html = wait_for(&client, &format!("{base}/"), |b| {
        b.contains("/node/n1/continue/")
    })
    .await;
    // Both variants render (the sum form, not a bespoke pane).
    assert!(html.contains(r#"value="Continue""#));
    assert!(html.contains(r#"value="ContinueWithInput""#));
    let continue_url = one_post_url(&html, "/node/n1/continue/");

    let resp = client
        .post(format!("{base}{continue_url}"))
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
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();
    let _gate = state.register_node("n1");

    let resp = client
        .post(format!("{base}/node/n1/submit/0"))
        .json(&json!({"anything": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], json!(false));
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("no such pending interaction"));
}

/// Submitting against a node that was never registered is rejected the same
/// way — never a panic, never silently resolving some other node's ask.
#[tokio::test(flavor = "multi_thread")]
async fn submit_against_unknown_node_returns_400() {
    let (addr, _state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let resp = client
        .post(format!("{base}/node/ghost/submit/0"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert!(v["error"].as_str().unwrap().contains("unknown node"));
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_with_non_object_body_is_rejected() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = state.register_node("n1");
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&sample_spec()));
    let html = wait_for(&client, &format!("{base}/"), |b| {
        b.contains("data-bind=\"answer.mood\"")
    })
    .await;
    let submit_url = one_post_url(&html, "/node/n1/submit/");

    let resp = client
        .post(format!("{base}{submit_url}"))
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
        .post(format!("{base}{submit_url}"))
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

    let gate = state.register_node("n1");
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&sample_spec()));
    let html = wait_for(&client, &format!("{base}/"), |b| {
        b.contains("data-bind=\"answer.mood\"")
    })
    .await;
    let submit_url = one_post_url(&html, "/node/n1/submit/");
    // The interaction id is the trailing path segment — a form is pending at
    // this id, not a continue gate, so hitting /continue at the SAME id must
    // be rejected without dropping the form.
    let interaction = submit_url.rsplit('/').next().unwrap();

    let resp = client
        .post(format!("{base}/node/n1/continue/{interaction}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], json!(false));
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("not a pending continue gate"));

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
        .post(format!("{base}{submit_url}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let got = handle.await.unwrap();
    assert_eq!(got, json!({"mood": "calm", "count": 1}));
}

/// Once resolved, an interaction's id is gone — resubmitting the SAME url
/// (a stale nonce) is rejected, never silently resolving a different pending
/// ask.
#[tokio::test(flavor = "multi_thread")]
async fn stale_interaction_after_resolution_is_rejected() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = state.register_node("n1");
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&sample_spec()));
    let html = wait_for(&client, &format!("{base}/"), |b| {
        b.contains("data-bind=\"answer.mood\"")
    })
    .await;
    let submit_url = one_post_url(&html, "/node/n1/submit/");

    let body = json!({"answer.mood": "calm", "answer.count": 1});
    let resp = client
        .post(format!("{base}{submit_url}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    handle.await.unwrap();

    // Same URL again — the interaction no longer exists.
    let resp = client
        .post(format!("{base}{submit_url}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("no such pending interaction"));
}

/// TWO NODES, each with a pending form, resolved independently in EITHER
/// order — resolving one never touches the other's pending state.
#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_with_pending_forms_resolve_independently_in_either_order() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate_a = state.register_node("alpha");
    let gate_b = state.register_node("beta");
    let handle_a = tokio::task::spawn_blocking(move || gate_a.present_form(&sample_spec()));
    let handle_b = tokio::task::spawn_blocking(move || gate_b.present_form(&sample_spec()));

    let html = wait_for(&client, &format!("{base}/"), |b| {
        b.contains("/node/alpha/submit/") && b.contains("/node/beta/submit/")
    })
    .await;
    assert!(html.contains("id=\"panel-alpha\""));
    assert!(html.contains("id=\"panel-beta\""));
    assert!(html.contains("data-node-id=\"alpha\""));
    assert!(html.contains("data-node-id=\"beta\""));

    let url_a = one_post_url(&html, "/node/alpha/submit/");
    let url_b = one_post_url(&html, "/node/beta/submit/");

    // Resolve beta FIRST, then alpha — order must not matter.
    let resp = client
        .post(format!("{base}{url_b}"))
        .json(&json!({"answer.mood": "beta-mood", "answer.count": 2}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        handle_b.await.unwrap(),
        json!({"mood": "beta-mood", "count": 2})
    );

    // alpha is still pending and unaffected.
    let html = client
        .get(format!("{base}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        html.contains("/node/alpha/submit/"),
        "alpha's ask survives beta's resolution"
    );

    let resp = client
        .post(format!("{base}{url_a}"))
        .json(&json!({"answer.mood": "alpha-mood", "answer.count": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        handle_a.await.unwrap(),
        json!({"mood": "alpha-mood", "count": 1})
    );
}

/// TWO CONCURRENT ASKS on ONE node: both render (stacked, neither
/// superseding the other) and both resolve independently, in either order —
/// the concurrency invariant PRD 20's fanout windows depend on.
#[tokio::test(flavor = "multi_thread")]
async fn two_concurrent_asks_on_one_node_both_render_and_resolve() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = state.register_node("n1");
    let g1 = gate.clone();
    let handle1 = tokio::task::spawn_blocking(move || g1.present_form(&sample_spec()));
    let g2 = gate.clone();
    let handle2 = tokio::task::spawn_blocking(move || g2.present_form(&sample_spec()));

    let html = wait_for(&client, &format!("{base}/"), |b| {
        all_post_urls(b, "/node/n1/submit/").len() == 2
    })
    .await;
    let urls = all_post_urls(&html, "/node/n1/submit/");
    assert_eq!(
        urls.len(),
        2,
        "both concurrent asks render, neither dropped"
    );
    assert_ne!(urls[0], urls[1], "each ask has its own address/nonce");

    // Resolve the SECOND-listed ask first.
    let resp = client
        .post(format!("{base}{}", urls[1]))
        .json(&json!({"answer.mood": "second", "answer.count": 20}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // The first ask is STILL pending (not dropped by resolving its sibling).
    let html = client
        .get(format!("{base}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(
        all_post_urls(&html, "/node/n1/submit/"),
        vec![urls[0].clone()]
    );

    let resp = client
        .post(format!("{base}{}", urls[0]))
        .json(&json!({"answer.mood": "first", "answer.count": 10}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Both driver calls unpark with exactly what was submitted to their own
    // ask — which internal `present_form` call happened to land at `urls[0]`
    // vs `urls[1]` is a scheduling detail, not something this test pins, so
    // compare the two results as a SET: exactly one call got "first"/10 and
    // the other got "second"/20, never both getting the same value and never
    // a value going missing.
    let mut got = vec![handle1.await.unwrap(), handle2.await.unwrap()];
    got.sort_by_key(|v| v["count"].as_i64().unwrap());
    assert_eq!(
        got,
        vec![
            json!({"mood": "first", "count": 10}),
            json!({"mood": "second", "count": 20}),
        ]
    );
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

    let gate = state.register_node("n1");
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
    let submit_url = one_post_url(&html, "/node/n1/submit/");
    let body = json!({"answer.mood": "calm", "answer.count": 0});
    client
        .post(format!("{base}{submit_url}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    handle.await.unwrap();
}

/// The timeline is append-only across loop boundaries: notes survive a
/// continue click (they are the context of everything that follows), and the
/// clicked gate itself stays on the page as its answered form.
#[tokio::test(flavor = "multi_thread")]
async fn notes_and_answered_continue_persist_across_the_loop_boundary() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = state.register_node("n1");
    gate.post_note("prior loop's note");

    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.await_continue());
    let html = wait_for(&client, &format!("{base}/"), |b| {
        b.contains("/node/n1/continue/")
    })
    .await;
    let continue_url = one_post_url(&html, "/node/n1/continue/");

    let resp = client
        .post(format!("{base}{continue_url}"))
        .json(&json!({"answer": "ContinueWithInput",
                      "answer.ContinueWithInput.input": "steer toward receipts"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        handle.await.unwrap(),
        tidepool_harness::selfharness::operator::ContinueSignal::ContinueWithInput(
            "steer toward receipts".to_string()
        )
    );

    let html = client
        .get(format!("{base}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        html.contains("prior loop's note"),
        "notes persist across the loop boundary:\n{html}"
    );
    assert!(
        html.contains("steer toward receipts"),
        "the operator's continue message stays on the timeline:\n{html}"
    );
    assert!(
        !html.contains("/node/n1/continue/"),
        "no live continue form remains after the click:\n{html}"
    );
}

/// A node whose id is a tree path (literal slashes — every labeled branch
/// child) must be answerable through its OWN baked `@post` URL, exactly as
/// served: the renderer percent-encodes the id into one path segment, axum
/// decodes it back, and the submission resolves. Pins the zero-context
/// probe's finding (2026-08-19): the raw-slash form 404'd before any
/// handler ran, making every tree child unanswerable from the page.
#[tokio::test(flavor = "multi_thread")]
async fn slash_path_node_submits_through_its_own_baked_url() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = state.register_node("root/1-finishes");
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.present_form(&sample_spec()));

    let html = wait_for(&client, &format!("{base}/"), |b| {
        b.contains("/node/root%2F1-finishes/submit/")
    })
    .await;
    let submit_url = one_post_url(&html, "/node/root%2F1-finishes/submit/");

    let resp = client
        .post(format!("{base}{submit_url}"))
        .json(&json!({"answer.mood": "calm", "answer.count": 3}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the served URL must resolve as-is");
    assert_eq!(
        handle.await.unwrap(),
        json!({"mood": "calm", "count": 3}),
        "the submission reaches the slash-path node's own gate"
    );
}

/// The two node-lifecycle fields the wire newly carries — the seed at birth
/// and the final value at the fold — render on the node's own section, with
/// the derived status flipping to done.
#[tokio::test(flavor = "multi_thread")]
async fn seed_and_final_value_render_on_the_nodes_section() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let default_gate = state.register_node("root");
    let _child = default_gate.node_gate("root/1-x").expect("child gate");
    default_gate.node_seeded("root/1-x", "NODE root/1 — DISCOVER: execution semantics");
    default_gate.retire_node("root/1-x");
    default_gate.node_finalized("root/1-x", "{\"tag\":\"FinishLayer\"}");

    let html = client
        .get(format!("{base}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("id=\"panel-root/1-x\""), "{html}");
    assert!(
        html.contains("DISCOVER: execution semantics"),
        "the seed shows on the section:\n{html}"
    );
    assert!(
        html.contains("FinishLayer"),
        "the final value shows:\n{html}"
    );
    assert!(html.contains(">done</span>"), "{html}");
}

/// `post_turn_source` ACCUMULATES a turn history: every posted source stays
/// on the page (newest first in the pane), rendered regardless of what else
/// is pending — the operator scrolls back through past turns.
#[tokio::test(flavor = "multi_thread")]
async fn post_turn_source_accumulates_a_history() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = state.register_node("n1");
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
async fn sse_first_frame_patches_panel_for_every_registered_node() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();
    let _alpha = state.register_node("alpha");
    let _beta = state.register_node("beta");

    let resp = client.get(format!("{base}/sse")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    // Two nodes registered before connect => two initial frames.
    while buf.matches("event: datastar-patch-elements").count() < 2 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for both initial SSE frames; got:\n{buf}"
        );
        let chunk = tokio::time::timeout(remaining, stream.next())
            .await
            .expect("timed out waiting for an SSE frame")
            .expect("SSE stream ended before both frames arrived")
            .expect("SSE stream error");
        buf.push_str(&String::from_utf8_lossy(&chunk));
    }

    assert!(buf.contains("id=\"panel-alpha\""), "got: {buf}");
    assert!(buf.contains("id=\"panel-beta\""), "got: {buf}");
}

/// A node registered AFTER an SSE stream connects still reaches that stream
/// as a patch frame (registration pings the tick) — the client mounts it
/// into the tree on first sight, so the operator watches the tree grow
/// without reloading.
#[tokio::test(flavor = "multi_thread")]
async fn sse_emits_a_frame_for_a_node_registered_after_connect() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();
    let _alpha = state.register_node("alpha");

    let resp = client.get(format!("{base}/sse")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();

    // Drain the initial frame for alpha first, so the frame asserted below
    // is unambiguously the LATE registration's.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !buf.contains("id=\"panel-alpha\"") {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "no initial frame; got:\n{buf}");
        let chunk = tokio::time::timeout(remaining, stream.next())
            .await
            .expect("timed out waiting for the initial SSE frame")
            .expect("SSE stream ended early")
            .expect("SSE stream error");
        buf.push_str(&String::from_utf8_lossy(&chunk));
    }

    // Born after connect — the driver's eager node_gate registration path.
    let _late = state.register_node("alpha/1-late");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !buf.contains("id=\"panel-alpha/1-late\"") {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "no frame for the late-registered node; got:\n{buf}"
        );
        let chunk = tokio::time::timeout(remaining, stream.next())
            .await
            .expect("timed out waiting for the late node's SSE frame")
            .expect("SSE stream ended early")
            .expect("SSE stream error");
        buf.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(
        buf.contains("data-path=\"alpha/1-late\""),
        "the frame carries the mount path: {buf}"
    );
}
