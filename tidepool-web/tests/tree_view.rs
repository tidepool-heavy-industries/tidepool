//! HTTP-level integration tests for the d3 tree view: `GET /` (the tree
//! shell + vendored d3 asset route), `GET /api/tree` (the JSON data source,
//! including parent links and status derivation), `GET /node/{node}/panel`
//! (the side pane's fragment source), and `GET /legacy` (the outline page,
//! still live at its new address). Assertions are scoped to the wire
//! contract (ids, JSON fields, the presence of the vendored asset) — never
//! visual markup, same discipline as `tests/operator_gate.rs`.

use std::net::SocketAddr;
use std::time::Duration;

use reqwest::Client;
use serde_json::{json, Value};
use tidepool_harness::selfharness::operator::OperatorGate;
use tidepool_web::{router, AppState};
use tokio::net::TcpListener;

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
async fn root_serves_the_tree_shell_with_the_vendored_d3_route_wired() {
    let (addr, _state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let resp = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let html = resp.text().await.unwrap();
    assert!(html.contains("id=\"tree-canvas\""), "{html}");
    assert!(html.contains("id=\"side-pane\""), "{html}");
    assert!(html.contains("id=\"side-pane-body\""), "{html}");

    // The exact src the shell baked in must itself be a live, same-origin
    // route serving real JS — never a CDN.
    let src_start = html.find("src=\"/assets/").expect("d3 script src present");
    let after = &html[src_start + 5..];
    let src = &after[..after.find('"').expect("closing quote")];
    assert!(src.starts_with("/assets/d3."), "{src}");

    let d3_resp = client.get(format!("{base}{src}")).send().await.unwrap();
    assert_eq!(d3_resp.status(), 200);
    let content_type = d3_resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(content_type.contains("javascript"), "{content_type}");
    let body = d3_resp.text().await.unwrap();
    assert!(body.contains("https://d3js.org"), "{body}");
    assert!(
        body.len() > 100_000,
        "expected the real d3 bundle, got {} bytes",
        body.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_still_serves_the_outline_page() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();
    let _gate = state.register_node("n1");

    let resp = client.get(format!("{base}/legacy")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let html = resp.text().await.unwrap();
    assert!(html.contains("id=\"tree\""), "{html}");
    assert!(html.contains("id=\"panel-n1\""), "{html}");
    // the outline page never references the d3 asset at all
    assert!(!html.contains("d3."), "{html}");
}

#[tokio::test(flavor = "multi_thread")]
async fn api_tree_reports_ids_parent_links_and_statuses() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let root_gate = state.register_node(tidepool_web::DEFAULT_NODE_ID);
    let _child = root_gate
        .node_gate("root/1-x")
        .expect("child gate registers");

    let resp = client.get(format!("{base}/api/tree")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let arr = body.as_array().expect("array response");
    assert_eq!(arr.len(), 2, "{arr:?}");

    let root = arr
        .iter()
        .find(|n| n["id"] == "root")
        .expect("root entry present");
    assert_eq!(root["path"], "root");
    assert_eq!(root["parent"], Value::Null, "{root:?}");
    assert_eq!(root["status"], "running", "{root:?}");
    assert!(root["rev"].is_u64(), "{root:?}");

    let child = arr
        .iter()
        .find(|n| n["id"] == "root/1-x")
        .expect("child entry present");
    assert_eq!(child["parent"], "root", "{child:?}");
    assert_eq!(child["status"], "running", "{child:?}");
}

/// A pending ask outranks everything in the derived status — the SAME rule
/// `render::status` applies for the `/legacy` outline must show up in the
/// `/api/tree` JSON too, since both derive from the one function.
#[tokio::test(flavor = "multi_thread")]
async fn api_tree_status_reflects_a_pending_ask() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = state.register_node("n1");
    let driver_gate = gate.clone();
    let handle = tokio::task::spawn_blocking(move || driver_gate.await_continue());

    // Wait until the ask is actually published before checking the endpoint.
    wait_for(&client, &format!("{base}/legacy"), |b| {
        b.contains("/node/n1/continue/")
    })
    .await;

    let body: Value = client
        .get(format!("{base}/api/tree"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let n1 = body
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "n1")
        .expect("n1 present");
    assert_eq!(n1["status"], "needs-you", "{n1:?}");

    // Resolve it so the spawned task doesn't leak past the test.
    let html = client
        .get(format!("{base}/legacy"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let needle = "@post('/node/n1/continue/";
    let start = html.find(needle).unwrap() + needle.len();
    let rest = &html[start..];
    let interaction = &rest[..rest.find('\'').unwrap()];
    client
        .post(format!("{base}/node/n1/continue/{interaction}"))
        .json(&json!({"answer": "Continue"}))
        .send()
        .await
        .unwrap();
    handle.await.unwrap();
}

/// A node with a no-op, non-node-shaped parent (no slash in the id) reports
/// `parent: null` — the top-level/forest case `d3.stratify`'s synthetic
/// super-root (client-side) exists to handle.
#[tokio::test(flavor = "multi_thread")]
async fn api_tree_top_level_node_has_null_parent() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();
    let _gate = state.register_node("standalone");

    let body: Value = client
        .get(format!("{base}/api/tree"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let n = body
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "standalone")
        .expect("present");
    assert_eq!(n["parent"], Value::Null, "{n:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn node_panel_route_serves_the_same_fragment_the_sse_stream_carries() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let gate = state.register_node("n1");
    gate.post_note("hello from the panel route");

    let resp = client
        .get(format!("{base}/node/n1/panel"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let html = resp.text().await.unwrap();
    assert!(html.starts_with("<div id=\"panel-n1\""), "{html}");
    assert!(html.contains("hello from the panel route"), "{html}");
}

/// A tree-path node id (containing a literal slash) is reachable through the
/// panel route only percent-encoded as a single path segment — same
/// discipline as `/node/{node}/submit/{interaction}` (axum matches `{node}`
/// as one segment; a raw slash 404s before any handler runs).
#[tokio::test(flavor = "multi_thread")]
async fn node_panel_route_percent_encodes_slash_path_ids() {
    let (addr, state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let root_gate = state.register_node(tidepool_web::DEFAULT_NODE_ID);
    let child_gate = root_gate.node_gate("root/1-x").expect("child gate");
    child_gate.post_note("child note");

    let resp = client
        .get(format!("{base}/node/root%2F1-x/panel"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let html = resp.text().await.unwrap();
    assert!(html.contains("child note"), "{html}");

    let raw_slash_resp = client
        .get(format!("{base}/node/root/1-x/panel"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        raw_slash_resp.status(),
        404,
        "a raw slash must not resolve to the child's panel"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn node_panel_route_404s_for_an_unregistered_node() {
    let (addr, _state) = boot().await;
    let base = format!("http://{addr}");
    let client = Client::new();

    let resp = client
        .get(format!("{base}/node/nope/panel"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["ok"], Value::Bool(false));
}
