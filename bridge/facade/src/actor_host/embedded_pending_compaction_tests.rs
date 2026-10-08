//! Engine component tests use a gated tool endpoint to isolate scheduler and
//! compaction behavior. Hosted lifetime coverage uses production assembly.

use super::embedded_service::{attach_actor, drive_conversation_with_transport};
use super::test_campaign::TestCampaign;
use async_trait::async_trait;
use exomonad_actor::{ResidentToolDispatchFuture, ResidentToolEndpoint, ResidentToolError};
use exomonad_tool::{
    CustomToolDeclaration, HostedTool, ToolArguments, ToolDeclaration, ToolInvocation, ToolKind,
};
use harness::{
    engine::ResponsesTransport,
    model::{AgentPath, CallId, Effort},
    store::ClaimState,
    transport::{Auth, ResponsesRequest, ResponsesTurn, TransportError},
};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};

struct Offline;

impl Auth for Offline {
    fn access(&self) -> Result<(String, String), TransportError> {
        panic!("the scripted transport must not request credentials")
    }
}

#[derive(Clone)]
struct InterruptibleRoundTransport {
    requests: Arc<AtomicU64>,
    first_started: Arc<tokio::sync::Notify>,
    resumed: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ResponsesTransport for InterruptibleRoundTransport {
    async fn create(&self, _request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        match self.requests.fetch_add(1, Ordering::SeqCst) + 1 {
            1 => {
                self.first_started.notify_one();
                std::future::pending::<Result<ResponsesTurn, TransportError>>().await
            }
            2 => {
                self.resumed.notify_one();
                Ok(ResponsesTurn {
                    response_id: "after-browser-interrupt".into(),
                    items: vec![harness::item::Item(json!({
                        "type":"message",
                        "role":"assistant",
                        "phase":"final_answer",
                        "content":[{"type":"output_text","text":"continued after interrupt"}]
                    }))],
                    usage: Default::default(),
                })
            }
            other => panic!("unexpected Engine request after interrupt: {other}"),
        }
    }
}

struct GatedEndpoint {
    tools: Vec<HostedTool>,
    releases: Arc<Mutex<HashMap<String, oneshot::Receiver<()>>>>,
    started: mpsc::UnboundedSender<String>,
    settled: mpsc::UnboundedSender<String>,
}

impl ResidentToolEndpoint for GatedEndpoint {
    fn snapshot_for_request(&self) -> Result<Arc<dyn ResidentToolEndpoint>, ResidentToolError> {
        Ok(Arc::new(Self {
            tools: self.tools.clone(),
            releases: self.releases.clone(),
            started: self.started.clone(),
            settled: self.settled.clone(),
        }))
    }

    fn tools(&self) -> &[HostedTool] {
        &self.tools
    }

    fn instructions(&self) -> Option<&str> {
        None
    }

    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolDispatchFuture {
        let Some(context) = invocation.context else {
            return Box::pin(async {
                Err(ResidentToolError::Unavailable(
                    "test invocation has no exact call context".into(),
                ))
            });
        };
        let call_id = context.call_id;
        let release = self.releases.lock().unwrap().remove(&call_id);
        let Some(release) = release else {
            return Box::pin(async move {
                Err(ResidentToolError::Unavailable(format!(
                    "no held call registered for {call_id}"
                )))
            });
        };
        let started = self.started.clone();
        let settled = self.settled.clone();
        Box::pin(async move {
            started
                .send(call_id.clone())
                .map_err(|_| ResidentToolError::Unavailable("test observer closed".into()))?;
            release.await.map_err(|_| {
                ResidentToolError::Unavailable("test call release was dropped".into())
            })?;
            let result = match invocation.arguments {
                ToolArguments::Raw(_) => json!("raw late output"),
                ToolArguments::Structured(_) => json!({"result":"typed late output"}),
            };
            settled
                .send(call_id)
                .map_err(|_| ResidentToolError::Unavailable("test observer closed".into()))?;
            Ok(exomonad_actor::ResidentToolResponse::Value(result))
        })
    }
}

#[derive(Clone)]
struct CompactionTransport {
    model_requests: Arc<Mutex<Vec<ResponsesRequest>>>,
    successor_seen: mpsc::UnboundedSender<()>,
    late_output_seen: mpsc::UnboundedSender<()>,
    compactions: Arc<AtomicU64>,
}

#[async_trait]
impl ResponsesTransport for CompactionTransport {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        if request.tools_allowed.as_ref().is_some_and(Vec::is_empty) {
            assert!(request.tools.is_empty());
            self.compactions.fetch_add(1, Ordering::Relaxed);
            for call_id in ["raw-pending-call", "typed-pending-call"] {
                assert!(
                    !request
                        .input
                        .iter()
                        .any(|item| item.0["call_id"] == call_id),
                    "compaction must not summarize an unanswered call: {:#?}",
                    request.input
                );
            }
            return Ok(message_turn(
                "plain-text-compaction",
                "Keep the raw and typed operations pending.",
            ));
        }

