use super::*;
use crate::exomonad::EmbeddedLaunchConfig;
use async_trait::async_trait;
use futures_util::StreamExt;
use harness::{
    engine::ResponsesTransport,
    model::AgentPath,
    transport::{Auth, ResponsesRequest, ResponsesTurn, TransportError, Usage},
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[path = "m1_host_fixture.rs"]
mod host_fixture;
use host_fixture::{RunningBrowserHost, cell_output_matches};

#[path = "m1_browser_process.rs"]
mod browser_process;

#[path = "m1_browser_runner.rs"]
mod browser_runner;

#[path = "m1_real_host_late_output_tests.rs"]
mod real_host_late_output_tests;

#[tokio::test]
#[ignore = "requires declared matched web, Node, Playwright and resident compiler inputs"]
async fn production_browser_executes_resident_haskell_retries_and_controls_root() {
    browser_runner::production_browser_journey().await;
}

fn browser_target(campaign: &test_campaign::TestCampaign) -> harness::embedding::HostIdentity {
    harness::embedding::HostIdentity {
        run: runtime_namespace(&campaign.config.run_root),
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

#[derive(Clone)]
struct OfflineHostAuth;

impl Auth for OfflineHostAuth {
    fn access(&self) -> Result<(String, String), TransportError> {
        panic!("deterministic host transport must not request credentials")
    }
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

async fn next_browser_event(
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

async fn browser_snapshot(
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
        asset_root: assets,
        session_secret_file: secret_file,
        codex_auth_file: auth_file,
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
    let fixture = RunningBrowserHost::start(&settings, &host_transport)
        .await
        .unwrap();
    let campaign = &fixture.campaign;
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

    let request_after_cell = tokio::time::timeout(Duration::from_secs(120), async {
        while let Some(request) = second_rx.recv().await {
            if request.input.iter().any(|item| {
                item.0["type"] == "custom_tool_call_output"
                    && item.0["call_id"] == "host-real-cell"
            }) {
                return Some(request);
            }
        }
        None
    })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "host Engine did not request a successor turn after the Haskell cell; transport requests={}",
                transport.calls.load(Ordering::SeqCst)
            )
        })
        .expect("scripted transport dropped its successor request");
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
    assert_eq!(transport.calls.load(Ordering::SeqCst), 3);
    assert!(
        retained_items.iter().any(|item| {
            item["type"] == "custom_tool_call_output"
                && item["call_id"] == "host-real-cell"
                && item.to_string().contains("42")
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
        asset_root: assets,
        session_secret_file: secret_file,
        codex_auth_file: auth_file,
        context_capacity_tokens: 2_000_000,
        concurrent_jobs: 1,
    };
    let campaign = test_campaign::TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.backend = crate::exomonad::HostBackendOptions::Embedded;
            config.embedded = Some(settings.clone());
        },
    )
    .await;
    std::fs::create_dir_all(&campaign.config.run_root).unwrap();
    let actor = campaign.actor.identity();
    let mut service =
        embedded_service::EmbeddedService::prepare(&campaign.config.run_root, &settings)
            .await
            .unwrap();
    let embedded = embedded_service::attach_actor(
        &service,
        &campaign.config.run_root,
        AgentPath("/root".into()),
        None,
        campaign.root_installation.clone(),
        Some("start a cell".into()),
    )
    .await
    .unwrap();
    let transport = CancelRunningCellTransport {
        requests: Arc::new(AtomicUsize::new(0)),
        successor: Arc::new(tokio::sync::Notify::new()),
    };
    let (lifecycle, _lifecycle_rx) =
        tokio::sync::watch::channel((Some(actor), harness::server::HostActorLifecycle::Waiting));
    let cancellation = embedded.cancellation.clone();
    let runtime = Arc::clone(&service.runtime);
    let settings_for_engine = settings.clone();
    let transport_for_engine = transport.clone();
    let mut running = tokio::spawn(async move {
        embedded_service::drive_conversation_with_transport::<OfflineHostAuth, _>(
            embedded.driver,
            runtime,
            &settings_for_engine,
            "offline".into(),
            harness::model::Effort::Medium,
            "resident test".into(),
            embedded.cancellation_rx,
            lifecycle,
            actor,
            transport_for_engine,
        )
        .await
    });
    tokio::select! {
        result = &mut running => panic!("Engine stopped before the long Haskell cell ran: {result:?}"),
        ready = tokio::time::timeout(Duration::from_secs(30), transport.successor.notified()) => {
            ready.expect("Engine did not advance while the resident Haskell cell was pending");
        }
    }

    let started = tokio::time::Instant::now();
    cancellation.send_replace(true);
    let result = tokio::time::timeout(Duration::from_secs(8), running)
        .await
        .expect("host cancellation failed to stop the 30-second resident Haskell cell")
        .unwrap();
    match result {
        Ok(()) => {}
        Err(error) if error == "engine cancelled" => {}
        Err(error) => {
            panic!("embedded Engine failed while cancelling its real Haskell call: {error}")
        }
    }
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(transport.requests.load(Ordering::SeqCst), 2);
    service.shutdown().await.unwrap();
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}

#[tokio::test]
async fn production_host_marks_embedded_root_ready_and_retires_invalid_auth_failure() {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret_file = files.path().join("browser-secret");
    std::fs::write(&secret_file, "offline-host-test-secret-32-bytes-long").unwrap();
    let auth_file = files.path().join("codex-auth.json");
    std::fs::write(&auth_file, "{}").unwrap();
    let settings = EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        asset_root: assets,
        session_secret_file: secret_file,
        codex_auth_file: auth_file,
        context_capacity_tokens: 200_000,
        concurrent_jobs: 1,
    };
    let mut campaign = test_campaign::TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.backend = crate::exomonad::HostBackendOptions::Embedded;
            config.embedded = Some(settings.clone());
        },
    )
    .await;
    std::fs::create_dir_all(&campaign.config.run_root).unwrap();
    let mut root_installation = campaign.root_installation.clone();
    root_installation.initial_user_message = Some("begin offline authentication check".into());
    let (lifecycle_tx, lifecycle_rx) = mpsc::channel(32);
    lifecycle_tx
        .send(LocalResidentDeployment::PolicyInstalled(Box::new(
            root_installation,
        )))
        .await
        .unwrap();
    let deployments = campaign.take_deployments();
    let forward = tokio::spawn(async move {
        let mut deployments = deployments;
        while let Some(deployment) = deployments.recv().await {
            if lifecycle_tx.send(deployment).await.is_err() {
                break;
            }
        }
    });

    let embedded_service =
        embedded_service::EmbeddedService::prepare(&campaign.config.run_root, &settings)
            .await
            .unwrap();
    let (readiness_tx, mut readiness_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = watch::channel(None);
    let (_config_tx, config_rx) = watch::channel(campaign.config.clone());
    let actor = campaign.actor.clone();
    let host_graph_forest = Arc::clone(&campaign.forest);
    let fleet = InteractiveFleet {
        provider_forest: Arc::clone(&campaign.forest),
        root: actor,
        config: campaign.config.clone(),
        run_root: campaign.config.run_root.clone(),
        #[cfg(feature = "codex-compat")]
        tmux: TmuxSession::new(&campaign.config.tmux_session).unwrap(),
        #[cfg(feature = "codex-compat")]
        backend: HostRuntimeMode::Embedded,
        worktrees: campaign.worktrees.clone(),
        #[cfg(feature = "codex-compat")]
        bindings: campaign.bindings.clone(),
        readiness: readiness_tx,
        worktree_authority: campaign.authority.clone(),
        #[cfg(feature = "codex-compat")]
        watch_retention: Arc::new(|_, _| false),
        #[cfg(feature = "codex-compat")]
        watch_observation: Arc::new(|_, _, _| false),
        #[cfg(feature = "codex-compat")]
        open_request: Arc::new(|_| None),
        #[cfg(feature = "codex-compat")]
        source_layers: None,
        #[cfg(feature = "codex-compat")]
        actor_recovery: exomonad_actor::ActorRecoveryJournal::open(
            campaign.config.run_root.join("actor-lifecycle.v2.jsonl"),
        )
        .unwrap(),
        #[cfg(feature = "codex-compat")]
        recovered_threads: Arc::new(BTreeMap::new()),
        #[cfg(feature = "codex-compat")]
        recovered_root_predecessor: None,
        host_graph: Arc::new(move || host_graph_forest.inspect_host_graph()),
    };
    let host = tokio::spawn(run_interactive_applications(
        lifecycle_rx,
        Arc::new(Mutex::new(HashMap::new())),
        fleet,
        shutdown_rx,
        config_rx,
        Some(embedded_service),
    ));

    let readiness = tokio::time::timeout(Duration::from_secs(30), readiness_rx.recv())
        .await
        .expect("production host did not publish embedded readiness")
        .expect("host dropped readiness before publishing it");
    let address = match readiness {
        ActorHostReadiness::EmbeddedReady { root, address } => {
            assert_eq!(root, campaign.actor.identity());
            address
        }
        other => panic!("production host reported unexpected readiness: {other:?}"),
    };
    let terminal = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(terminal) = campaign.actor.terminal().get() {
                break terminal;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("invalid credentials did not fail and retire the hosted root");
    assert_eq!(terminal.kind, ActorExitKind::Failed, "{terminal:?}");

    let api = format!("http://{address}/api");
    let origin = format!("https://{address}");
    let client = reqwest::Client::new();
    let secret = std::fs::read_to_string(settings.session_secret_file).unwrap();
    let login = client
        .post(format!("{api}/session"))
        .header("Origin", &origin)
        .json(&serde_json::json!({ "secret": secret }))
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
    let mut request = format!("ws://{address}/api/ws")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Origin", origin.parse().unwrap());
    request
        .headers_mut()
        .insert("Cookie", cookie.parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let snapshot: serde_json::Value =
        serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
    assert_eq!(snapshot["snapshot"]["actors"][0]["lifecycle"], "lost");
    assert_eq!(
        snapshot["snapshot"]["conversations"][0]["state"],
        "cancelled"
    );
    let response = client
        .post(format!("{api}/commands"))
        .header("Origin", &origin)
        .header(reqwest::header::COOKIE, &cookie)
        .json(&browser_input(
            &browser_target(&campaign),
            "must not wake a retired root",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    let rejection = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = socket.next().await.unwrap().unwrap();
            let event: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            if event["event"]["event"]["kind"] == "command.receipt" {
                break event;
            }
        }
    })
    .await
    .expect("host did not reject browser input after root retirement");
    assert_eq!(rejection["event"]["event"]["value"]["outcome"], "refused");
    socket.close(None).await.unwrap();

    shutdown_tx.send_replace(Some(NativeRetirement::Terminate));
    let host_result = tokio::time::timeout(Duration::from_secs(30), host)
        .await
        .expect("production host did not shut down")
        .expect("production host task panicked");
    assert!(host_result.is_ok(), "host cleanup failed: {host_result:?}");
    campaign.forest.shutdown().await;
    let _ = campaign.hosted.await;
    forward.abort();
    assert!(forward.await.unwrap_err().is_cancelled());
}
