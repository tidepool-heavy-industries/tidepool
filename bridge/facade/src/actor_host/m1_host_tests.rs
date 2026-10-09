use super::*;
use crate::exomonad::EmbeddedLaunchConfig;
use async_trait::async_trait;
use futures_util::StreamExt;
use harness::{
    engine::ResponsesTransport,
    model::AgentPath,
    transport::{ResponsesRequest, ResponsesTurn, TransportError, Usage},
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::hosted_test_context::{cell_output_matches, HostedTestRuntime};

#[path = "m1_browser_process.rs"]
mod browser_process;

#[path = "m1_browser_runner.rs"]
mod browser_runner;

#[path = "m1_warm_cell_performance.rs"]
mod warm_cell_performance;

#[path = "m1_cancel_performance.rs"]
mod cancel_performance;
#[path = "m1_eight_actor_performance.rs"]
mod eight_actor_performance;

#[path = "m1_real_host_late_output_tests.rs"]
mod real_host_late_output_tests;

#[path = "m1_request_reload_tests.rs"]
mod request_reload_tests;

#[path = "m1_chat_projection_tests.rs"]
mod chat_projection_tests;

#[tokio::test]
#[ignore = "requires declared matched web, Node, Playwright and resident compiler inputs"]
async fn production_browser_executes_resident_haskell_retries_and_controls_root() {
    browser_runner::production_browser_journey().await;
}

fn browser_target(
    campaign: &super::hosted_test_context::HostedActorContext,
) -> harness::embedding::HostIdentity {
    harness::embedding::HostIdentity {
        run: runtime_namespace(&campaign.config.run_directory.path()),
        actor: AgentPath("/root".into()),
        incarnation: campaign.actor.identity().incarnation.0.to_string(),
    }
}

fn browser_input(
    target: &harness::embedding::HostIdentity,
    text: &str,
) -> harness::server::ClientCommand {
    harness::server::ClientCommand::Host {
        operation_id: harness::embedding::ClientOperationId(uuid::Uuid::new_v4()),
        command: harness::server::HostCommand::Input {
            target: target.clone(),
            text: text.to_owned(),
        },
    }
}

#[derive(Clone)]
struct HostCellTransport {
    requests: Arc<Mutex<Vec<ResponsesRequest>>>,
    second_request: mpsc::UnboundedSender<ResponsesRequest>,
    calls: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct CancelRunningCellTransport {
    requests: Arc<AtomicUsize>,
    successor: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ResponsesTransport for CancelRunningCellTransport {
    async fn create(&self, _request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        let round = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        let items = match round {
            1 => vec![harness::item::Item(json!({
                "type":"custom_tool_call",
                "call_id":"host-cancel-cell",
                "name":"haskell",
                "input":"do { sleep (seconds 30); pure (99 :: Int) }"
            }))],
            2 => {
                self.successor.notify_one();
                vec![harness::item::Item(json!({
                    "type":"message",
                    "role":"assistant",
                    "phase":"final_answer",
                    "content":[{"type":"output_text","text":"waiting for cell"}]
                }))]
            }
            other => panic!("unexpected model request after cancellation scenario: {other}"),
        };
        Ok(ResponsesTurn {
            response_id: format!("cancel-host-{round}"),
            items,
            usage: Usage::default(),
        })
    }
}

#[async_trait]
impl ResponsesTransport for HostCellTransport {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        let round = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.requests.lock().push(request.clone());
        match round {
            1 => Ok(ResponsesTurn {
                response_id: "host-cell".into(),
                items: vec![harness::item::Item(json!({
                    "type":"custom_tool_call",
                    "call_id":"host-real-cell",
                    "name":"haskell",
                    "input":"40 + 2 :: Int"
                }))],
                usage: Usage::default(),
            }),
            2..=4 => {
                let completed = request.input.iter().any(|item| {
                    item.0["type"] == "custom_tool_call_output"
                        && item.0["call_id"] == "host-real-cell"
                });
                self.second_request.send(request).unwrap();
                Ok(ResponsesTurn {
                    response_id: format!("host-cell-observation-{round}"),
                    items: vec![harness::item::Item(json!({
                        "type":"message",
                        "role":"assistant",
                        "phase":"final_answer",
                        "content":[{"type":"output_text","text":if completed {
                            "The resident cell settled."
                        } else {
                            "Waiting for the resident cell."
                        }}]
                    }))],
                    usage: Usage::default(),
                })
            }
            other => panic!("unexpected post-completion model request {other}"),
        }
    }
}

async fn browser_socket(
    address: std::net::SocketAddr,
    cookie: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let origin = format!("https://{address}");
    let mut request = format!("ws://{address}/api/ws")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Origin", origin.parse().unwrap());
    request
        .headers_mut()
        .insert("Cookie", cookie.parse().unwrap());
    tokio_tungstenite::connect_async(request).await.unwrap().0
}

pub(super) async fn next_browser_event(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    kind: &str,
) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let frame = socket.next().await.unwrap().unwrap();
            let event: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            if event["type"] == "event" && event["event"]["event"]["kind"] == kind {
                return event;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for browser event {kind}"))
}

pub(super) async fn browser_snapshot(
    address: std::net::SocketAddr,
    cookie: &str,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Value,
) {
    let mut socket = browser_socket(address, cookie).await;
    let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .expect("browser did not receive its initial snapshot")
        .unwrap()
        .unwrap();
    let snapshot: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
    (socket, snapshot)
}

async fn browser_snapshot_until(
    address: std::net::SocketAddr,
    cookie: &str,
    lifecycle: &str,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Value,
) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (mut socket, snapshot) = browser_snapshot(address, cookie).await;
            if snapshot["snapshot"]["actors"][0]["lifecycle"] == lifecycle {
                return (socket, snapshot);
            }
            socket.close(None).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("host did not publish {lifecycle} snapshot"))
}