        let round = {
            let mut requests = self.model_requests.lock().unwrap();
            requests.push(request.clone());
            requests.len()
        };
        match round {
            1 => Ok(ResponsesTurn {
                response_id: "pending-calls".into(),
                items: vec![
                    harness::item::Item(json!({
                        "type":"custom_tool_call",
                        "call_id":"raw-pending-call",
                        "name":"raw_hold",
                        "input":"opaque raw input"
                    })),
                    harness::item::Item(json!({
                        "type":"function_call",
                        "call_id":"typed-pending-call",
                        "name":"typed_hold",
                        "arguments":"{\"value\":7}"
                    })),
                ],
                usage: harness::transport::Usage {
                    input_tokens: 100_001,
                    ..Default::default()
                },
            }),
            2 => {
                for call_id in ["raw-pending-call", "typed-pending-call"] {
                    assert!(
                        request
                            .input
                            .iter()
                            .any(|item| item.0["call_id"] == call_id),
                        "successor request must carry exact pending call {call_id}: {:#?}",
                        request.input
                    );
                }
                let _ = self.successor_seen.send(());
                Ok(message_turn("await-late-output", "Waiting for both tools."))
            }
            3 => {
                for (call_id, expected) in [
                    ("raw-pending-call", "raw late output"),
                    ("typed-pending-call", "typed late output"),
                ] {
                    assert!(
                        request.input.iter().any(|item| {
                            item.0["call_id"] == call_id && item.0.to_string().contains(expected)
                        }),
                        "successor request must contain late output for {call_id}: {:#?}",
                        request.input
                    );
                }
                let _ = self.late_output_seen.send(());
                Ok(message_turn("finished", "Both late outputs were retained."))
            }
            4 => Ok(ResponsesTurn {
                response_id: "pending-before-cancel".into(),
                items: vec![
                    harness::item::Item(json!({
                        "type":"custom_tool_call",
                        "call_id":"cancel-pending-call",
                        "name":"raw_hold",
                        "input":"hold until Engine cancellation"
                    })),
                    harness::item::Item(json!({
                        "type":"message",
                        "role":"assistant",
                        "phase":"final_answer",
                        "content":[{"type":"output_text","text":"cancel this pending call"}]
                    })),
                ],
                usage: Default::default(),
            }),
            other => panic!("unexpected model request {other}"),
        }
    }
}

fn message_turn(response_id: &str, text: &str) -> ResponsesTurn {
    ResponsesTurn {
        response_id: response_id.into(),
        items: vec![harness::item::Item(json!({
            "type":"message",
            "role":"assistant",
            "phase":"final_answer",
            "content":[{"type":"output_text","text":text}]
        }))],
        usage: Default::default(),
    }
}

