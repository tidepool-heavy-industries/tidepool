use super::test_campaign::{explicit_display_text, COLD_DEBUG_CELL_SETTLEMENT_BUDGET};
use super::*;
use async_trait::async_trait;
use futures_util::FutureExt;
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
const LOCAL_STARTUP_CALL: &str = "read-started-local-actors";
const INTENTIONAL_COORDINATOR_FAILURE: &str = "HOSTED_INTENTIONAL_COORDINATOR_FAILURE";
const INTENTIONAL_PARENT_FAILURE: &str = "M2_INTENTIONAL_PARENT_EXECUTION_FAILURE";

const TYPED_CHILD_REPLY_SETTLEMENT_BUDGET: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, PartialEq, Eq)]
enum CapturedScenario {
    Success,
    CancelWhileParked,
    ShutdownAfterChildFailure,
    CoordinatorFailureAfterChildFailure,
    FailureAfterReplies,
    ConcurrentNominalJoin,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HostedScenario {
    LocalActorStartup,
    Captured(CapturedScenario),
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

struct FrozenProviderPrefix {
    transcript: Vec<harness::item::Item>,
    inherited_effort: harness::model::Effort,
    configuration_positions: Vec<usize>,
}

impl FrozenProviderPrefix {
    fn capture(request: &ResponsesRequest) -> Self {
        let configuration_positions = request
            .input
            .iter()
            .enumerate()
            .filter_map(|(position, item)| item.is_configuration_update().then_some(position))
            .collect::<Vec<_>>();
        let inherited_effort = request
            .input
            .iter()
            .rev()
            .find_map(harness::item::Item::configuration_effort)
            .expect("original provider request retains its effective effort pin");
        Self {
            transcript: request
                .input
                .iter()
                .filter(|item| !item.is_configuration_update())
                .cloned()
                .collect(),
            inherited_effort,
            configuration_positions,
        }
    }

    fn assert_inherited(&self, request: &ResponsesRequest) {
        // BeforeCall copies every ordinary item unchanged and re-roots the
        // latest effort pin after that transcript. Configuration positions are
        // branch-local; neither configuration nor transcript may be lost.
        let transcript = request
            .input
            .iter()
            .filter(|item| !item.is_configuration_update())
            .cloned()
            .collect::<Vec<_>>();
        let difference = self
            .transcript
            .iter()
            .zip(&transcript)
            .position(|(expected, actual)| expected != actual)
            .unwrap_or_else(|| self.transcript.len().min(transcript.len()));
        assert!(
            transcript.starts_with(&self.transcript),
            "child {} changed its exact inherited provider transcript: first_difference={}, frozen_items={}, child_items={}, frozen_configuration_positions={:?}, frozen_item={}, child_item={}",
            request.session_id,
            difference,
            self.transcript.len(),
            transcript.len(),
            self.configuration_positions,
            provider_item_diagnostic(self.transcript.get(difference)),
            provider_item_diagnostic(transcript.get(difference)),
        );
        let inherited_effort = request
            .input
            .iter()
            .find_map(harness::item::Item::configuration_effort);
        assert_eq!(
            inherited_effort,
            Some(self.inherited_effort),
            "child {} must retain the checkpoint's inherited effort pin",
            request.session_id,
        );
        assert_eq!(
            request.pinned_effort, self.inherited_effort,
            "child {} request effort must mirror its inherited positional pin",
            request.session_id,
        );
        let configuration_positions = request
            .input
            .iter()
            .enumerate()
            .filter_map(|(position, item)| item.is_configuration_update().then_some(position))
            .collect::<Vec<_>>();
        eprintln!(
            "[captured-engine] child {} preserves {} exact inherited provider items and {:?} effort; frozen configuration positions {:?}, child configuration positions {:?}",
            request.session_id,
            self.transcript.len(),
            self.inherited_effort,
            self.configuration_positions,
            configuration_positions,
        );
    }
}

fn provider_item_diagnostic(item: Option<&harness::item::Item>) -> String {
    match item {
        Some(item) => serde_json::to_string(item)
            .expect("normalized provider item serializes")
            .chars()
            .take(2048)
            .collect(),
        None => "<missing>".into(),
    }
}

struct CapturedHostTransport {
    runtime: Arc<embedded_harness::EmbeddedHarnessRuntime>,
    setup_settlements: Mutex<Option<tokio::sync::broadcast::Receiver<OperationId>>>,
    parent_prefix: Mutex<Option<FrozenProviderPrefix>>,
    provider_outputs: Mutex<HashMap<OperationId, Vec<harness::item::Item>>>,
    provider_changed: watch::Sender<u64>,
    scenario: HostedScenario,
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
                        HostedScenario::Captured(CapturedScenario::ShutdownAfterChildFailure | CapturedScenario::CoordinatorFailureAfterChildFailure) => format!("{}\n{}\n{}\n{}\ndisplay True", tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_checkpoint_scope_setup.hs"), tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_captured_group_setup.hs"), tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_shutdown_command_refusal.hs"), tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_shutdown_gate_setup.hs")),
                        HostedScenario::LocalActorStartup | HostedScenario::Captured(CapturedScenario::Success | CapturedScenario::CancelWhileParked | CapturedScenario::FailureAfterReplies) => format!("{}\n{}\ndisplay True", tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_checkpoint_scope_setup.hs"), tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_captured_group_setup.hs")),
                        HostedScenario::Captured(CapturedScenario::ConcurrentNominalJoin) => format!("{}\n{}\n{}", tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_checkpoint_scope_setup.hs"), tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_captured_group_setup.hs"), tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_nominal_join_setup.hs")),
                    }
                }))],
                2 => {
                    *self.parent_prefix.lock() = Some(FrozenProviderPrefix::capture(&request));
                    self.setup_requested.notify_one();
                    self.setup_ready.notified().await;
                    let source = match self.scenario {
                        HostedScenario::LocalActorStartup => {
                            "seed <- R.call (readSeed (R.client seedStore)) ()\ngroup <- R.call (readGroup (R.client groupStore)) ()\ndisplay (case (seed, group) of (Nothing, Nothing) -> True; _ -> False)".to_owned()
                        }
                        HostedScenario::Captured(CapturedScenario::Success | CapturedScenario::CancelWhileParked) => {
                            tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_captured_unfold_and_await.hs")
                        }
                        HostedScenario::Captured(CapturedScenario::ShutdownAfterChildFailure | CapturedScenario::CoordinatorFailureAfterChildFailure) => {
                            tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_shutdown_parent_join.hs")
                        }
                        HostedScenario::Captured(CapturedScenario::FailureAfterReplies) => {
                            tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_captured_unfold_await_then_fail.hs")
                        }
                        HostedScenario::Captured(CapturedScenario::ConcurrentNominalJoin) => {
                            tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_nominal_join_a.hs")
                        }
                    };
                    vec![haskell_call(
                        if self.scenario == HostedScenario::LocalActorStartup {
                            LOCAL_STARTUP_CALL
                        } else {
                            PENDING_CALL
                        },
                        &source,
                        if self.scenario
                            == HostedScenario::Captured(CapturedScenario::ConcurrentNominalJoin)
                        {
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
                        assert!(
                            original.iter().any(|claim| claim.operation == failed
                                && claim.request == failed.request
                                && claim.state == harness::store::ClaimState::Settled),
                            "failed original operation was not durably settled before capture reuse"
                        );
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
                            HostedScenario::Captured(CapturedScenario::ShutdownAfterChildFailure | CapturedScenario::CoordinatorFailureAfterChildFailure) if ordinal == 0 => "display (error \"HOSTED_INTENTIONAL_CHILD_FAILURE\" :: Int)".to_owned(),
                            HostedScenario::Captured(CapturedScenario::ShutdownAfterChildFailure | CapturedScenario::CoordinatorFailureAfterChildFailure) => tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_shutdown_parked_child.hs"),
                            HostedScenario::Captured(CapturedScenario::ConcurrentNominalJoin) => "respond (m2MakeReply sessionInput)".to_owned(),
                            HostedScenario::Captured(CapturedScenario::FailureAfterReplies) if ordinal >= 2 => tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_captured_child_reuse_nominal.hs"),
                            _ => "respond capturedGetter".to_owned(),
                        }
                    }))]
                }
                2 if self.scenario
                    == HostedScenario::Captured(CapturedScenario::FailureAfterReplies)
                    && ordinal < 2 =>
                {
                    let mut failed = self.parent_failed.subscribe();
                    while !*failed.borrow_and_update() {
                        failed.changed().await.unwrap();
                    }
                    vec![harness::item::Item(json!({
                        "type":"custom_tool_call", "call_id":format!("captured-child-after-failure-{path}"),
                        "name":"haskell", "input":"display (show (capturedValue, capturedGetter))"
                    }))]
                }
                3 if self.scenario
                    == HostedScenario::Captured(CapturedScenario::FailureAfterReplies)
                    && ordinal < 2 =>
                {
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
                2 if self.scenario
                    == HostedScenario::Captured(CapturedScenario::ConcurrentNominalJoin) =>
                {
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
    assert_eq!(explicit_display_text(response), expected, "{response}");
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
    context
        .while_root_live("successful root provider round", async {
            transport.root_steps_tx.send(RootStep::Finish).unwrap();
            successful_rounds(context, &[context.actor.identity()]).await;
        })
        .await
        .unwrap_or_else(|error| panic!("{error}"));
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
async fn embedded_local_actor_startup_and_calls_keep_custody_off_the_host_stack() {
    captured_host_scenario(HostedScenario::LocalActorStartup).await;
}

#[tokio::test]
async fn embedded_captured_unfold_awaits_two_child_replies_before_parent_call_returns() {
    captured_host_scenario(HostedScenario::Captured(CapturedScenario::Success)).await;
}

#[tokio::test]
async fn embedded_host_shutdown_after_child_failure_settles_active_calls() {
    captured_host_scenario(HostedScenario::Captured(
        CapturedScenario::ShutdownAfterChildFailure,
    ))
    .await;
}

#[tokio::test]
async fn embedded_coordinator_failure_after_child_failure_preserves_cleanup_proof() {
    captured_host_scenario(HostedScenario::Captured(
        CapturedScenario::CoordinatorFailureAfterChildFailure,
    ))
    .await;
}

#[tokio::test]
async fn embedded_captured_children_and_capture_survive_failure_of_the_unfinished_parent_cell() {
    captured_host_scenario(HostedScenario::Captured(
        CapturedScenario::FailureAfterReplies,
    ))
    .await;
}

#[tokio::test]
async fn embedded_same_root_parked_nominal_a_joins_later_b_publication() {
    captured_host_scenario(HostedScenario::Captured(
        CapturedScenario::ConcurrentNominalJoin,
    ))
    .await;
}

#[tokio::test]
async fn embedded_parked_captured_pipeline_cancellation_settles_invocation_owned_children() {
    captured_host_scenario(HostedScenario::Captured(
        CapturedScenario::CancelWhileParked,
    ))
    .await;
}

async fn root_browser_projection(
    context: &super::hosted_test_context::HostedActorContext,
    address: std::net::SocketAddr,
    cookie: &str,
    target: &harness::embedding::HostIdentity,
    lifecycle: harness::server::HostActorLifecycle,
) -> harness::server::HostActorProjection {
    context
        .while_root_live(
            "root browser lifecycle projection",
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
                    tokio::task::yield_now().await;
                }
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{error}"))
        .expect("exact root browser lifecycle is projected")
}

// Offer another real provider tool call only after subscribing to its settlement.
async fn root_tool_call(
    context: &super::hosted_test_context::HostedActorContext,
    transport: &CapturedHostTransport,
    call_id: &str,
    source: &str,
) -> Result<Value, String> {
    let scheduler = transport.runtime.scheduler();
    let mut settlements = scheduler.operation_settlements();
    context
        .while_root_live(
            "root Haskell operation and provider output",
            tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                transport
                    .root_steps_tx
                    .send(RootStep::Tool {
                        call_id: call_id.into(),
                        source: source.into(),
                        mode: CallMode::Blocking,
                    })
                    .unwrap();
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
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("root call {call_id}: {error}"))
        .expect("scripted root Haskell operation settles")
}

async fn captured_host_scenario(scenario: HostedScenario) {
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
                run: runtime_namespace(&config.run_directory.path()),
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
    let check_shutdown_parked = matches!(
        scenario,
        HostedScenario::Captured(
            CapturedScenario::CancelWhileParked
                | CapturedScenario::ShutdownAfterChildFailure
                | CapturedScenario::CoordinatorFailureAfterChildFailure
        )
    );
    let forest_after_shutdown = Arc::clone(&host.context.forest);
    let runtime_after_shutdown = Arc::clone(&host.runtime);
    let observer_after_shutdown = host.context.observer.clone();
    let actor_after_shutdown = host.context.actor.clone();
    let owners_after_shutdown = Arc::clone(&host.context.owners);
    let transport_after_shutdown = prepared_transport.lock().clone().unwrap();
    let scenario_result = std::panic::AssertUnwindSafe(host.run_scenario(|host| {
        Box::pin(async move {
            host.input("Exercise the captured checkpoint scenario.")
                .await
                .unwrap();
            let transport = prepared_transport
                .lock()
                .take()
                .expect("scripted Responses transport");
            let campaign = host.context.clone();
            if matches!(scenario, HostedScenario::Captured(
                CapturedScenario::ShutdownAfterChildFailure | CapturedScenario::CoordinatorFailureAfterChildFailure
            )) {
                assert!(campaign.config.command_resources.is_none(),
                    "the command refusal control requires a host without command resource authority");
            }
            let actor = campaign.actor.identity();
            let runtime = Arc::clone(&host.runtime);
            let address = host.address;
            let api = format!("http://{address}/api");
            let origin = format!("https://{address}");
            let root_origin = transport.root_origin.clone();
            assert_eq!(
                root_origin,
                ConversationIdentity::Embedded {
                    run: runtime_namespace(&campaign.config.run_directory.path()),
                    actor: AgentPath("/root".into()),
                    incarnation: actor.incarnation.0.to_string(),
                }
            );
            let scheduler = runtime.scheduler();
            let mut setup_settlements = transport.setup_settlements.lock().take().unwrap();
            campaign
                .while_root_live(
                    "scope setup provider call",
                    tokio::time::timeout(
                        Duration::from_secs(60),
                        transport.scope_setup_issued.notified(),
                    ),
                )
                .await
                .unwrap_or_else(|error| panic!("{error}"))
                .expect("root provider did not issue its original scope setup operation");
            let setup_operation = transport.operation(&root_origin, "captured-scope-setup");
            let setup = campaign
                .while_root_live(
                    "scope setup operation settlement",
                    tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
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
                    }),
                )
                .await
                .unwrap_or_else(|error| panic!("{error}"))
                .expect("original scope setup operation did not settle within its cold budget");
            assert_committed_haskell_value(&setup, "True");
            campaign
                .while_root_live(
                    "provider turn after scope setup",
                    tokio::time::timeout(
                        Duration::from_secs(60),
                        transport.setup_requested.notified(),
                    ),
                )
                .await
                .unwrap_or_else(|error| panic!("{error}"))
                .expect("root Engine did not request a turn after scope setup settled");
            let parked_baseline = if scenario
                == HostedScenario::Captured(CapturedScenario::CancelWhileParked)
            {
                let graph = campaign.forest.inspect_host_graph();
                assert_eq!(
                    graph.len(),
                    3,
                    "setup owns one root and two record services"
                );
                for label in ["embedded-checkpoint-seeds", "embedded-checkpoint-groups"] {
                    let record = graph.iter().find(|node| node.label == label).unwrap();
                    assert!(!record.model_actor);
                    assert!(record.terminal.is_none());
                    assert_eq!(record.creator, Some(actor));
                    assert_eq!(
                        campaign.forest.actor_session(record.actor),
                        campaign.forest.actor_session(actor)
                    );
                }
                // These persistent receive loops share the root machine and survive
                // cancellation of an independent workbench invocation.
                let parked = campaign
                    .forest
                    .measurement_snapshot()
                    .and_then(|snapshot| snapshot.parked)
                    .expect("quiescent setup retains a fresh shared-machine snapshot");
                assert_eq!(parked, 3, "root and both record receive loops are parked");
                eprintln!("[captured-engine] persistent root and record-service baseline: {parked} parked");
                Some(parked)
            } else {
                None
            };
            let scenario = match scenario {
                HostedScenario::Captured(scenario) => scenario,
                HostedScenario::LocalActorStartup => {
                    let mut settlements = scheduler.operation_settlements();
                    transport.setup_ready.notify_one();
                    let operation = campaign
                        .while_root_live(
                            "started local actor calls settle",
                            tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                                loop {
                                    let settled = settlements
                                        .recv()
                                        .await
                                        .expect("local call settlement observer");
                                    if settled.origin == root_origin && settled.call.0 == LOCAL_STARTUP_CALL
                                    {
                                        break settled;
                                    }
                                }
                            }),
                        )
                        .await
                        .unwrap_or_else(|error| panic!("{error}"))
                        .expect("both started local actors answer their original typed calls");
                    assert_eq!(
                        operation,
                        transport.operation(&root_origin, LOCAL_STARTUP_CALL)
                    );
                    let result =
                        embedded_operation(&runtime, &operation, COLD_DEBUG_CELL_SETTLEMENT_BUDGET)
                            .await
                            .expect("local actor reads return their original state");
                    assert_committed_haskell_value(&result, "True");
                    received_output(&transport, &operation).await;
                    let graph = campaign.forest.inspect_host_graph();
                    for label in ["embedded-checkpoint-seeds", "embedded-checkpoint-groups"] {
                        let local = graph
                            .iter()
                            .find(|node| node.label == label)
                            .expect("started local actor is retained by its original kernel");
                        assert_eq!(local.creator, Some(actor));
                        assert_eq!(local.supervisor_parent, Some(actor));
                        assert!(!local.model_actor);
                        assert!(local.terminal.is_none());
                    }
                    assert!(transport.children.lock().is_empty());
                    finish_root(&transport, &campaign).await;
                    return;
                }
            };
            // Subscribe before issuing the parent call: settlement may precede observation,
            // and Scheduler::wait refuses operations that are not admitted yet.
            let mut parent_settlements = scheduler.operation_settlements();

            transport.setup_ready.notify_one();
            let mut sessions = std::collections::HashSet::new();
            campaign.while_root_live(
                "initial captured child provider requests",
                tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                    for _ in 0..2 {
                        let request = {
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
                        }.expect("captured child provider observation closed before both requests");
                        transport
                            .parent_prefix
                            .lock()
                            .as_ref()
                            .expect("original request prefix captured")
                            .assert_inherited(&request);
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
                }),
            )
            .await
            .unwrap_or_else(|error| panic!("{error}"))
            .expect("both captured children did not start within their shared preparation budget while the parent call was pending");
            assert_eq!(sessions.len(), 2);
            eprintln!(
                "[captured-engine] two child provider branches ready while exact parent claim is pending"
            );
            if matches!(scenario, CapturedScenario::ShutdownAfterChildFailure | CapturedScenario::CoordinatorFailureAfterChildFailure) {
                transport.reply_children.send_replace(true);
                let mut failed = None;
                tokio::time::timeout(TYPED_CHILD_REPLY_SETTLEMENT_BUDGET, async {
                    loop {
                        let operation = parent_settlements.recv().await.unwrap();
                        if sessions.contains(&operation.origin) {
                            let output = scheduler.wait(&operation).await.unwrap();
                            let JobOutput::Completed(Err(failure)) = output else {
                                panic!("first child must retain its intentional failure: {output:?}");
                            };
                            assert!(failure.to_string().contains("HOSTED_INTENTIONAL_CHILD_FAILURE"), "{failure}");
                            failed = Some(operation);
                            break;
                        }
                    }
                }).await.expect("intentional child failure settles");
                let failed = failed.unwrap();
                let active_origin = sessions.iter().find(|origin| **origin != failed.origin).unwrap();
                let active_key = (active_origin.clone(), format!("captured-child-{}", active_origin.actor().0));
                let active = tokio::time::timeout(Duration::from_secs(30), async {
                    let mut observations = tokio::time::interval(Duration::from_millis(20));
                    loop {
                        if let Some(operation) = transport.operations.lock().get(&active_key).cloned() {
                            break operation;
                        }
                        observations.tick().await;
                    }
                }).await.expect("other exact provider operation is issued");
                let active_actor = campaign.observer.installations().into_iter()
                    .find(|installation| campaign.binding(installation.actor.identity())
                        .is_some_and(|binding| embedded_harness::original_operation(binding.identity(), &active).is_ok())).unwrap().actor;
                let target = harness::embedding::HostIdentity {
                    run: runtime_namespace(&campaign.config.run_directory.path()),
                    actor: active_origin.actor().clone(),
                    incarnation: active_actor.identity().incarnation.0.to_string(),
                };
                let native = exomonad_tool::ToolInvocationContext {
                    origin: exomonad_tool::ToolInvocationOrigin::Model(
                        embedded_harness::original_operation(&target, &active).unwrap(),
                    ),
                    call_id: active.call.0.clone(),
                    namespace: None,
                };
                tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                    let mut observations = tokio::time::interval(Duration::from_millis(20));
                    while active_actor.hosted_workbench_waiting(&native).is_none() {
                        assert!(runtime.store().claims_for_operation(&active).unwrap().iter()
                            .any(|claim| claim.state == harness::store::ClaimState::Pending));
                        observations.tick().await;
                    }
                }).await.expect("the other exact native call parks before host teardown");
                let graph = campaign.forest.inspect_host_graph();
                let gate = graph.iter().find(|node| node.label == "embedded-shutdown-park-gate")
                    .expect("the real owned gate actor is installed");
                assert!(gate.terminal.is_none(), "gate must remain live before host teardown: {gate:?}");
                assert!(!gate.model_actor, "the gate is a record actor, not a provider branch");
                assert_eq!(gate.creator, Some(actor), "the root owns the parked gate");
                eprintln!("[captured-engine] exact sibling parks on owned actor gate {:?}", gate.actor);
                let parent = transport.operation(&root_origin, PENDING_CALL);
                let parent_identity = harness::embedding::HostIdentity {
                    run: runtime_namespace(&campaign.config.run_directory.path()),
                    actor: root_origin.actor().clone(),
                    incarnation: actor.incarnation.0.to_string(),
                };
                let parent_native = exomonad_tool::ToolInvocationContext {
                    origin: exomonad_tool::ToolInvocationOrigin::Model(
                        embedded_harness::original_operation(&parent_identity, &parent).unwrap(),
                    ),
                    call_id: parent.call.0.clone(),
                    namespace: None,
                };
                tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
                    let mut observations = tokio::time::interval(Duration::from_millis(20));
                    loop {
                        assert!(runtime.store().claims_for_operation(&parent).unwrap().iter()
                            .any(|claim| claim.state == harness::store::ClaimState::Pending),
                            "the original parent must remain pending until native shutdown");
                        assert!(runtime.store().claims_for_operation(&active).unwrap().iter()
                            .any(|claim| claim.state == harness::store::ClaimState::Pending),
                            "the owned gate must retain the active sibling while the parent waits");
                        if campaign.actor.hosted_workbench_waiting(&parent_native).is_some() {
                            break;
                        }
                        observations.tick().await;
                    }
                }).await.expect("the exact parent parks on both child settlements before teardown");
                eprintln!("[captured-engine] exact parent parks on child settlement join before teardown");
                assert!(matches!(scheduler.wait(&failed).await.unwrap(), JobOutput::Completed(Err(_))));
                if scenario == CapturedScenario::CoordinatorFailureAfterChildFailure {
                    campaign.observer.fail_coordination(INTENTIONAL_COORDINATOR_FAILURE);
                    let failure = host.while_host_running(std::future::pending::<()>()).await.unwrap_err();
                    let super::hosted_test_context::HostBarrierFailure::CoordinationFailed { error, .. } = failure else {
                        panic!("expected exact coordinator failure before host result: {failure}");
                    };
                    assert_eq!(error, INTENTIONAL_COORDINATOR_FAILURE);
                    panic!("{INTENTIONAL_COORDINATOR_FAILURE}");
                }
                // Returning invokes production teardown with one failed child,
                // an active child awaiting the owned gate, and the parent's outstanding join.
                return;
            }
            if scenario == CapturedScenario::CancelWhileParked {
                let pending = transport.operation(&root_origin, PENDING_CALL);
                let target = harness::embedding::HostIdentity {
                    run: runtime_namespace(&campaign.config.run_directory.path()),
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
                campaign
                    .while_root_live(
                        "native cancellation owner admission",
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
                        }),
                    )
                    .await
                    .unwrap_or_else(|error| panic!("{error}"))
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
                    &campaign,
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
                let known_children = campaign
                    .observer
                    .installations()
                    .into_iter()
                    .filter(|installation| installation.checkpoint)
                    .map(|installation| installation.actor)
                    .collect::<Vec<_>>();
                assert_eq!(known_children.len(), 2);
                let graph = campaign.forest.inspect_host_graph();
                let mut child_requests = std::collections::HashSet::new();
                for child in &known_children {
                    assert_eq!(
                        campaign.forest.actor_session(child.identity()),
                        campaign.forest.actor_session(actor),
                        "captured child uses the original shared machine"
                    );
                    let node = graph
                        .iter()
                        .find(|node| node.actor == child.identity())
                        .expect("original captured child remains installed");
                    assert!(node.terminal.is_none());
                    assert_eq!(node.creator, Some(actor));
                    assert_eq!(node.workbench, exomonad_actor::ActorWorkbenchPosture::Idle);
                    assert_eq!(
                        node.active_requests.len(),
                        1,
                        "each child owns its original presented request: {node:?}"
                    );
                    assert!(node.queued_requests.is_empty(), "{node:?}");
                    assert!(node
                        .provider_turn
                        .as_ref()
                        .is_some_and(|turn| turn.state == exomonad_model::ProviderTurnState::Active));
                    assert!(child_requests.insert(node.active_requests[0]));
                }
                let baseline = parked_baseline.expect("cancellation retains its persistent baseline");
                // A typed child request runs inside a mailbox handler. SuspendedCast
                // retains the agentLoop receiver, while ResidentInteractiveAwait owns
                // the separate handler continuation waiting for the model's reply.
                let child_receivers = known_children.len();
                let child_handlers = child_requests.len();
                assert_eq!(
                    campaign
                        .forest
                        .measurement_snapshot()
                        .and_then(|snapshot| snapshot.parked),
                    Some(baseline + child_receivers + child_handlers + 1),
                    "child receivers, presented request handlers and the original parent await are parked"
                );
                eprintln!(
                    "[captured-engine] parked owners: persistent {baseline}, child receivers {child_receivers}, child requests {child_requests:?}, original parent await 1"
                );
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
                    Some(baseline),
                    "interrupt consumes the invocation continuations and preserves persistent receive loops"
                );
                let graph = campaign.forest.inspect_host_graph();
                for child in &known_children {
                    let node = graph
                        .iter()
                        .find(|node| node.actor == child.identity())
                        .expect("original child retains its terminal observation");
                    assert!(node.terminal.is_some());
                    assert!(node.active_requests.is_empty(), "{node:?}");
                    assert!(node.queued_requests.is_empty(), "{node:?}");
                }
                for label in ["embedded-checkpoint-seeds", "embedded-checkpoint-groups"] {
                    assert!(graph
                        .iter()
                        .find(|node| node.label == label)
                        .expect("persistent record service remains installed")
                        .terminal
                        .is_none());
                }
                eprintln!(
                    "[captured-engine] cancellation restored {baseline} persistent parked receive loops"
                );
                assert!(
                    campaign.actor.terminal().get().is_none(),
                    "interrupt must preserve the root actor"
                );
                let waiting = root_browser_projection(
                    &campaign,
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
                    &campaign,
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
                    &campaign,
                    &transport,
                    "captured-cancelled-prefix-absent",
                    "display (capturedValue :: Int)",
                )
                .await
                .unwrap();
                assert_absent_binding(&absent, "capturedValue");
                let cleaned = root_tool_call(
                    &campaign,
                    &transport,
                    "captured-interrupted-group-cleanup",
                    &tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_captured_group_cleanup.hs"),
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
                return;
            }
            if scenario == CapturedScenario::ConcurrentNominalJoin {
                let published_b = root_tool_call(
                    &campaign,
                    &transport,
                    "nominal-join-root-b",
                    &tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_nominal_join_b.hs"),
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
                CapturedScenario::ShutdownAfterChildFailure | CapturedScenario::CoordinatorFailureAfterChildFailure => unreachable!("shutdown scenario returns before child replies"),
                CapturedScenario::Success | CapturedScenario::CancelWhileParked => {
                    assert_committed_haskell_value(&result.unwrap(), "True");
                    received_output(&transport, &transport.operation(&root_origin, PENDING_CALL)).await;
                    let public = root_tool_call(
                        &campaign,
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
                        &campaign,
                        &transport,
                        "nominal-join-final-read",
                        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_nominal_join_final.hs"),
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
                    // provider_tool_error starts metadata with the serialized
                    // FailureEnvelope itself, then adds publication and items.
                    assert_eq!(metadata["class"], "runtime", "{metadata}");
                    assert_eq!(metadata["phase"], "run", "{metadata}");
                    assert!(failure.message().contains(INTENTIONAL_PARENT_FAILURE));
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
                        &campaign,
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
                            &campaign,
                            &transport,
                            &format!("captured-parent-absent-{name}"),
                            source,
                        )
                        .await
                        .expect("missing failed-cell name returns a compile rejection");
                        assert_absent_binding(&absent, name);
                    }
                    let rebound = root_tool_call(
                        &campaign,
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
                        &campaign,
                        &transport,
                        REUSE_CALL,
                        &tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_captured_unfold_reuse_after_failure.hs"),
                    )
                    .await
                    .expect("the failed cell's transferred capture admits and joins a third child");
                    let third_request = requests_rx
                        .try_recv()
                        .expect("third child provider request");
                    transport
                        .parent_prefix
                        .lock()
                        .as_ref()
                        .expect("original request prefix survives parent failure")
                        .assert_inherited(&third_request);
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
                    &campaign,
                    &transport,
                    "captured-active-cleanup-refusal",
                    &tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_captured_group_cleanup.hs"),
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
                &campaign,
                &transport,
                "captured-group-cleanup",
                &tidepool_testing::fixture_source("bridge/facade/src/actor_host/embedded_captured_group_cleanup.hs"),
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
        })
    }))
    .catch_unwind()
    .await;
    if scenario == HostedScenario::Captured(CapturedScenario::CoordinatorFailureAfterChildFailure) {
        let payload =
            scenario_result.expect_err("coordinator scenario must retain its original marker");
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied());
        if message != Some(INTENTIONAL_COORDINATOR_FAILURE) {
            std::panic::resume_unwind(payload);
        }
        assert!(owners_after_shutdown
            .lock()
            .values()
            .all(|owner| owner.native_retirement == NativeRetirement::Preserve));
    } else if let Err(payload) = scenario_result {
        std::panic::resume_unwind(payload);
    }
    if matches!(
        scenario,
        HostedScenario::Captured(
            CapturedScenario::ShutdownAfterChildFailure
                | CapturedScenario::CoordinatorFailureAfterChildFailure
        )
    ) {
        let scheduler = runtime_after_shutdown.scheduler();
        let operations = transport_after_shutdown
            .operations
            .lock()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut failed = 0;
        let mut cancelled = 0;
        for operation in operations {
            if operation.call.0 != PENDING_CALL && !operation.call.0.starts_with("captured-child-")
            {
                continue;
            }
            match scheduler.wait(&operation).await.unwrap() {
                JobOutput::Completed(Err(failure))
                    if failure
                        .to_string()
                        .contains("HOSTED_INTENTIONAL_CHILD_FAILURE") =>
                {
                    failed += 1
                }
                JobOutput::Cancelled | JobOutput::CancelledWithReceipt(_) => cancelled += 1,
                outcome => panic!(
                    "shutdown must retain exact terminal proof for {operation:?}: {outcome:?}"
                ),
            }
        }
        assert_eq!(
            failed, 1,
            "original child failure remains retained after teardown"
        );
        assert_eq!(
            cancelled, 2,
            "parent and active child obtain exact cancellation receipts"
        );
        assert!(actor_after_shutdown
            .terminal()
            .cleanup()
            .unwrap()
            .is_confirmed());
        for installation in observer_after_shutdown.installations() {
            assert!(installation
                .actor
                .terminal()
                .cleanup()
                .unwrap()
                .is_confirmed());
            assert_eq!(
                observed_resource_release(installation.actor.identity(), &owners_after_shutdown),
                Some(exomonad_actor::ResourceRelease::Released),
            );
        }
        let graph = forest_after_shutdown.inspect_host_graph();
        assert_eq!(
            graph
                .iter()
                .filter(|node| node.label == "embedded-shutdown-park-gate")
                .count(),
            1,
            "the owned gate retains its exact terminal cleanup observation"
        );
        assert!(graph.iter().all(|node| {
            node.terminal.is_some()
                && node.active_requests.is_empty()
                && node.queued_requests.is_empty()
        }));
    }
    if check_shutdown_parked {
        assert_eq!(
            forest_after_shutdown
                .measurement_snapshot()
                .and_then(|snapshot| snapshot.parked),
            Some(0),
            "full shutdown consumes the root and record-service receive loops"
        );
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

enum PreflightStep {
    Cell { call: String, source: String },
    InterruptStream,
    Finish,
}

struct PreflightTransport {
    runtime: Arc<embedded_harness::EmbeddedHarnessRuntime>,
    origin: ConversationIdentity,
    steps: tokio::sync::Mutex<mpsc::UnboundedReceiver<PreflightStep>>,
    sender: mpsc::UnboundedSender<PreflightStep>,
    operations: Mutex<HashMap<String, OperationId>>,
    requests: Mutex<Vec<ResponsesRequest>>,
    changed: watch::Sender<u64>,
}

#[async_trait]
impl ResponsesTransport for PreflightTransport {
    async fn create(&self, _: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        panic!("hosted preflight requires the Engine's exact request identity")
    }

    async fn create_streaming_for_request(
        &self,
        request_id: &RequestId,
        request: ResponsesRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        self.requests.lock().push(request);
        self.changed.send_modify(|revision| *revision += 1);
        let items = match self
            .steps
            .lock()
            .await
            .recv()
            .await
            .expect("preflight script")
        {
            PreflightStep::Cell { call, source } => {
                let operation = OperationId {
                    origin: self.origin.clone(),
                    request: request_id.clone(),
                    call: CallId(call.clone()),
                };
                assert!(self
                    .operations
                    .lock()
                    .insert(call.clone(), operation)
                    .is_none());
                self.changed.send_modify(|revision| *revision += 1);
                vec![haskell_call(&call, &source, CallMode::Blocking)]
            }
            PreflightStep::InterruptStream => {
                return Err(TransportError::IncompleteResponse(
                    harness::transport::StreamInterruption::MissingCompletion,
                ));
            }
            PreflightStep::Finish => vec![harness::item::Item(json!({
                "type":"message", "role":"assistant", "phase":"final_answer",
                "content":[{"type":"output_text","text":"preflight and recovery confirmed"}]
            }))],
        };
        for item in &items {
            sink.send(harness::transport::sse::StreamEvent::ItemDone(item.clone()))
                .await
                .map_err(|_| TransportError::Stream("preflight provider receiver closed".into()))?;
        }
        Ok(ResponsesTurn {
            response_id: format!("preflight-{}", request_id.0),
            items,
            usage: Usage::default(),
        })
    }
}

impl PreflightTransport {
    async fn cell(&self, call: &str, source: &str) -> (OperationId, Value) {
        let mut changed = self.changed.subscribe();
        self.sender
            .send(PreflightStep::Cell {
                call: call.into(),
                source: source.into(),
            })
            .unwrap();
        let operation = tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
            loop {
                if let Some(operation) = self.operations.lock().get(call).cloned() {
                    break operation;
                }
                changed.changed().await.unwrap();
            }
        })
        .await
        .expect("actual provider issued preflight cell");
        let output = tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
            loop {
                if let Ok(Some(output)) = self.runtime.scheduler().output(&operation).await {
                    break output;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("real preflight cell settled");
        durable_output(&self.runtime, &operation, &output).await;
        let JobOutput::Completed(Ok(receipt)) = output else {
            panic!("expected actual Haskell receipt: {output:?}");
        };
        let durable = self
            .runtime
            .store()
            .replay_output_operation(&operation)
            .unwrap()
            .unwrap();
        tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
            loop {
                if self
                    .requests
                    .lock()
                    .iter()
                    .any(|request| request.input.contains(&durable))
                {
                    break;
                }
                changed.changed().await.unwrap();
            }
        })
        .await
        .expect("real provider received exact durable preflight output");
        (operation, receipt)
    }
}

