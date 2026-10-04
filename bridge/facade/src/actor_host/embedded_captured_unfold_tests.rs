use super::test_campaign::COLD_DEBUG_CELL_SETTLEMENT_BUDGET;
use super::*;
use async_trait::async_trait;
use harness::{
    engine::ResponsesTransport,
    model::{AgentPath, CallId, ConversationIdentity, OperationId, RequestId},
    transport::{ResponsesRequest, ResponsesTurn, TransportError, Usage},
    turn::JobOutput,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

const PENDING_CALL: &str = "captured-unfold-and-await";
const REUSE_CALL: &str = "reuse-failed-cell-capture";
const RESUME_INPUT: &str = "resume interrupted captured fixture";

const TYPED_CHILD_REPLY_SETTLEMENT_BUDGET: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, PartialEq, Eq)]
enum CapturedScenario {
    Success,
    CancelWhileParked,
    FailureAfterReplies,
    ConcurrentNominalJoin,
}

#[derive(Clone, Copy)]
enum CallMode {
    Blocking,
    Asynchronous,
}

fn haskell_call(call_id: &str, source: &str, mode: CallMode) -> harness::item::Item {
    harness::item::Item(json!({
        "type": "custom_tool_call", "call_id": call_id, "name": "haskell", "input": source,
        "async": matches!(mode, CallMode::Asynchronous),
    }))
}

enum RootStep {
    Tool {
        call_id: String,
        source: String,
        mode: CallMode,
    },
    Finish,
}

struct ChildRounds {
    ordinal: usize,
    round: usize,
}

struct CapturedHostTransport {
    runtime: Arc<embedded_harness::EmbeddedHarnessRuntime>,
    setup_settlements: Mutex<Option<tokio::sync::broadcast::Receiver<OperationId>>>,
    parent_prefix: Mutex<Option<Vec<harness::item::Item>>>,
    provider_outputs: Mutex<HashMap<OperationId, Vec<harness::item::Item>>>,
    provider_changed: watch::Sender<u64>,
    scenario: CapturedScenario,
    root_origin: ConversationIdentity,
    root_round: AtomicUsize,
    operations: Mutex<HashMap<(ConversationIdentity, String), OperationId>>,
    scope_setup_issued: Notify,
    setup_requested: Notify,
    setup_ready: Notify,
    root_steps: tokio::sync::Mutex<mpsc::UnboundedReceiver<RootStep>>,
    root_steps_tx: mpsc::UnboundedSender<RootStep>,
    children: Mutex<HashMap<String, ChildRounds>>,
    parent_failed: watch::Sender<bool>,
    reply_children: watch::Sender<bool>,
    finish_children: watch::Sender<bool>,
    reads: mpsc::UnboundedSender<(ConversationIdentity, String)>,
    requests: mpsc::UnboundedSender<ResponsesRequest>,
}

impl CapturedHostTransport {
    fn operation(&self, origin: &ConversationIdentity, call_id: &str) -> OperationId {
        self.operations
            .lock()
            .get(&(origin.clone(), call_id.to_owned()))
            .unwrap_or_else(|| panic!("provider did not issue operation {call_id} for {origin:?}"))
            .clone()
    }