#[tokio::test]
async fn engine_component_carries_raw_and_typed_pending_calls_through_compaction_and_late_output() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let (started_tx, mut started_rx) = mpsc::unbounded_channel();
                let (settled_tx, mut settled_rx) = mpsc::unbounded_channel();
                let (raw_release_tx, raw_release_rx) = oneshot::channel();
                let (typed_release_tx, typed_release_rx) = oneshot::channel();
                let (cancel_release_tx, cancel_release_rx) = oneshot::channel();
                let endpoint = GatedEndpoint {
                    tools: vec![
                        HostedTool::Custom(CustomToolDeclaration {
                            name: "raw_hold".into(),
                            description: "Hold a raw call until the test releases it.".into(),
                            schedule: exomonad_tool::ToolScheduling::default(),
                            implementation: exomonad_tool::ToolImplementation::default(),
                            effect_keys: Vec::new(),
                        }),
                        HostedTool::Function(ToolDeclaration {
                            name: "typed_hold".into(),
                            description: "Hold a typed call until the test releases it.".into(),
                            input_schema: json!({
                                "type":"object",
                                "properties":{"value":{"type":"integer"}},
                                "required":["value"],
                                "additionalProperties":false
                            }),
                            output_schema: None,
                            kind: ToolKind::Call,
                            schedule: exomonad_tool::ToolScheduling::default(),
                            implementation: exomonad_tool::ToolImplementation::default(),
                            effect_keys: Vec::new(),
                        }),
                    ],
                    releases: Arc::new(Mutex::new(HashMap::from([
                        ("raw-pending-call".into(), raw_release_rx),
                        ("typed-pending-call".into(), typed_release_rx),
                        ("cancel-pending-call".into(), cancel_release_rx),
                    ]))),
                    started: started_tx,
                    settled: settled_tx,
                };
                let actor = campaign.actor.identity();
                let mut installation = campaign.root_installation.clone();
                installation.policy = Arc::new(endpoint);

                let files = tempfile::tempdir().unwrap();
                let assets = files.path().join("assets");
                std::fs::create_dir_all(&assets).unwrap();
                std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
                let session_secret_file = files.path().join("session-secret");
                std::fs::write(
                    &session_secret_file,
                    "embedded-compaction-secret-is-long-enough",
                )
                .unwrap();
                let credential_file = files.path().join("codex-auth.json");
                std::fs::write(&credential_file, "{}").unwrap();
                let settings = crate::exomonad::EmbeddedLaunchConfig {
                    listen: "127.0.0.1:0".parse().unwrap(),
                    public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
                    public_origin: None,
                    asset_root: assets,
                    browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
                    session_secret_file: Some(session_secret_file),
                    provider: crate::exomonad::EmbeddedModelProvider::Codex,
                    credential_file,
                    context_capacity_tokens: 200_000,
                    concurrent_jobs: 2,
                };
                let mut service = campaign
                    .prepare_engine_component_service(&settings)
                    .await
                    .unwrap();
                let embedded = attach_actor(
                    &service,
                    campaign.session_root.path(),
                    AgentPath("/root".into()),
                    None,
                    installation,
                    Some("exercise pending-call compaction".into()),
                )
                .await
                .unwrap();
                let (lifecycle, _lifecycle_rx) =
                    watch::channel((Some(actor), harness::server::HostActorLifecycle::Waiting));
                let (successor_tx, mut successor_rx) = mpsc::unbounded_channel();
                let (late_output_tx, mut late_output_rx) = mpsc::unbounded_channel();
                let transport = CompactionTransport {
                    model_requests: Arc::new(Mutex::new(Vec::new())),
                    successor_seen: successor_tx,
                    late_output_seen: late_output_tx,
                    compactions: Arc::new(AtomicU64::new(0)),
                };
                let settings_for_engine = settings.clone();
                let runtime = Arc::clone(&service.runtime);
                let engine_transport = transport.clone();
                let conversation = Arc::clone(&embedded.conversation);
                let stop_driver = embedded.cancellation.clone();
                let mut running = tokio::spawn(async move {
                    drive_conversation_with_transport::<Offline, _>(
                        embedded.driver,
                        runtime,
                        &settings_for_engine,
                        "offline-compaction".into(),
                        Effort::Medium,
                        "production pending-call test".into(),
                        embedded.cancellation_rx,
                        lifecycle,
                        actor,
                        engine_transport,
                    )
                    .await
                });

                let first = tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
                    .await
                    .expect("raw call did not start")
                    .unwrap();
                let second = tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
                    .await
                    .expect("typed call did not start")
                    .unwrap();
                assert_eq!(
                    [first.as_str(), second.as_str()]
                        .into_iter()
                        .collect::<std::collections::HashSet<_>>(),
                    ["raw-pending-call", "typed-pending-call"]
                        .into_iter()
                        .collect()
                );
                tokio::time::timeout(Duration::from_secs(10), successor_rx.recv())
                    .await
                    .expect("compacted successor request did not include pending calls")
                    .expect("transport dropped successor signal");
                raw_release_tx.send(()).unwrap();
                typed_release_tx.send(()).unwrap();
                for _ in 0..2 {
                    tokio::time::timeout(Duration::from_secs(5), settled_rx.recv())
                        .await
                        .expect("released operations did not settle")
                        .expect("test endpoint dropped settlement observer");
                }
                conversation
                    .input(
                        "release-follow-up",
                        "operator",
                        "Continue after both held operations settle.",
                    )
                    .await
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(5), late_output_rx.recv())
                    .await
                    .expect("Engine did not make a request after both late outputs settled")
                    .expect("transport dropped late-output request signal");
                conversation
                    .input(
                        "begin-pending-cancel",
                        "operator",
                        "Start a held operation so the Engine can cancel it.",
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
                        .await
                        .expect("cancellable operation did not start")
                        .expect("test endpoint dropped start observer"),
                    "cancel-pending-call"
                );
                stop_driver.send_replace(true);
                tokio::time::timeout(Duration::from_secs(10), &mut running)
                    .await
                    .expect("pending Engine cancellation did not finish")
                    .unwrap()
                    .expect("cancelling a pending Engine must confirm cleanup");
                cancel_release_tx.send(()).unwrap();
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(5), settled_rx.recv())
                        .await
                        .expect("cancelled test future did not exit after release")
                        .expect("test endpoint dropped settlement observer"),
                    "cancel-pending-call"
                );
                let requests = transport.model_requests.lock().unwrap();
                assert_eq!(requests.len(), 4);
                for (call_id, output) in [
                    ("raw-pending-call", "raw late output"),
                    ("typed-pending-call", "typed late output"),
                ] {
                    assert!(requests[2].input.iter().any(|item| {
                        item.0["call_id"] == call_id && item.0.to_string().contains(output)
                    }));
                }
                drop(requests);
                assert_eq!(transport.compactions.load(Ordering::Relaxed), 1);
                let cancellation_claim = service
                    .runtime
                    .store()
                    .claims(&CallId("cancel-pending-call".into()))
                    .unwrap();
                assert_eq!(cancellation_claim.len(), 1);
                assert_eq!(cancellation_claim[0].state, ClaimState::Settled);
                service.shutdown().await.unwrap();
            })
        })
        .await;
}