#[tokio::test]
async fn production_host_retains_http_haskell_commands_and_reconnects_without_replay() {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret_file = files.path().join("browser-secret");
    let secret = "offline-host-cell-test-secret-is-long-enough";
    std::fs::write(&secret_file, secret).unwrap();
    let auth_file = files.path().join("codex-auth.json");
    std::fs::write(&auth_file, "{}").unwrap();
    let settings = EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        public_origin: None,
        asset_root: assets,
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(secret_file),
        provider: crate::exomonad::EmbeddedModelProvider::Codex,
        credential_file: auth_file,
        context_capacity_tokens: 2_000_000,
        concurrent_jobs: 1,
    };
    let (second_tx, mut second_rx) = mpsc::unbounded_channel();
    let transport = Arc::new(HostCellTransport {
        requests: Arc::new(Mutex::new(Vec::new())),
        second_request: second_tx,
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let host_transport: Arc<dyn ResponsesTransport> = transport.clone();
    let fixture = HostedTestRuntime::start(&settings, &host_transport)
        .await
        .unwrap();
    let campaign = &fixture.context;
    let target = browser_target(campaign);
    let embedded_runtime = Arc::clone(&fixture.runtime);
    let address = fixture.address;
    let origin = format!("https://{address}");
    let api = format!("http://{address}/api");
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{api}/session"))
        .header("Origin", &origin)
        .json(&json!({"secret": secret}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookie = login.headers()[reqwest::header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let (mut socket, ready_snapshot) = browser_snapshot(address, &cookie).await;
    assert_eq!(
        ready_snapshot["snapshot"]["actors"][0]["lifecycle"],
        "waiting"
    );

    let command = "run one Haskell cell and retain its result";
    let submission = browser_input(&target, command);
    let accepted = client
        .post(format!("{api}/commands"))
        .header("Origin", &origin)
        .header(reqwest::header::COOKIE, &cookie)
        .json(&submission)
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
    let accepted: Value = accepted.json().await.unwrap();
    let command_id = accepted["command_id"].as_str().unwrap().to_owned();
    let admitted = next_browser_event(&mut socket, "command.receipt").await;
    assert_eq!(admitted["event"]["event"]["value"]["commandId"], command_id);
    assert_eq!(admitted["event"]["event"]["value"]["outcome"], "admitted");
    let envelope_id = admitted["event"]["event"]["value"]["envelopeId"]
        .as_str()
        .unwrap()
        .parse::<i64>()
        .unwrap();

    // Whole-cell checking, item compilation, and display compilation are distinct
    // worker requests; this budget covers all three on a cold compiler.
    let request_after_cell = tokio::time::timeout(Duration::from_secs(300), async {
        while let Some(request) = second_rx.recv().await {
            if request.input.iter().any(|item| {
                item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == "host-real-cell"
            }) {
                return Some(request);
            }
        }
        None
    })
    .await;
    let diagnostic = if request_after_cell.is_err() {
        fixture.cell_settlement_diagnostic("host-real-cell").await
    } else {
        String::new()
    };
    let request_after_cell = request_after_cell
        .unwrap_or_else(|_| {
            panic!(
                "host Engine did not request a successor turn after the Haskell cell; transport requests={}, {diagnostic}",
                transport.calls.load(Ordering::SeqCst),
            )
        })
        .expect("scripted transport dropped its successor request");
    let completed_provider_turns = transport.calls.load(Ordering::SeqCst);
    assert_eq!(
        request_after_cell
            .input
            .iter()
            .filter(|item| item.0["content"] == command)
            .count(),
        1,
        "browser command must be included exactly once"
    );
    assert!(
        request_after_cell
            .input
            .iter()
            .any(|item| cell_output_matches(item, "host-real-cell", "42")),
        "successor request must retain the real resident Haskell result: {:#?}",
        request_after_cell.input
    );

    let duplicate = client
        .post(format!("{api}/commands"))
        .header("Origin", &origin)
        .header(reqwest::header::COOKIE, &cookie)
        .json(&submission)
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), reqwest::StatusCode::ACCEPTED);
    let observation: Value = client
        .get(format!("{api}/commands/{command_id}"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(observation["envelopeId"], envelope_id);

    socket.close(None).await.unwrap();
    let (mut reconnected, idle_snapshot) =
        browser_snapshot_until(address, &cookie, "waiting").await;
    assert_eq!(
        idle_snapshot["snapshot"]["conversations"][0]["state"],
        "idle"
    );

    let store = embedded_runtime.store();
    let mut request = store
        .agent(&target.actor)
        .unwrap()
        .unwrap()
        .head_request
        .unwrap();
    let mut retained_items = Vec::new();
    for _ in 0..64 {
        let history = client
            .get(format!("{api}/history/{}", request.0))
            .header(reqwest::header::COOKIE, &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(history.status(), reqwest::StatusCode::OK);
        let history: Value = history.json().await.unwrap();
        retained_items.extend(
            history["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["item"].clone()),
        );
        let Some(parent) = history["parentId"].as_str() else {
            break;
        };
        request = harness::model::RequestId(parent.to_owned());
    }
    assert!(
        !retained_items.is_empty(),
        "retained request chain was empty"
    );
    assert_eq!(
        retained_items
            .iter()
            .filter(|item| item["content"] == command)
            .count(),
        1,
        "reconnected history must retain one copy of the browser command"
    );
    assert_eq!(
        retained_items
            .iter()
            .filter(|item| item["call_id"] == "host-real-cell" && item["type"] == "custom_tool_call")
            .count(),
        1,
        "reconnect/replay must not re-execute the Haskell call"
    );
    assert_eq!(
        transport.calls.load(Ordering::SeqCst),
        completed_provider_turns
    );
    assert!(
        retained_items.iter().any(|item| {
            cell_output_matches(&harness::item::Item(item.clone()), "host-real-cell", "42")
        }),
        "browser history did not retain the real Haskell output"
    );
    reconnected.close(None).await.unwrap();

    let retire = client
        .post(format!("{api}/commands"))
        .header("Origin", &origin)
        .header(reqwest::header::COOKIE, &cookie)
        .json(&harness::server::ClientCommand::Host {
            operation_id: harness::embedding::ClientOperationId(uuid::Uuid::new_v4()),
            command: harness::server::HostCommand::Retire {
                target: target.clone(),
            },
        })
        .send()
        .await
        .unwrap();
    assert_eq!(retire.status(), reqwest::StatusCode::ACCEPTED);
    let (mut retired_socket, retired_snapshot) =
        browser_snapshot_until(address, &cookie, "retired").await;
    assert_eq!(
        retired_snapshot["snapshot"]["actors"][0]["lifecycle"],
        "retired"
    );
    assert_eq!(
        retired_snapshot["snapshot"]["conversations"][0]["state"],
        "cancelled"
    );
    let rejected = client
        .post(format!("{api}/commands"))
        .header("Origin", &origin)
        .header(reqwest::header::COOKIE, &cookie)
        .json(&browser_input(&target, "must not reach a retired actor"))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::ACCEPTED);
    let rejection = next_browser_event(&mut retired_socket, "command.receipt").await;
    assert_eq!(rejection["event"]["event"]["value"]["outcome"], "refused");
    retired_socket.close(None).await.unwrap();

    fixture.stop().await.unwrap();
}

#[tokio::test]
async fn host_cancellation_stops_a_real_running_haskell_cell() {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret_file = files.path().join("browser-secret");
    std::fs::write(
        &secret_file,
        "offline-cancellation-test-secret-is-long-enough",
    )
    .unwrap();
    let auth_file = files.path().join("codex-auth.json");
    std::fs::write(&auth_file, "{}").unwrap();
    let settings = EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        public_origin: None,
        asset_root: assets,
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(secret_file),
        provider: crate::exomonad::EmbeddedModelProvider::Codex,
        credential_file: auth_file,
        context_capacity_tokens: 2_000_000,
        concurrent_jobs: 1,
    };
    let transport = CancelRunningCellTransport {
        requests: Arc::new(AtomicUsize::new(0)),
        successor: Arc::new(tokio::sync::Notify::new()),
    };
    let provider: Arc<dyn ResponsesTransport> = Arc::new(transport.clone());
    let host = HostedTestRuntime::start(&settings, &provider)
        .await
        .unwrap();
    host.input("Run the resident Sleep cell before cancellation.")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(300), async {
        transport.successor.notified().await;
        loop {
            let claims = host
                .runtime
                .store()
                .claims(&harness::model::CallId("host-cancel-cell".into()))
                .unwrap();
            if let [claim] = claims.as_slice() {
                assert_eq!(claim.state, harness::store::ClaimState::Pending);
                let context = exomonad_tool::ToolInvocationContext {
                    origin: exomonad_tool::ToolInvocationOrigin::Model(
                        embedded_harness::original_operation(
                            &browser_target(&host.context),
                            &claim.operation,
                        )
                        .unwrap(),
                    ),
                    call_id: claim.operation.call.0.clone(),
                    namespace: None,
                };
                if host
                    .context
                    .actor
                    .hosted_workbench_waiting(&context)
                    .is_some()
                {
                    assert!(host
                        .runtime
                        .scheduler()
                        .output(&claim.operation)
                        .await
                        .unwrap()
                        .is_none());
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the admitted resident Haskell cell must reach its actual Sleep suspension");

    let started = tokio::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(8), host.stop())
        .await
        .expect("host cancellation failed to stop the 30-second resident Haskell cell")
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(transport.requests.load(Ordering::SeqCst), 2);
}

struct RejectFirstRequest {
    authentication: bool,
    tool_before_rejection: bool,
    requests: Mutex<Vec<ResponsesRequest>>,
    successor_stalled: tokio::sync::Notify,
    reject_successor: tokio::sync::Notify,
}

#[async_trait]
impl ResponsesTransport for RejectFirstRequest {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        let round = {
            let mut requests = self.requests.lock();
            requests.push(request);
            requests.len()
        };
        if self.tool_before_rejection && round == 1 {
            return Ok(ResponsesTurn {
                response_id: "before-rejection-tool".into(),
                items: vec![harness::item::Item(json!({
                    "type":"custom_tool_call", "call_id":"before-rejection-cell",
                    "name":"haskell", "input":"40 + 2 :: Int"
                }))],
                usage: Usage::default(),
            });
        }
        if round == if self.tool_before_rejection { 2 } else { 1 } {
            if self.tool_before_rejection {
                self.successor_stalled.notify_one();
                self.reject_successor.notified().await;
            }
            return Err(if self.authentication {
                TransportError::Authentication
            } else {
                TransportError::Http {
                    status: 400,
                    diagnostic: Some(harness::transport::HttpDiagnostic {
                        code: Some("invalid_function_parameters".into()),
                        error_type: Some("invalid_request_error".into()),
                        param: Some("tools[0].parameters".into()),
                        message: Some("required must contain view".into()),
                    }),
                }
            });
        }
        Ok(ResponsesTurn {
            response_id: "explicit-successor".into(),
            items: vec![harness::item::Item(json!({
                "type":"message", "role":"assistant", "phase":"final_answer",
                "content":[{"type":"output_text","text":"explicit input recovered"}]
            }))],
            usage: Usage::default(),
        })
    }
}

#[tokio::test]
async fn production_host_preserves_rejected_request_and_waits_for_explicit_input() {
    for authentication in [false, true] {
        rejected_request_host_case(authentication, false).await;
    }
}

#[tokio::test]
async fn production_host_recovers_ancestor_tool_output_after_request_rejection() {
    for authentication in [false, true] {
        rejected_request_host_case(authentication, true).await;
    }
}

async fn rejected_request_host_case(authentication: bool, tool_before_rejection: bool) {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret = "offline-host-rejection-secret-is-long-enough";
    let secret_file = files.path().join("browser-secret");
    std::fs::write(&secret_file, secret).unwrap();
    let auth_file = files.path().join("unused-auth.json");
    std::fs::write(&auth_file, "{}").unwrap();
    let settings = EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        public_origin: None,
        asset_root: assets,
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(secret_file),
        provider: crate::exomonad::EmbeddedModelProvider::Codex,
        credential_file: auth_file,
        context_capacity_tokens: 200_000,
        concurrent_jobs: 1,
    };
    let transport = Arc::new(RejectFirstRequest {
        authentication,
        tool_before_rejection,
        requests: Mutex::new(Vec::new()),
        successor_stalled: tokio::sync::Notify::new(),
        reject_successor: tokio::sync::Notify::new(),
    });
    let host_transport: Arc<dyn ResponsesTransport> = transport.clone();
    let fixture = HostedTestRuntime::start(&settings, &host_transport)
        .await
        .unwrap();
    let target = browser_target(&fixture.context);
    let actor = fixture.context.actor.identity();
    let api = format!("http://{}/api", fixture.address);
    let origin = format!("https://{}", fixture.address);
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{api}/session"))
        .header("Origin", &origin)
        .json(&json!({"secret":secret}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookie = login.headers()[reqwest::header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let first = browser_input(&target, "retain rejected user input");
    let accepted = client
        .post(format!("{api}/commands"))
        .header("Origin", &origin)
        .header(reqwest::header::COOKIE, &cookie)
        .json(&first)
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
    let store = fixture.runtime.store();
    if tool_before_rejection {
        tokio::time::timeout(
            Duration::from_secs(30),
            transport.successor_stalled.notified(),
        )
        .await
        .expect("successor request did not start while tool was pending");
        assert!(!transport.requests.lock()[1].input.iter().any(|item| {
            item.0["type"] == "custom_tool_call_output"
                && item.0["call_id"] == "before-rejection-cell"
        }));
        // Match the existing real-cell fixture budget for its three compiler stages.
        tokio::time::timeout(Duration::from_secs(300), async {
            loop {
                let claims = store
                    .claims(&harness::model::CallId("before-rejection-cell".into()))
                    .unwrap();
                if claims.first().is_some_and(|claim| {
                    store
                        .replay_output_operation(&claim.operation)
                        .unwrap()
                        .is_some_and(|item| {
                            cell_output_matches(&item, "before-rejection-cell", "42")
                        })
                }) {
                    assert_eq!(claims.len(), 1);
                    assert!(!store
                        .items(&claims[0].request)
                        .unwrap()
                        .iter()
                        .any(|item| { item.0["type"] == "custom_tool_call_output" }));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("tool did not settle before provider rejection");
        transport.reject_successor.notify_one();
    }
    let failed_head = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let head = store.agent(&target.actor).unwrap().unwrap().head_request;
            if let Some(head) = head {
                if store
                    .events(Some(&head))
                    .unwrap()
                    .iter()
                    .any(|event| event.kind == "request_failed")
                {
                    break head;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("provider failure never advanced the exact durable head");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        transport.requests.lock().len(),
        if tool_before_rejection { 2 } else { 1 },
        "provider rejection retried without input"
    );
    assert!(fixture.context.actor.terminal().get().is_none());
    assert_eq!(fixture.context.actor.identity(), actor);
    let (mut socket, snapshot) = browser_snapshot_until(fixture.address, &cookie, "waiting").await;
    assert_eq!(snapshot["snapshot"]["conversations"][0]["state"], "idle");
    let failed = snapshot["snapshot"]["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|request| request["id"] == failed_head.0)
        .expect("failed request absent after reconnect");
    assert_eq!(failed["state"], "failed");
    assert_eq!(
        failed["failure"]["kind"],
        if authentication {
            "authentication"
        } else {
            "http"
        }
    );
    if !authentication {
        assert_eq!(
            failed["failure"]["diagnostic"]["code"],
            "invalid_function_parameters"
        );
        assert_eq!(
            failed["failure"]["diagnostic"]["message"],
            "required must contain view"
        );
    }
    // Replaying the same browser operation does not create another envelope or request.
    let duplicate = client
        .post(format!("{api}/commands"))
        .header("Origin", &origin)
        .header(reqwest::header::COOKIE, &cookie)
        .json(&first)
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), reqwest::StatusCode::ACCEPTED);
    let followup = browser_input(&target, "explicit corrected followup");
    let accepted = client
        .post(format!("{api}/commands"))
        .header("Origin", &origin)
        .header(reqwest::header::COOKIE, &cookie)
        .json(&followup)
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
    let successor = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let head = store.agent(&target.actor).unwrap().unwrap().head_request;
            if let Some(head) = head.filter(|head| head != &failed_head) {
                break head;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("explicit followup did not complete");
    assert_eq!(
        store.request(&successor).unwrap().unwrap().parent,
        Some(failed_head)
    );
    let requests = transport.requests.lock();
    let expected_requests = if tool_before_rejection { 3 } else { 2 };
    assert_eq!(requests.len(), expected_requests);
    let followup_request = &requests[expected_requests - 1];
    if tool_before_rejection {
        assert_eq!(
            followup_request
                .input
                .iter()
                .filter(|item| { cell_output_matches(item, "before-rejection-cell", "42") })
                .count(),
            1,
            "settled ancestor output was not recovered exactly once"
        );
        assert_eq!(
            followup_request
                .input
                .iter()
                .filter(|item| {
                    item.0["type"] == "custom_tool_call"
                        && item.0["call_id"] == "before-rejection-cell"
                })
                .count(),
            1
        );
        assert_eq!(
            store
                .claims(&harness::model::CallId("before-rejection-cell".into()))
                .unwrap()
                .len(),
            1
        );
    }
    for text in ["retain rejected user input", "explicit corrected followup"] {
        assert_eq!(
            followup_request
                .input
                .iter()
                .filter(|item| item.0["content"] == text)
                .count(),
            1
        );
    }
    drop(requests);
    assert!(fixture.context.actor.terminal().get().is_none());
    socket.close(None).await.unwrap();
    fixture.stop().await.unwrap();
}