#[tokio::test]
async fn admitted_cell_late_type_error_has_no_effect_or_publication_on_retry() {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret_file = files.path().join("session-secret");
    std::fs::write(&secret_file, "whole-cell-preflight-secret-is-long-enough").unwrap();
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
    let slot = Arc::new(Mutex::new(None));
    let prepared = slot.clone();
    let host = super::hosted_test_context::HostedTestRuntime::start_with_factory(
        &settings,
        |config| {
            let authored = config.workspace.join(".exomonad");
            std::fs::create_dir_all(&authored).unwrap();
            std::fs::write(
                authored.join("AgentSpec.hs"),
                tidepool_testing::fixture_source(
                    "bridge/facade/src/actor_host/embedded_bad_final_agent_spec.hs",
                ),
            )
            .unwrap();
            crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
                project.haskell.source_roots = vec![".".into()];
                project.haskell.modules = vec!["AgentSpec".into()];
                project.haskell.spec = Some("AgentSpec.agentSpec".into());
            });
            test_campaign::commit_workspace(&config.workspace);
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::load(
                    &config.workspace,
                    &config.run_directory.path(),
                )
                .unwrap(),
            );
        },
        move |runtime, config| {
            let (sender, steps) = mpsc::unbounded_channel();
            let transport = Arc::new(PreflightTransport {
                runtime: runtime.clone(),
                origin: ConversationIdentity::Embedded {
                    run: runtime_namespace(&config.run_directory.path()),
                    actor: AgentPath("/root".into()),
                    incarnation: exomonad_actor::Incarnation::FIRST.0.to_string(),
                },
                steps: tokio::sync::Mutex::new(steps),
                sender,
                operations: Mutex::new(HashMap::new()),
                requests: Mutex::new(Vec::new()),
                changed: watch::channel(0).0,
            });
            *prepared.lock() = Some(transport.clone());
            transport
        },
    )
    .await
    .expect("production hosted preflight starts");
    host.run_scenario(|host| {
        Box::pin(async move {
            host.input("Exercise whole-cell preflight admission.")
                .await
                .unwrap();
            let transport = slot.lock().take().unwrap();
            let actor = host.context.actor.identity();
            let binding = host
                .context
                .while_root_live(
                    "preflight root binding",
                    tokio::time::timeout(Duration::from_secs(30), async {
                        loop {
                            if let Some(binding) = host.context.binding(actor) {
                                break binding;
                            }
                            tokio::task::yield_now().await;
                        }
                    }),
                )
                .await
                .unwrap_or_else(|error| panic!("{error}"))
                .expect("actual attached root binding");
            assert_eq!(
                binding.inbox.watermark(),
                0,
                "ordinary user input must not publish an actor notification"
            );
            let source = tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/embedded_bad_final_cell.hs",
            )
            .replace("TARGET_ID", &actor.id.0.to_string())
            .replace("TARGET_INCARNATION", &actor.incarnation.0.to_string());

            // Prove this installed tool, effect route and target before a negative check.
            let control_source = source
                .replace("neverPublished", "publishedControl")
                .replace("pure (True :: Int)", "display (42 :: Int)");
            let (_, control) = host
                .context
                .while_root_live(
                    "preflight positive control",
                    transport.cell("preflight-positive-control", &control_source),
                )
                .await
                .unwrap_or_else(|error| panic!("{error}"));
            assert_committed_haskell_value(&control, "42");
            assert_eq!(binding.inbox.watermark(), 1);
            assert!(
                transport
                    .requests
                    .lock()
                    .last()
                    .unwrap()
                    .input
                    .iter()
                    .any(|item| { item.0["content"] == "whole-cell-preflight-sentinel" }),
                "positive control notification must reach the actual provider"
            );
            assert!(host.runtime.store().unread("/root").unwrap().is_empty());

            let (operation, rejected) = host
                .context
                .while_root_live(
                    "preflight type rejection",
                    transport.cell("bad-final-call", &source),
                )
                .await
                .unwrap_or_else(|error| panic!("{error}"));
            assert_preflight_rejection(&rejected);
            assert_eq!(
                rejected["publication"]["status"], "notPublished",
                "{rejected}"
            );
            assert_eq!(rejected["publication"]["reason"], "rejected", "{rejected}");
            super::test_campaign::require_ghc_compile_rejection(&rejected, &["Bool", "Int"])
                .unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(
                binding.inbox.watermark(),
                1,
                "type rejection must execute no notification"
            );
            let durable = host
                .runtime
                .store()
                .replay_output_operation(&operation)
                .unwrap()
                .unwrap();
            let claims = host
                .runtime
                .store()
                .claims_for_operation(&operation)
                .unwrap();
            assert!(!claims.is_empty());
            assert!(claims
                .iter()
                .all(|claim| claim.state == harness::store::ClaimState::Settled));

            // The interrupted stream is real Engine input. Explicit host input recovers
            // its retained lineage without dispatching the settled Haskell call again.
            transport
                .sender
                .send(PreflightStep::InterruptStream)
                .unwrap();
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap();
            let api = format!("http://{}/api", host.address);
            let origin = format!("https://{}", host.address);
            let login = client
                .post(format!("{api}/session"))
                .header("Origin", &origin)
                .json(&json!({"secret":"whole-cell-preflight-secret-is-long-enough"}))
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
            let target = harness::embedding::HostIdentity {
                run: runtime_namespace(&host.context.config.run_directory.path()),
                actor: AgentPath("/root".into()),
                incarnation: actor.incarnation.0.to_string(),
            };
            let waiting = root_browser_projection(
                &host.context,
                host.address,
                &cookie,
                &target,
                harness::server::HostActorLifecycle::Waiting,
            )
            .await;
            assert!(waiting.active_round.is_none());
            assert!(
                host.runtime.store().unread("/root").unwrap().is_empty(),
                "the interrupted round must await explicit input"
            );
            let requests_before = transport.requests.lock().len();
            let compiler_before = tidepool_extract_cmd::extract_spawn_count();
            let mut changed = transport.changed.subscribe();
            let wake = client
                .post(format!("{api}/commands"))
                .header("Origin", &origin)
                .header(reqwest::header::COOKIE, &cookie)
                .json(&harness::server::ClientCommand::Host {
                    operation_id: harness::embedding::ClientOperationId(uuid::Uuid::new_v4()),
                    command: harness::server::HostCommand::Input {
                        target: target.clone(),
                        text: "retry retained preflight rejection".into(),
                    },
                })
                .send()
                .await
                .unwrap();
            assert_eq!(wake.status(), reqwest::StatusCode::ACCEPTED);
            let recovered = host
                .context
                .while_root_live(
                    "preflight recovery provider request",
                    tokio::time::timeout(Duration::from_secs(30), async {
                        loop {
                            if let Some(request) =
                                transport.requests.lock().get(requests_before).cloned()
                            {
                                break request;
                            }
                            changed.changed().await.unwrap();
                        }
                    }),
                )
                .await
                .unwrap_or_else(|error| panic!("{error}"))
                .expect("real recovery provider request");
            assert!(recovered
                .input
                .iter()
                .any(|item| { item.0["content"] == "retry retained preflight rejection" }));
            let returned = recovered
                .input
                .iter()
                .filter(|item| {
                    item.0["type"] == "custom_tool_call_output"
                        && item.0["call_id"] == operation.call.0
                })
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(
                returned,
                vec![durable.clone()],
                "recovery returns the original receipt exactly once"
            );
            assert_eq!(
                tidepool_extract_cmd::extract_spawn_count(),
                compiler_before,
                "recovery of a settled rejection must not resubmit compiler work"
            );
            assert_eq!(
                host.runtime
                    .store()
                    .replay_output_operation(&operation)
                    .unwrap(),
                Some(durable)
            );
            assert_eq!(
                host.runtime
                    .store()
                    .claims_for_operation(&operation)
                    .unwrap()
                    .len(),
                claims.len()
            );
            assert_eq!(
                transport
                    .operations
                    .lock()
                    .keys()
                    .filter(|call| call.as_str() == "bad-final-call")
                    .count(),
                1
            );
            assert_eq!(binding.inbox.watermark(), 1);

            let (_, absent) = host
                .context
                .while_root_live(
                    "preflight absent binding",
                    transport.cell(
                        "preflight-binding-absent",
                        "display (neverPublished :: Int)",
                    ),
                )
                .await
                .unwrap_or_else(|error| panic!("{error}"));
            assert_absent_binding(&absent, "neverPublished");
            let (_, valid) = host
                .context
                .while_root_live(
                    "preflight valid cell after recovery",
                    transport.cell(
                        "preflight-valid-after-retry",
                        &source.replace("pure (True :: Int)", "display (42 :: Int)"),
                    ),
                )
                .await
                .unwrap_or_else(|error| panic!("{error}"));
            assert_committed_haskell_value(&valid, "42");
            assert!(
                valid["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item["installedBindings"]
                        .as_array()
                        .is_some_and(|names| names.iter().any(|name| name == "neverPublished"))),
                "valid control publishes the binding: {valid}"
            );
            assert_eq!(
                binding.inbox.watermark(),
                2,
                "only valid cells send notifications"
            );
            assert!(host.context.actor.terminal().get().is_none());
            transport.sender.send(PreflightStep::Finish).unwrap();
            host.context
                .while_root_live(
                    "successful preflight provider round",
                    successful_rounds(&host.context, &[actor]),
                )
                .await
                .unwrap_or_else(|error| panic!("{error}"));
        })
    })
    .await;
}