#[derive(Clone)]
struct FailedCompactionTransport {
    normal_requests: Arc<AtomicU64>,
    compaction_seen: mpsc::UnboundedSender<()>,
    resumed_after_failure: mpsc::UnboundedSender<()>,
}

#[async_trait]
impl ResponsesTransport for FailedCompactionTransport {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        if request.tools_allowed.as_ref().is_some_and(Vec::is_empty) {
            assert!(request.tools.is_empty());
            let _ = self.compaction_seen.send(());
            return Ok(message_turn("empty-summary", "   "));
        }

        let round = self.normal_requests.fetch_add(1, Ordering::Relaxed);
        match round {
            0 => Ok(ResponsesTurn {
                response_id: "pending-before-failed-compaction".into(),
                items: vec![harness::item::Item(json!({
                    "type":"custom_tool_call",
                    "call_id":"cleanup-after-failed-compaction",
                    "name":"raw_hold",
                    "input":"hold until the test cancels the Engine"
                }))],
                usage: harness::transport::Usage {
                    input_tokens: 100_001,
                    ..Default::default()
                },
            }),
            1 => {
                let _ = self.resumed_after_failure.send(());
                Ok(message_turn(
                    "continued-after-failed-compaction",
                    "Continue.",
                ))
            }
            other => panic!("unexpected unbounded normal request {other}"),
        }
    }
}