    async fn create_for_request(
        &self,
        request_id: &RequestId,
        request: ResponsesRequest,
    ) -> Result<ResponsesTurn, TransportError> {
        let (prefix, incarnation) = request.session_id.rsplit_once(':').unwrap();
        let (run, actor) = prefix.rsplit_once(':').unwrap();
        let origin = ConversationIdentity::Embedded {
            run: run.into(),
            actor: AgentPath(actor.into()),
            incarnation: incarnation.into(),
        };
        {
            let operations = self.operations.lock();
            let mut outputs = self.provider_outputs.lock();
            for ((owner, call), operation) in operations.iter() {
                if owner != &origin {
                    continue;
                }
                let returned = request
                    .input
                    .iter()
                    .filter(|item| {
                        item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == *call
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if !returned.is_empty() {
                    outputs.insert(operation.clone(), returned);
                }
            }
        }
        self.provider_changed.send_modify(|revision| *revision += 1);
        let (_, path) = request
            .session_id
            .rsplit_once(':')
            .unwrap()
            .0
            .rsplit_once(':')
            .unwrap();
        let (response_id, items) = if path == "/root" {
            let round = self.root_round.fetch_add(1, Ordering::SeqCst) + 1;
            let items = match round {
                1 => vec![harness::item::Item(json!({
                    "type":"custom_tool_call", "call_id":"captured-scope-setup", "name":"haskell",
                    "input":match self.scenario {
                        CapturedScenario::Success | CapturedScenario::CancelWhileParked => format!("{}\n{}\ndisplay True", include_str!("embedded_checkpoint_scope_setup.hs"), include_str!("embedded_captured_group_setup.hs")),
                        CapturedScenario::FailureAfterReplies => format!("{}\n{}\ndisplay True", include_str!("embedded_checkpoint_scope_setup.hs"), include_str!("embedded_captured_group_setup.hs")),
                        CapturedScenario::ConcurrentNominalJoin => format!("{}\n{}\n{}", include_str!("embedded_checkpoint_scope_setup.hs"), include_str!("embedded_captured_group_setup.hs"), include_str!("embedded_nominal_join_setup.hs")),
                    }
                }))],
                2 => {
                    *self.parent_prefix.lock() = Some(request.input.clone());
                    self.setup_requested.notify_one();
                    self.setup_ready.notified().await;
                    vec![haskell_call(
                        PENDING_CALL,
                        match self.scenario {
                            CapturedScenario::Success | CapturedScenario::CancelWhileParked => {
                                include_str!("embedded_captured_unfold_and_await.hs")
                            }
                            CapturedScenario::FailureAfterReplies => {
                                include_str!("embedded_captured_unfold_await_then_fail.hs")
                            }
                            CapturedScenario::ConcurrentNominalJoin => {
                                include_str!("embedded_nominal_join_a.hs")
                            }
                        },
                        if self.scenario == CapturedScenario::ConcurrentNominalJoin {
                            CallMode::Asynchronous
                        } else {
                            CallMode::Blocking
                        },
                    )]
                }
                _ => match self
                    .root_steps
                    .lock()
                    .await
                    .recv()
                    .await
                    .expect("scripted root steps remain available")
                {
                    RootStep::Tool {
                        call_id,
                        source,
                        mode,
                    } => {
                        if call_id == "captured-after-interrupt" {
                            assert!(
                                request
                                    .input
                                    .iter()
                                    .any(|item| item.0["content"] == RESUME_INPUT),
                                "resumed root request must include its durable browser input"
                            );
                        }
                        vec![haskell_call(&call_id, &source, mode)]
                    }
                    RootStep::Finish => vec![harness::item::Item(json!({
                        "type":"message", "role":"assistant", "phase":"final_answer",
                        "content":[{"type":"output_text","text":"same-cell child replies received"}]
                    }))],
                },
            };
            (format!("captured-parent-{round}"), items)
        } else {
            let (ordinal, round) = {
                let mut children = self.children.lock();
                let ordinal = children.len();
                let state = children
                    .entry(request.session_id.clone())
                    .or_insert(ChildRounds { ordinal, round: 0 });
                state.round += 1;
                (state.ordinal, state.round)
            };
            let items = match round {
                1 => {
                    let current = if ordinal < 2 {
                        PENDING_CALL
                    } else {
                        REUSE_CALL
                    };
                    let claims = self
                        .runtime
                        .store()
                        .claims(&CallId(current.into()))
                        .unwrap();
                    let expected = self.operation(&self.root_origin, current);
                    let claim = claims
                        .iter()
                        .find(|claim| {
                            claim.operation == expected && claim.request == expected.request
                        })
                        .expect("original captured operation remains admitted");
                    assert!(
                        matches!(claim.state, harness::store::ClaimState::Pending),
                        "child started after parent result settled"
                    );
                    if ordinal >= 2 {
                        let original = self
                            .runtime
                            .store()
                            .claims(&CallId(PENDING_CALL.into()))
                            .unwrap();
                        let failed = self.operation(&self.root_origin, PENDING_CALL);
                        assert!(original.iter().any(|claim| claim.operation == failed
                            && claim.request == failed.request && claim.state == harness::store::ClaimState::Settled),
                            "failed original operation was not durably settled before capture reuse");
                    }
                    let input = serde_json::to_value(&request.input).unwrap();
                    let items = input.as_array().unwrap();
                    assert!(
                        !items
                            .iter()
                            .any(|item| item["call_id"] == PENDING_CALL
                                || item["call_id"] == REUSE_CALL),
                        "current unfinished call leaked into child provider history"
                    );
                    assert!(
                        items
                            .iter()
                            .any(|item| item["call_id"] == "captured-scope-setup"),
                        "earlier provider provenance was lost"
                    );
                    self.requests.send(request.clone()).unwrap();
                    if ordinal < 2 {
                        let mut release = self.reply_children.subscribe();
                        while !*release.borrow_and_update() {
                            release.changed().await.unwrap();
                        }
                    }
                    vec![harness::item::Item(json!({
                        "type":"custom_tool_call", "call_id":format!("captured-child-{path}"),
                        "name":"haskell", "input":match self.scenario {
                            CapturedScenario::ConcurrentNominalJoin => "respond (m2MakeReply sessionInput)",
                            CapturedScenario::FailureAfterReplies if ordinal >= 2 => include_str!("embedded_captured_child_reuse_nominal.hs"),
                            _ => "respond capturedGetter",
                        }
                    }))]
                }
                2 if self.scenario == CapturedScenario::FailureAfterReplies && ordinal < 2 => {
                    let mut failed = self.parent_failed.subscribe();
                    while !*failed.borrow_and_update() {
                        failed.changed().await.unwrap();
                    }
                    vec![harness::item::Item(json!({
                        "type":"custom_tool_call", "call_id":format!("captured-child-after-failure-{path}"),
                        "name":"haskell", "input":"display (show (capturedValue, capturedGetter))"
                    }))]
                }
                3 if self.scenario == CapturedScenario::FailureAfterReplies && ordinal < 2 => {
                    let (prefix, incarnation) = request.session_id.rsplit_once(':').unwrap();
                    let (run, path) = prefix.rsplit_once(':').unwrap();
                    self.reads
                        .send((
                            ConversationIdentity::Embedded {
                                run: run.into(),
                                actor: AgentPath(path.into()),
                                incarnation: incarnation.into(),
                            },
                            format!("captured-child-after-failure-{path}"),
                        ))
                        .unwrap();
                    let mut finished = self.finish_children.subscribe();
                    while !*finished.borrow_and_update() {
                        finished.changed().await.unwrap();
                    }
                    vec![harness::item::Item(json!({
                        "type":"message", "role":"assistant", "phase":"final_answer",
                        "content":[{"type":"output_text","text":"retained child scope remains usable"}]
                    }))]
                }
                2 if self.scenario == CapturedScenario::ConcurrentNominalJoin => {
                    let mut finished = self.finish_children.subscribe();
                    while !*finished.borrow_and_update() {
                        finished.changed().await.unwrap();
                    }
                    vec![harness::item::Item(json!({
                        "type":"message", "role":"assistant", "phase":"final_answer",
                        "content":[{"type":"output_text","text":"original nominal reply delivered"}]
                    }))]
                }
                2 => vec![harness::item::Item(json!({
                    "type":"message", "role":"assistant", "phase":"final_answer",
                    "content":[{"type":"output_text","text":"typed reply delivered"}]
                }))],
                other => panic!("unexpected captured child request {other}"),
            };
            (format!("captured-child-{path}-{round}"), items)
        };
        for item in &items {
            if item.0["type"] == "custom_tool_call" {
                let call_id = item.0["call_id"].as_str().unwrap().to_owned();
                let operation = OperationId {
                    origin: origin.clone(),
                    request: request_id.clone(),
                    call: CallId(call_id.clone()),
                };
                assert!(
                    self.operations
                        .lock()
                        .insert((origin.clone(), call_id.clone()), operation)
                        .is_none(),
                    "fixture unexpectedly reused a provider call identity"
                );
                if origin == self.root_origin && call_id == "captured-scope-setup" {
                    self.scope_setup_issued.notify_one();
                }
            }
        }
        Ok(ResponsesTurn {
            response_id,
            items,
            usage: Usage::default(),
        })
    }
}

#[async_trait]
impl ResponsesTransport for CapturedHostTransport {
    async fn create(&self, _: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        panic!("checkpoint gate requires the Engine's exact durable request identity")
    }

    async fn create_streaming_for_request(
        &self,
        request_id: &RequestId,
        request: ResponsesRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        let turn = self.create_for_request(request_id, request).await?;
        for item in &turn.items {
            sink.send(harness::transport::sse::StreamEvent::ItemDone(item.clone()))
                .await
                .map_err(|_| TransportError::Stream("provider stream receiver closed".into()))?;
        }
        Ok(turn)
    }
}

fn assert_committed_haskell_value(response: &Value, expected: &str) {
    let committed_run =
        serde_json::to_value(tidepool_runtime::session::WorkbenchRunStatus::Committed).unwrap();
    let committed_item =
        serde_json::to_value(tidepool_runtime::session::WorkbenchItemStatus::Committed).unwrap();
    assert_eq!(response["status"], committed_run, "{response}");
    assert_eq!(response["publication"]["status"], "published", "{response}");
    for item in response["items"].as_array().expect("cell item receipts") {
        assert_eq!(item["status"], committed_item, "{response}");
    }
    let item = response["items"]
        .as_array()
        .expect("WorkbenchResponse.items must be an array")
        .last()
        .expect("Haskell operation must have a result item");
    assert_eq!(item["status"], committed_item, "{response}");
    let displays = response["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| item["operations"].as_array().into_iter().flatten())
        .filter_map(|operation| operation.get("display"))
        .collect::<Vec<_>>();
    let [display] = displays.as_slice() else {
        panic!("expected one explicit display: {response}");
    };
    assert_eq!(display["text"], expected, "{response}");
    assert!(
        display["output"]["sequence"].as_i64().is_some(),
        "{response}"
    );
    assert!(display["output"]["run"].as_str().is_some(), "{response}");
}

fn assert_replied_cell(receipt: &Value, expected_items: usize) {
    assert_eq!(receipt["status"], "replied", "{receipt}");
    assert_eq!(receipt["publication"]["status"], "published", "{receipt}");
    let items = receipt["items"].as_array().expect("child item receipts");
    assert_eq!(items.len(), expected_items, "{receipt}");
    for item in items {
        assert_eq!(item["status"], "committed", "{receipt}");
    }
    let reply = items.last().unwrap();
    assert_eq!(reply["terminalTransfer"], "replyAccepted", "{receipt}");
    assert!(
        reply["operations"]
            .as_array()
            .is_some_and(|operations| operations
                .iter()
                .any(|operation| operation["effect"] == "reply"
                    && operation["disposition"] == "committed")),
        "typed reply effect was not committed: {receipt}"
    );
}

async fn received_output(transport: &CapturedHostTransport, operation: &OperationId) {
    let mut changed = transport.provider_changed.subscribe();
    let expected = transport
        .runtime
        .store()
        .replay_output_operation(operation)
        .unwrap()
        .expect("exact operation has durable output before provider delivery");
    tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
        loop {
            if let Some(returned) = transport.provider_outputs.lock().get(operation).cloned() {
                assert_eq!(
                    returned,
                    vec![expected.clone()],
                    "provider must receive exactly the durable original output"
                );
                return;
            }
            changed
                .changed()
                .await
                .expect("provider observation owner remains live");
        }
    })
    .await
    .expect("exact durable output returned to its provider conversation");
}

async fn durable_output(
    runtime: &embedded_harness::EmbeddedHarnessRuntime,
    operation: &OperationId,
    output: &JobOutput,
) {
    let expected =
        harness::item::Item::tool_output(&operation.call, harness::item::ToolKind::Custom, output);
    tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
        loop {
            if let Some(item) = runtime.store().replay_output_operation(operation).unwrap() {
                assert_eq!(
                    item, expected,
                    "Store must retain the exact Scheduler terminal output"
                );
                let claims = runtime.store().claims_for_operation(operation).unwrap();
                assert!(!claims.is_empty());
                assert!(claims.iter().all(|claim| claim.operation == *operation
                    && claim.state == harness::store::ClaimState::Settled));
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("exact operation and output become durably settled");
}

async fn successful_rounds(
    context: &super::hosted_test_context::HostedActorContext,
    actors: &[ActorRef],
) {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let graph = context.forest.inspect_host_graph();
            if actors.iter().all(|actor| {
                graph.iter().any(|node| {
                    node.actor == *actor
                        && !node.provider_observation_stale
                        && node.provider_turn.as_ref().is_some_and(|turn| {
                            turn.state == exomonad_model::ProviderTurnState::Succeeded
                        })
                        && node.active_requests.is_empty()
                        && node.queued_requests.is_empty()
                })
            }) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual Engine success and durable settlement authorize idle cleanup");
}

async fn finish_root(
    transport: &CapturedHostTransport,
    context: &super::hosted_test_context::HostedActorContext,
) {
    transport.root_steps_tx.send(RootStep::Finish).unwrap();
    successful_rounds(context, &[context.actor.identity()]).await;
}

fn assert_absent_binding(response: &Value, binding: &str) {
    assert_preflight_rejection(response);
    assert_eq!(
        response["publication"]["status"], "notPublished",
        "{response}"
    );
    assert_eq!(response["publication"]["reason"], "rejected", "{response}");
    let diagnostics = response["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| item["diagnostics"].as_array().into_iter().flatten())
        .filter_map(|diagnostic| diagnostic["message"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        diagnostics.contains(binding),
        "specific absent binding missing from diagnostics: {response}"
    );
    assert!(
        diagnostics.to_lowercase().contains("not in scope"),
        "absence must be proved by name resolution: {response}"
    );
}

async fn embedded_operation(
    runtime: &embedded_harness::EmbeddedHarnessRuntime,
    operation: &OperationId,
    settlement_budget: Duration,
) -> Result<Value, String> {
    let call_id = &operation.call.0;
    let call = &operation.call;
    let claim = runtime
        .store()
        .claims(call)
        .unwrap()
        .into_iter()
        .find(|claim| &claim.operation == operation && claim.request == operation.request)
        .unwrap_or_else(|| panic!("embedded Haskell operation {call_id} was not admitted"));
    let turns = runtime.store().replay_turns(&operation.request).unwrap();
    assert_eq!(
        turns
            .iter()
            .filter(|turn| turn.request == operation.request
                && turn
                    .model_response
                    .items
                    .iter()
                    .any(|item| item.0["type"] == "custom_tool_call"
                        && item.0["name"] == "haskell"
                        && item.0["call_id"] == operation.call.0))
            .count(),
        1,
        "exact checkpoint operation must belong to one recorded provider response"
    );
    eprintln!("[captured-engine] wait exact operation {operation:?}");
    let output = tokio::time::timeout(
        settlement_budget,
        runtime.scheduler().wait(&claim.operation),
    )
    .await
    .unwrap_or_else(|_| {
        panic!("embedded Haskell operation {call_id} did not settle within {settlement_budget:?}")
    })
    .unwrap();
    durable_output(runtime, operation, &output).await;
    match output {
        JobOutput::Completed(result) => result.map_err(|error| error.to_string()),
        other => panic!("embedded Haskell operation {call_id} failed: {other:?}"),
    }
}

#[tokio::test]
async fn embedded_captured_unfold_awaits_two_child_replies_before_parent_call_returns() {
    captured_host_scenario(CapturedScenario::Success).await;
}

#[tokio::test]
async fn embedded_captured_children_and_capture_survive_failure_of_the_unfinished_parent_cell() {
    captured_host_scenario(CapturedScenario::FailureAfterReplies).await;
}

#[tokio::test]
async fn embedded_same_root_parked_nominal_a_joins_later_b_publication() {
    captured_host_scenario(CapturedScenario::ConcurrentNominalJoin).await;
}

#[tokio::test]
async fn embedded_parked_captured_pipeline_cancellation_settles_invocation_owned_children() {
    captured_host_scenario(CapturedScenario::CancelWhileParked).await;
}

async fn root_browser_projection(
    address: std::net::SocketAddr,
    cookie: &str,
    target: &harness::embedding::HostIdentity,
    lifecycle: harness::server::HostActorLifecycle,
) -> harness::server::HostActorProjection {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (mut socket, snapshot) =
                super::m1_host_tests::browser_snapshot(address, cookie).await;
            socket.close(None).await.unwrap();
            for value in snapshot["snapshot"]["actors"]
                .as_array()
                .expect("browser actor projection")
            {
                let actor: harness::server::HostActorProjection =
                    serde_json::from_value(value.clone()).unwrap();
                if actor.identity == *target && actor.lifecycle == lifecycle {
                    return actor;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("exact root browser lifecycle is projected")
}

// Offer another real provider tool call only after subscribing to its settlement.
async fn root_tool_call(
    transport: &CapturedHostTransport,
    call_id: &str,
    source: &str,
) -> Result<Value, String> {
    let scheduler = transport.runtime.scheduler();
    let mut settlements = scheduler.operation_settlements();
    transport
        .root_steps_tx
        .send(RootStep::Tool {
            call_id: call_id.into(),
            source: source.into(),
            mode: CallMode::Blocking,
        })
        .unwrap();
    tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
        loop {
            let settled = settlements.recv().await.expect("root settlement observer");
            if settled.origin == transport.root_origin && settled.call.0 == call_id {
                let expected = transport.operation(&transport.root_origin, call_id);
                assert_eq!(
                    settled, expected,
                    "settlement must match the exact provider call"
                );
                let result = embedded_operation(
                    &transport.runtime,
                    &expected,
                    COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
                )
                .await;
                received_output(transport, &expected).await;
                return result;
            }
        }
    })
    .await
    .expect("scripted root Haskell operation settles")
}

async fn captured_host_scenario(scenario: CapturedScenario) {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret_file = files.path().join("session-secret");
    std::fs::write(&secret_file, "embedded-children-secret-is-long-enough").unwrap();
    let auth_file = files.path().join("codex-auth.json");
    std::fs::write(&auth_file, "{}").unwrap();
    let settings = crate::exomonad::EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        public_origin: None,
        asset_root: assets,
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(secret_file),
        provider: crate::exomonad::EmbeddedModelProvider::Codex,
        credential_file: auth_file,
        context_capacity_tokens: 200_000,
        concurrent_jobs: 3,
    };
    let (requests_tx, mut requests_rx) = mpsc::unbounded_channel();
    let (reads_tx, mut reads_rx) = mpsc::unbounded_channel();
    let (root_steps_tx, root_steps) = mpsc::unbounded_channel();
    let prepared_transport = Arc::new(Mutex::new(None));
    let transport_slot = prepared_transport.clone();
    let host = super::hosted_test_context::HostedTestRuntime::start_with_factory(
        &settings,
        |_| {},
        move |runtime, config| {
            let root_origin = ConversationIdentity::Embedded {
                run: runtime_namespace(&config.run_root),
                actor: AgentPath("/root".into()),
                incarnation: exomonad_actor::Incarnation::FIRST.0.to_string(),
            };
            let transport = Arc::new(CapturedHostTransport {
                runtime: runtime.clone(),
                setup_settlements: Mutex::new(Some(runtime.scheduler().operation_settlements())),
                parent_prefix: Mutex::new(None),
                provider_outputs: Mutex::new(HashMap::new()),
                provider_changed: watch::channel(0).0,
                scenario,
                root_origin,
                root_round: AtomicUsize::new(0),
                operations: Mutex::new(HashMap::new()),
                scope_setup_issued: Notify::new(),
                setup_requested: Notify::new(),
                setup_ready: Notify::new(),
                root_steps: tokio::sync::Mutex::new(root_steps),
                root_steps_tx,
                children: Mutex::new(HashMap::new()),
                parent_failed: watch::channel(false).0,
                reply_children: watch::channel(false).0,
                finish_children: watch::channel(false).0,
                reads: reads_tx,
                requests: requests_tx,
            });
            *transport_slot.lock() = Some(transport.clone());
            transport
        },
    )
    .await
    .expect("production embedded host starts");
    let transport = prepared_transport
        .lock()
        .take()
        .expect("scripted Responses transport");
    let campaign = host.context.clone();
    let actor = campaign.actor.identity();
    let runtime = Arc::clone(&host.runtime);
    let address = host.address;
    let root_origin = transport.root_origin.clone();
    assert_eq!(
        root_origin,
        ConversationIdentity::Embedded {
            run: runtime_namespace(&campaign.config.run_root),
            actor: AgentPath("/root".into()),
            incarnation: actor.incarnation.0.to_string(),
        }
    );
    let scheduler = runtime.scheduler();
    let mut setup_settlements = transport.setup_settlements.lock().take().unwrap();
    tokio::time::timeout(
        Duration::from_secs(60),
        transport.scope_setup_issued.notified(),
    )
    .await
    .expect("root provider did not issue its original scope setup operation");
    let setup_operation = transport.operation(&root_origin, "captured-scope-setup");
    let setup = tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
        loop {
            let settled = setup_settlements
                .recv()
                .await
                .expect("scope setup settlement observation must remain available");
            if settled == setup_operation {
                break;
            }
        }
        embedded_operation(
            &runtime,
            &setup_operation,
            COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
        )
        .await
        .unwrap()
    })
    .await
    .expect("original scope setup operation did not settle within its cold budget");
    assert_committed_haskell_value(&setup, "True");
    tokio::time::timeout(
        Duration::from_secs(60),
        transport.setup_requested.notified(),
    )
    .await
    .expect("root Engine did not request a turn after scope setup settled");
    // Subscribe before issuing the parent call: settlement may precede observation,
    // and Scheduler::wait refuses operations that are not admitted yet.
    let mut parent_settlements = scheduler.operation_settlements();

    transport.setup_ready.notify_one();
    let mut sessions = std::collections::HashSet::new();
    for _ in 0..2 {
        let request = tokio::time::timeout(Duration::from_secs(120), async {
            tokio::select! {
                biased;
                settled = async {
                    loop {
                        let operation = parent_settlements.recv().await
                            .expect("exact parent settlement observation must remain available");
                        if operation.origin == root_origin && operation.call.0 == PENDING_CALL {
                            return operation;
                        }
                    }
                } => {
                    assert_eq!(settled, transport.operation(&root_origin, PENDING_CALL));
                    let result = scheduler.wait(&settled).await
                        .expect("settlement event follows admitted retained terminal output");
                    panic!("parent settled before both captured children started: {result:?}");
                }
                request = requests_rx.recv() => request,
            }
        })
        .await
        .expect("captured children did not start while the parent call was pending")
        .unwrap();
        let frozen = transport
            .parent_prefix
            .lock()
            .clone()
            .expect("original request prefix captured");
        assert!(
            request.input.starts_with(&frozen),
            "child history must preserve the exact frozen provider prefix"
        );
        assert!(request
            .input
            .iter()
            .all(|item| item.0["call_id"] != PENDING_CALL && item.0["call_id"] != REUSE_CALL));
        let parent = transport.operation(&root_origin, PENDING_CALL);
        assert!(
            runtime
                .store()
                .claims(&parent.call)
                .unwrap()
                .iter()
                .any(|claim| claim.operation == parent
                    && claim.request == parent.request
                    && claim.state == harness::store::ClaimState::Pending),
            "original exact parent operation settled before both children were offered replies"
        );
        let (prefix, incarnation) = request.session_id.rsplit_once(':').unwrap();
        let (run, actor) = prefix.rsplit_once(':').unwrap();
        sessions.insert(ConversationIdentity::Embedded {
            run: run.into(),
            actor: AgentPath(actor.into()),
            incarnation: incarnation.into(),
        });
    }
    assert_eq!(sessions.len(), 2);
    eprintln!(
        "[captured-engine] two child provider branches ready while exact parent claim is pending"
    );
    if scenario == CapturedScenario::CancelWhileParked {
        let pending = transport.operation(&root_origin, PENDING_CALL);
        let target = harness::embedding::HostIdentity {
            run: runtime_namespace(&campaign.config.run_root),
            actor: AgentPath("/root".into()),
            incarnation: actor.incarnation.0.to_string(),
        };
        let native = exomonad_tool::ToolInvocationContext {
            origin: exomonad_tool::ToolInvocationOrigin::Model(
                embedded_harness::original_operation(&target, &pending).unwrap(),
            ),
            call_id: pending.call.0.clone(),
            namespace: None,
        };
        tokio::time::timeout(Duration::from_secs(30), async {
            while campaign.actor.hosted_workbench_waiting(&native).is_none() {
                assert!(runtime
                    .store()
                    .claims_for_operation(&pending)
                    .unwrap()
                    .iter()
                    .any(|claim| claim.state == harness::store::ClaimState::Pending));
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the exact native cancellation owner is armed");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let login = client
            .post(format!("{api}/session"))
            .header("Origin", &origin)
            .json(&json!({"secret": "embedded-children-secret-is-long-enough"}))
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
        let running = root_browser_projection(
            address,
            &cookie,
            &target,
            harness::server::HostActorLifecycle::Running,
        )
        .await;
        let round = running
            .active_round
            .expect("running root owns the interrupted Engine round");
        let interrupt = harness::server::ClientCommand::Host {
            operation_id: harness::embedding::ClientOperationId(uuid::Uuid::new_v4()),
            command: harness::server::HostCommand::Interrupt {
                target: target.clone(),
                expected_round: round,
            },
        };
        assert!(campaign.actor.hosted_workbench_waiting(&native).is_some());
        assert!(scheduler.output(&pending).await.unwrap().is_none());
        let requested = client
            .post(format!("{api}/commands"))
            .header("Origin", &origin)
            .header(reqwest::header::COOKIE, &cookie)
            .json(&interrupt)
            .send()
            .await
            .unwrap();
        assert_eq!(requested.status(), reqwest::StatusCode::ACCEPTED);
        let parent = transport.operation(&root_origin, PENDING_CALL);
        let cancelled = tokio::time::timeout(Duration::from_secs(30), scheduler.wait(&parent))
            .await
            .expect("exact original parent operation settles after browser interrupt")
            .unwrap();
        match cancelled {
            JobOutput::CancelledWithReceipt(Ok(ref receipt)) => {
                assert_eq!(receipt["publication"]["status"], "notPublished");
                assert_eq!(receipt["publication"]["reason"], "cancelled");
            }
            JobOutput::CancelledWithReceipt(Err(ref failure)) => {
                let metadata = failure
                    .metadata()
                    .expect("cancellation retains its publication outcome");
                assert_eq!(metadata["publication"]["status"], "notPublished");
                assert_eq!(metadata["publication"]["reason"], "cancelled");
            }
            other => panic!("parked pipeline did not settle as cancelled: {other:?}"),
        }
        assert!(
            matches!(
                scheduler
                    .cancellation_acknowledgment(&pending)
                    .await
                    .unwrap(),
                Some(harness::provider::CancellationAcknowledgment::StoppedWithReceipt(_))
            ),
            "the exact native owner must acknowledge cancellation"
        );
        durable_output(&runtime, &pending, &cancelled).await;
        let known_children = campaign
            .observer
            .installations()
            .into_iter()
            .filter(|installation| installation.checkpoint)
            .map(|installation| installation.actor)
            .collect::<Vec<_>>();
        assert_eq!(known_children.len(), 2);
        for child in &known_children {
            let terminal = child.terminal();
            tokio::time::timeout(Duration::from_secs(30), terminal.wait())
                .await
                .expect("invocation-owned children retire after the interrupted call");
            let cleanup = terminal
                .cleanup()
                .expect("retirement retains owner cleanup evidence");
            assert_eq!(cleanup.actor(), child.identity());
            assert!(cleanup.is_confirmed(), "{cleanup:?}");
        }
        assert_eq!(
            campaign
                .forest
                .measurement_snapshot()
                .and_then(|snapshot| snapshot.parked),
            Some(0)
        );
        assert!(
            campaign.actor.terminal().get().is_none(),
            "interrupt must preserve the root actor"
        );
        let waiting = root_browser_projection(
            address,
            &cookie,
            &target,
            harness::server::HostActorLifecycle::Waiting,
        )
        .await;
        assert!(
            waiting.active_round.is_none(),
            "interrupted Engine round must clear before new input"
        );
        let input = harness::server::ClientCommand::Host {
            operation_id: harness::embedding::ClientOperationId(uuid::Uuid::new_v4()),
            command: harness::server::HostCommand::Input {
                target: target.clone(),
                text: RESUME_INPUT.into(),
            },
        };
        let wake = client
            .post(format!("{api}/commands"))
            .header("Origin", &origin)
            .header(reqwest::header::COOKIE, &cookie)
            .json(&input)
            .send();
        let resumed = root_tool_call(
            &transport,
            "captured-after-interrupt",
            "let resumedAfterInterrupt = x + getX\ndisplay (resumedAfterInterrupt == 83)",
        );
        let (woken, resumed) = tokio::join!(wake, resumed);
        assert_eq!(woken.unwrap().status(), reqwest::StatusCode::ACCEPTED);
        assert_committed_haskell_value(
            &resumed.expect("new root Haskell call succeeds after interrupt"),
            "True",
        );
        let absent = root_tool_call(
            &transport,
            "captured-cancelled-prefix-absent",
            "display (capturedValue :: Int)",
        )
        .await
        .unwrap();
        assert_absent_binding(&absent, "capturedValue");
        let cleaned = root_tool_call(
            &transport,
            "captured-interrupted-group-cleanup",
            include_str!("embedded_captured_group_cleanup.hs"),
        )
        .await
        .unwrap();
        assert_committed_haskell_value(&cleaned, "True");
        assert!(
            campaign.actor.terminal().get().is_none(),
            "resumed root remains live until ordinary shutdown"
        );
        received_output(&transport, &pending).await;
        finish_root(&transport, &campaign).await;
        host.stop()
            .await
            .expect("production shutdown confirms host and forest cleanup");
        return;
    }
    if scenario == CapturedScenario::ConcurrentNominalJoin {
        let published_b = root_tool_call(
            &transport,
            "nominal-join-root-b",
            include_str!("embedded_nominal_join_b.hs"),
        )
        .await
        .expect("same-root B publishes while A remains parked");
        assert_committed_haskell_value(&published_b, "True");
        assert_eq!(
            published_b["publication"]["status"], "published",
            "{published_b}"
        );
        let original_a = transport.operation(&root_origin, PENDING_CALL);
        assert!(
            runtime
                .store()
                .claims(&original_a.call)
                .unwrap()
                .iter()
                .any(|claim| claim.operation == original_a
                    && claim.request == original_a.request
                    && claim.state == harness::store::ClaimState::Pending),
            "B must publish before the original root A call settles"
        );
    }
    transport.reply_children.send_replace(true);
    // Observe each admitted child's terminal reply before waiting for the parent:
    // a rejected child cell cannot deliver the typed response the parent awaits.
    let mut replied = std::collections::HashSet::new();
    tokio::time::timeout(TYPED_CHILD_REPLY_SETTLEMENT_BUDGET, async {
        while replied.len() < sessions.len() {
            let operation = parent_settlements
                .recv()
                .await
                .expect("child settlement observer");
            if sessions.contains(&operation.origin)
                && operation.call.0 == format!("captured-child-{}", operation.origin.actor().0)
            {
                let reply =
                    embedded_operation(&runtime, &operation, TYPED_CHILD_REPLY_SETTLEMENT_BUDGET)
                        .await
                        .unwrap_or_else(|cause| {
                            panic!(
                        "captured child cell rejected before typed reply: {operation:?}: {cause}"
                    )
                        });
                assert_replied_cell(&reply, 1);
                assert!(replied.insert(operation.origin));
            }
        }
    })
    .await
    .expect("both captured child operations settle");
    let result = embedded_operation(
        &runtime,
        &transport.operation(&root_origin, PENDING_CALL),
        COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
    )
    .await;
    match scenario {
        CapturedScenario::Success | CapturedScenario::CancelWhileParked => {
            assert_committed_haskell_value(&result.unwrap(), "True");
            received_output(&transport, &transport.operation(&root_origin, PENDING_CALL)).await;
            let public = root_tool_call(
                &transport,
                "captured-public-prefix",
                "display (capturedValue == 41 && capturedGetter == 42)",
            )
            .await
            .unwrap();
            assert_committed_haskell_value(&public, "True");
        }
        CapturedScenario::ConcurrentNominalJoin => {
            let published_a = result.unwrap();
            assert_committed_haskell_value(&published_a, "True");
            assert_eq!(
                published_a["publication"]["status"], "published",
                "{published_a}"
            );
            let joined = root_tool_call(
                &transport,
                "nominal-join-final-read",
                include_str!("embedded_nominal_join_final.hs"),
            )
            .await
            .expect("final provider tool reads both A and B public declarations");
            assert_committed_haskell_value(&joined, "True");
            received_output(&transport, &transport.operation(&root_origin, PENDING_CALL)).await;
        }
        CapturedScenario::FailureAfterReplies => {
            result.expect_err(
                "the original unfinished parent cell must fail after its child replies",
            );
            let failed = scheduler
                .wait(&transport.operation(&root_origin, PENDING_CALL))
                .await
                .unwrap();
            let JobOutput::Completed(Err(failure)) = failed else {
                panic!("failed creator must retain its exact tool failure: {failed:?}");
            };
            let metadata = failure
                .metadata()
                .expect("failed creator retains structured publication");
            assert_eq!(metadata["publication"]["status"], "notPublished");
            assert_eq!(metadata["publication"]["reason"], "failed");
            assert!(metadata["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["operations"]
                    .as_array()
                    .is_some_and(|operations| !operations.is_empty()))));
            assert!(
                campaign.actor.terminal().get().is_none(),
                "the fixture fails a cell while its parent actor remains live"
            );
            eprintln!(
                "[captured-engine] actual parent execution failed after both typed child replies"
            );
            let public_names = root_tool_call(
                &transport,
                "captured-parent-public-probe",
                "display (show (x, getX))",
            )
            .await
            .expect("earlier public names remain installed");
            assert_committed_haskell_value(&public_names, "(41,42)");
            for (name, source) in [
                ("capturedValue", "display (capturedValue :: Int)"),
                ("capturedGetter", "display (capturedGetter :: Int)"),
                (
                    "privateCapturedHelper",
                    "display (privateCapturedHelper (0 :: Int))",
                ),
                ("capturedSuffix", "display (capturedSuffix :: Int)"),
            ] {
                let absent = root_tool_call(
                    &transport,
                    &format!("captured-parent-absent-{name}"),
                    source,
                )
                .await
                .expect("missing failed-cell name returns a compile rejection");
                assert_absent_binding(&absent, name);
            }
            let rebound = root_tool_call(
                &transport, "captured-parent-rebind",
                "capturedValue <- pure (99 :: Int)\nlet capturedGetter = capturedValue + 1\ndisplay (show (capturedValue, capturedGetter))",
            ).await.expect("public rebinding succeeds after the failed cell");
            assert_committed_haskell_value(&rebound, "(99,100)");
            transport.parent_failed.send_replace(true);
            let mut retained = std::collections::HashSet::new();
            for _ in 0..2 {
                let (origin, call) =
                    tokio::time::timeout(Duration::from_secs(120), reads_rx.recv())
                        .await
                        .expect(
                            "admitted children did not read their scope after parent cell failure",
                        )
                        .unwrap();
                let value = embedded_operation(
                    &runtime,
                    &transport.operation(&origin, &call),
                    COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
                )
                .await
                .unwrap();
                assert_committed_haskell_value(&value, "(41,42)");
                retained.insert(origin);
            }
            assert_eq!(retained.len(), 2);
            let reused = root_tool_call(
                &transport,
                REUSE_CALL,
                include_str!("embedded_captured_unfold_reuse_after_failure.hs"),
            )
            .await
            .expect("the failed cell's transferred capture admits and joins a third child");
            let third_request = requests_rx
                .try_recv()
                .expect("third child provider request");
            let frozen = transport.parent_prefix.lock().clone().unwrap();
            assert!(
                third_request.input.starts_with(&frozen),
                "transferred capture preserves its exact original provider prefix"
            );
            let (prefix, incarnation) = third_request.session_id.rsplit_once(':').unwrap();
            let (run, path) = prefix.rsplit_once(':').unwrap();
            let origin = ConversationIdentity::Embedded {
                run: run.into(),
                actor: AgentPath(path.into()),
                incarnation: incarnation.into(),
            };
            assert!(
                !sessions.contains(&origin),
                "reuse must admit an independent third child"
            );
            let third_operation = transport.operation(&origin, &format!("captured-child-{path}"));
            let reply = embedded_operation(
                &runtime,
                &third_operation,
                TYPED_CHILD_REPLY_SETTLEMENT_BUDGET,
            )
            .await
            .unwrap();
            assert_replied_cell(&reply, 3);
            assert_committed_haskell_value(&reused, "True");
            eprintln!(
                "[captured-engine] retained children and independently reused capture succeeded"
            );
        }
    }
    let expected_children = if scenario != CapturedScenario::FailureAfterReplies {
        2
    } else {
        3
    };
    assert_eq!(transport.children.lock().len(), expected_children);
    if matches!(
        scenario,
        CapturedScenario::FailureAfterReplies | CapturedScenario::ConcurrentNominalJoin
    ) {
        for origin in &sessions {
            let operation =
                transport.operation(origin, &format!("captured-child-{}", origin.actor().0));
            received_output(&transport, &operation).await;
        }
        let refusal = root_tool_call(
            &transport,
            "captured-active-cleanup-refusal",
            include_str!("embedded_captured_group_cleanup.hs"),
        )
        .await
        .unwrap();
        assert_committed_haskell_value(&refusal, "False");
        let actors = campaign
            .observer
            .installations()
            .into_iter()
            .filter(|installation| {
                installation.checkpoint && installation.actor.terminal().get().is_none()
            })
            .map(|installation| installation.actor.identity())
            .collect::<Vec<_>>();
        assert_eq!(
            actors.len(),
            2,
            "refused cleanup must retain both actor-owned children"
        );
        for actor in &actors {
            let node = campaign
                .forest
                .inspect_host_graph()
                .into_iter()
                .find(|node| node.actor == *actor)
                .unwrap();
            assert!(node
                .provider_turn
                .as_ref()
                .is_some_and(|turn| turn.state == exomonad_model::ProviderTurnState::Active));
            assert!(node.terminal.is_none());
        }
        transport.finish_children.send_replace(true);
        successful_rounds(&campaign, &actors).await;
    }
    let cleaned = root_tool_call(
        &transport,
        "captured-group-cleanup",
        include_str!("embedded_captured_group_cleanup.hs"),
    )
    .await
    .expect("known original group cleanup settles");
    assert_committed_haskell_value(&cleaned, "True");
    let known_children = campaign
        .observer
        .installations()
        .into_iter()
        .filter(|installation| installation.checkpoint)
        .map(|installation| installation.actor)
        .collect::<Vec<_>>();
    assert_eq!(known_children.len(), expected_children);
    for child in &known_children {
        let terminal = child.terminal();
        tokio::time::timeout(Duration::from_secs(30), terminal.wait())
            .await
            .expect("known captured child retires after group cleanup");
        let cleanup = terminal
            .cleanup()
            .expect("child retirement retains cleanup evidence");
        assert_eq!(cleanup.actor(), child.identity());
        assert!(cleanup.is_confirmed(), "{cleanup:?}");
    }
    finish_root(&transport, &campaign).await;
    host.stop()
        .await
        .expect("production shutdown confirms host and forest cleanup");
}

fn preflight_invocation(
    request_id: &str,
    call_id: &str,
    source: String,
) -> exomonad_tool::ToolInvocation {
    exomonad_tool::ToolInvocation {
        context: Some(exomonad_tool::ToolInvocationContext::external(
            "whole-cell-preflight".into(),
            request_id.into(),
            call_id.into(),
            Some(call_id.into()),
            Some("haskell".into()),
        )),
        name: exomonad_actor::HASKELL_TOOL.into(),
        arguments: exomonad_tool::ToolArguments::Raw(source),
    }
}

async fn preflight_dispatch_without_effect(
    campaign: &mut test_campaign::TestCampaign,
    invocation: exomonad_tool::ToolInvocation,
) -> Value {
    let policy = campaign.root_installation.policy.clone();
    tokio::select! {
        result = policy.dispatch_json_boxed(invocation) => result.expect("actual admitted Haskell invocation settles"),
        effect = campaign.next_deployment("forbidden preflight notification", Duration::from_secs(120), |event| match event {
            LocalResidentDeployment::NotificationSend(command) => Ok(command),
            other => Err(other),
        }) => panic!("effect ran before complete-cell rejection: {}", effect.message()),
    }
}

fn assert_preflight_rejection(response: &Value) {
    use tidepool_runtime::session::{WorkbenchItemStatus, WorkbenchRunStatus};
    assert_eq!(
        response["status"],
        serde_json::to_value(WorkbenchRunStatus::Rejected).unwrap(),
        "{response}"
    );
    let items = response["items"].as_array().expect("rejection receipts");
    assert!(!items.is_empty(), "type error must retain its receipt");
    for item in items {
        assert_ne!(
            item["status"],
            serde_json::to_value(WorkbenchItemStatus::Committed).unwrap(),
            "{response}"
        );
        assert!(
            item["installedBindings"]
                .as_array()
                .is_none_or(Vec::is_empty),
            "{response}"
        );
        assert!(
            item["operations"].as_array().is_none_or(Vec::is_empty),
            "{response}"
        );
    }
}

#[tokio::test]
async fn admitted_cell_late_type_error_has_no_effect_or_publication_on_retry() {
    let mut campaign = test_campaign::TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(), |admission| admission,
        |config| {

            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(&authored).unwrap();
            std::fs::write(authored.join("AgentSpec.hs"), include_str!("embedded_bad_final_agent_spec.hs")).unwrap();
            std::fs::write(authored.join("config.toml"), "[haskell]\nsource_roots = ['.']\nmodules = ['AgentSpec']\nspec = 'AgentSpec.agentSpec'\n").unwrap();
            test_campaign::commit_workspace(&config.workspace);
            config.workspace_inputs = Some(crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root).unwrap());
        },
    ).await;
    let actor = campaign.actor.identity();
    let policy = campaign.root_installation.policy.clone();
    let status = policy
        .dispatch_json_boxed(exomonad_tool::ToolInvocation {
            context: None,
            name: "status".into(),
            arguments: exomonad_tool::ToolArguments::Structured(json!({"view":"detailed"})),
        })
        .await
        .unwrap();
    assert!(
        status.to_string().contains("AgentSpec.agentSpec"),
        "actual AgentSpec must be installed: {status}"
    );

    let runtime =
        embedded_harness::EmbeddedHarnessRuntime::open(campaign.session_root.path(), 1).unwrap();
    let embedded = runtime
        .attach(
            harness::embedding::HostIdentity {
                run: runtime_namespace(campaign.session_root.path()),
                actor: AgentPath("/root".into()),
                incarnation: actor.incarnation.0.to_string(),
            },
            campaign.actor.clone(),
            Arc::new(
                embedded_policy::EmbeddedPolicyInstallation::from_installation(
                    &campaign.root_installation,
                ),
            ),
            None,
        )
        .unwrap();
    let binding = open_embedded_actor_binding(
        campaign.session_root.path(),
        actor,
        AgentPath("/root".into()),
        Some(embedded.conversation.clone()),
    )
    .unwrap();
    assert_eq!(binding.inbox.watermark(), 0);
    let source = include_str!("embedded_bad_final_cell.hs")
        .replace("TARGET_ID", &actor.id.0.to_string())
        .replace("TARGET_INCARNATION", &actor.incarnation.0.to_string());
    let invocation = preflight_invocation("bad-final-request", "bad-final-call", source.clone());
    let rejected = preflight_dispatch_without_effect(&mut campaign, invocation.clone()).await;
    assert_preflight_rejection(&rejected);
    let diagnostics = rejected.to_string();
    assert!(
        diagnostics.contains("Bool") && diagnostics.contains("Int"),
        "must reject the real final Bool::Int type error: {rejected}"
    );
    assert_eq!(binding.inbox.watermark(), 0);
    campaign.assert_no_deployment("preflight rejected all effects", |event| {
        matches!(event, LocalResidentDeployment::NotificationSend(_))
    });
    policy
        .complete_boxed(tidepool_runtime::session::WorkbenchForkBoundary::external(
            "whole-cell-preflight".into(),
            "bad-final-request".into(),
            "bad-final-call".into(),
        ))
        .await
        .unwrap();
    let requests = tidepool_extract_cmd::extract_spawn_count();
    let retry = preflight_dispatch_without_effect(&mut campaign, invocation).await;
    assert_eq!(
        retry, rejected,
        "same exact operation must retain its rejection"
    );
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        requests,
        "retained rejection must not resubmit compiler work"
    );
    assert_eq!(binding.inbox.watermark(), 0);
    let absent = preflight_dispatch_without_effect(
        &mut campaign,
        preflight_invocation(
            "binding-probe-request",
            "binding-probe-call",
            "neverPublished".into(),
        ),
    )
    .await;
    assert_preflight_rejection(&absent);
    let absent_diagnostic = absent.to_string();
    assert!(
        absent_diagnostic.contains("neverPublished") && absent_diagnostic.contains("not in scope"),
        "rejected bind must remain absent from the public lexical environment: {absent}"
    );
    assert!(
        campaign.actor.terminal().get().is_none(),
        "type rejection keeps the admitted actor live"
    );

    let valid = source.replace("pure (True :: Int)", "display (42 :: Int)");
    let mut control = tokio::spawn(async move {
        policy
            .dispatch_json_boxed(preflight_invocation(
                "valid-control-request",
                "valid-control-call",
                valid,
            ))
            .await
    });
    let notification = tokio::select! {
        result = &mut control => panic!("valid first statement returned before emitting its effect: {result:?}"),
        effect = campaign.next_deployment("valid first-statement control", Duration::from_secs(120), |event| match event {
            LocalResidentDeployment::NotificationSend(command) => Ok(command), other => Err(other),
        }) => effect,
    };
    assert_eq!(notification.owner(), actor);
    assert_eq!(notification.target(), actor);
    assert_eq!(notification.message(), "whole-cell-preflight-sentinel");
    let mut notifications = JoinSet::new();
    schedule_embedded_notification_send(notification, &binding, &mut notifications);
    let (_, admitted) = tokio::time::timeout(Duration::from_secs(30), notifications.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    admitted.unwrap();
    let store = super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    let committed = campaign
        .drive_actor_output(&store, async {
            tokio::time::timeout(Duration::from_secs(120), control)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
        })
        .await;
    assert_committed_haskell_value(&committed, "42");
    assert!(
        committed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["installedBindings"]
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == "neverPublished"))),
        "valid control publishes its binding: {committed}"
    );
    assert_eq!(
        binding.inbox.watermark(),
        1,
        "only valid control can publish the actual effect"
    );
    let unread = runtime.store().unread("/root").unwrap();
    assert_eq!(unread.len(), 1);
    assert_eq!(
        runtime
            .store()
            .get_item(&unread[0].item_hash)
            .unwrap()
            .unwrap()
            .0["content"],
        "whole-cell-preflight-sentinel"
    );
    campaign.assert_no_deployment("valid control executes once", |event| {
        matches!(event, LocalResidentDeployment::NotificationSend(_))
    });
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
}