#[tokio::test]
async fn engine_component_compaction_failure_continues_once_then_cleans_pending_call_on_cancel() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let (started_tx, mut started_rx) = mpsc::unbounded_channel();
                let (settled_tx, mut settled_rx) = mpsc::unbounded_channel();
                let (release_tx, release_rx) = oneshot::channel();
                let endpoint = GatedEndpoint {
                    tools: vec![HostedTool::Custom(CustomToolDeclaration {
                        name: "raw_hold".into(),
                        description: "Hold a raw call until the test releases it.".into(),
                        schedule: exomonad_tool::ToolScheduling::default(),
                        implementation: exomonad_tool::ToolImplementation::default(),
                        effect_keys: Vec::new(),
                    })],
                    releases: Arc::new(Mutex::new(HashMap::from([(
                        "cleanup-after-failed-compaction".into(),
                        release_rx,
                    )]))),
                    started: started_tx,
                    settled: settled_tx,
                };
                let actor = campaign.actor.identity();
                let mut installation = campaign.root_installation.clone();
                installation.policy = Arc::new(endpoint);

                let files = tempfile::tempdir().unwrap();
                let assets = files.path().join("assets");
                std::fs::create_dir_all(&assets).unwrap();
                std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
                let session_secret_file = files.path().join("session-secret");
                std::fs::write(
                    &session_secret_file,
                    "embedded-compaction-secret-is-long-enough",
                )
                .unwrap();
                let credential_file = files.path().join("codex-auth.json");
                std::fs::write(&credential_file, "{}").unwrap();
                let settings = crate::exomonad::EmbeddedLaunchConfig {
                    listen: "127.0.0.1:0".parse().unwrap(),
                    public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
                    public_origin: None,
                    asset_root: assets,
                    browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
                    session_secret_file: Some(session_secret_file),
                    provider: crate::exomonad::EmbeddedModelProvider::Codex,
                    credential_file,
                    context_capacity_tokens: 200_000,
                    concurrent_jobs: 2,
                };
                let mut service = campaign
                    .prepare_engine_component_service(&settings)
                    .await
                    .unwrap();
                let embedded = attach_actor(
                    &service,
                    campaign.session_root.path(),
                    AgentPath("/root".into()),
                    None,
                    installation,
                    Some("exercise failed compaction cleanup".into()),
                )
                .await
                .unwrap();
                let (lifecycle, _lifecycle_rx) =
                    watch::channel((Some(actor), harness::server::HostActorLifecycle::Waiting));
                let (compaction_tx, mut compaction_rx) = mpsc::unbounded_channel();
                let (resumed_tx, mut resumed_rx) = mpsc::unbounded_channel();
                let transport = FailedCompactionTransport {
                    normal_requests: Arc::new(AtomicU64::new(0)),
                    compaction_seen: compaction_tx,
                    resumed_after_failure: resumed_tx,
                };
                let runtime = Arc::clone(&service.runtime);
                let engine_transport = transport.clone();
                let stop_driver = embedded.cancellation.clone();
                let mut running = tokio::spawn(async move {
                    drive_conversation_with_transport::<Offline, _>(
                        embedded.driver,
                        runtime,
                        &settings,
                        "offline-compaction-failure".into(),
                        Effort::Medium,
                        "production compaction failure test".into(),
                        embedded.cancellation_rx,
                        lifecycle,
                        actor,
                        engine_transport,
                    )
                    .await
                });

                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(10), started_rx.recv())
                        .await
                        .expect("pending operation did not start")
                        .unwrap(),
                    "cleanup-after-failed-compaction"
                );
                tokio::time::timeout(Duration::from_secs(10), compaction_rx.recv())
                    .await
                    .expect("failed compaction was not attempted")
                    .expect("transport dropped compaction signal");
                tokio::time::timeout(Duration::from_secs(10), resumed_rx.recv())
                    .await
                    .expect("Engine did not continue on the original window")
                    .expect("transport dropped post-failure signal");
                assert_eq!(transport.normal_requests.load(Ordering::Relaxed), 2);
                stop_driver.send_replace(true);
                tokio::time::timeout(Duration::from_secs(10), &mut running)
                    .await
                    .expect("Engine cancellation did not clean the pending call")
                    .unwrap()
                    .expect("cancelling the pending Engine must confirm cleanup");
                release_tx.send(()).unwrap();
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(5), settled_rx.recv())
                        .await
                        .expect("cancelled test future did not exit after release")
                        .expect("test endpoint dropped settlement observer"),
                    "cleanup-after-failed-compaction"
                );
                let attempts = service
                    .runtime
                    .store()
                    .events(None)
                    .unwrap()
                    .into_iter()
                    .filter(|event| event.kind == "compaction_attempt")
                    .collect::<Vec<_>>();
                assert_eq!(attempts.len(), 1);
                let attempt: serde_json::Value =
                    serde_json::from_str(&attempts[0].payload).unwrap();
                assert_eq!(attempt["outcome"], "failed");
                let claim = service
                    .runtime
                    .store()
                    .claims(&CallId("cleanup-after-failed-compaction".into()))
                    .unwrap();
                assert_eq!(claim.len(), 1);
                assert_eq!(claim[0].state, ClaimState::Settled);
                service.shutdown().await.unwrap();
            })
        })
        .await;
}

#[tokio::test]
async fn attached_round_interrupt_preserves_host_and_accepts_later_input() {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret = files.path().join("session-secret");
    std::fs::write(&secret, "interrupt-test-secret-is-long-enough").unwrap();
    let auth = files.path().join("codex-auth.json");
    std::fs::write(&auth, "{}").unwrap();
    let settings = crate::exomonad::EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        public_origin: None,
        asset_root: assets,
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(secret),
        provider: crate::exomonad::EmbeddedModelProvider::Codex,
        credential_file: auth,
        context_capacity_tokens: 200_000,
        concurrent_jobs: 1,
    };
    let transport = InterruptibleRoundTransport {
        requests: Arc::new(AtomicU64::new(0)),
        first_started: Arc::new(tokio::sync::Notify::new()),
        resumed: Arc::new(tokio::sync::Notify::new()),
    };
    let provider: Arc<dyn ResponsesTransport> = Arc::new(transport.clone());
    let host = super::hosted_test_context::HostedTestRuntime::start(&settings, &provider)
        .await
        .expect("production interrupt host starts");
    host.input("Begin the provider round to interrupt.")
        .await
        .unwrap();
    let actor = host.context.actor.identity();
    let binding = host
        .context
        .binding(actor)
        .expect("actual root binding attached");
    let conversation = binding.conversation().unwrap();
    tokio::time::timeout(Duration::from_secs(30), transport.first_started.notified())
        .await
        .expect("Engine enters the first actual provider round");
    let inactive_round = harness::embedding::EmbeddedRoundId(uuid::Uuid::new_v4());
    assert!(conversation
        .control(harness::embedding::HostControl::Interrupt {
            expected_round: inactive_round,
        })
        .await
        .is_err());
    let first_round = conversation
        .active_round()
        .expect("running provider round identity");
    assert_eq!(
        conversation
            .control(harness::embedding::HostControl::Interrupt {
                expected_round: first_round,
            })
            .await
            .unwrap()["requested"],
        true
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        while conversation.active_round().is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("interrupted Engine returns its real attachment to idle");
    assert!(
        host.context.actor.terminal().get().is_none(),
        "round interruption preserves the actor"
    );
    let interrupted_head = host
        .runtime
        .store()
        .agent(&AgentPath("/root".into()))
        .unwrap()
        .unwrap()
        .head_request;
    assert!(
        interrupted_head.is_some(),
        "interrupted durable head was not retained"
    );
    assert!(conversation
        .control(harness::embedding::HostControl::Interrupt {
            expected_round: first_round,
        })
        .await
        .is_err());

    conversation
        .input("after-interrupt", "browser", "continue after the interrupt")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), transport.resumed.notified())
        .await
        .expect("later input did not start a fresh Engine round");
    tokio::time::timeout(Duration::from_secs(30), async {
        while conversation.active_round().is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("resumed actual provider round completes");
    let resumed_head = host
        .runtime
        .store()
        .agent(&AgentPath("/root".into()))
        .unwrap()
        .unwrap()
        .head_request;
    assert_ne!(resumed_head, interrupted_head);

    assert_eq!(transport.requests.load(Ordering::SeqCst), 2);
    host.stop()
        .await
        .expect("production interrupt host acknowledges cleanup");
}
