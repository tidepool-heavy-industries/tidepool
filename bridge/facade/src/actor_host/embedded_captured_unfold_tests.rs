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

// Semantic acceptance includes cold whole-cell compilation, validation and
// native attachment in a debug build. Performance gates keep their own budgets.
const COLD_DEBUG_CELL_SETTLEMENT_BUDGET: Duration = Duration::from_secs(300);
const TYPED_CHILD_REPLY_SETTLEMENT_BUDGET: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, PartialEq, Eq)]
enum CapturedScenario {
    Success,
    FailureAfterReplies,
}

struct ChildRounds {
    ordinal: usize,
    round: usize,
}

struct CapturedHostTransport {
    runtime: Arc<embedded_harness::EmbeddedHarnessRuntime>,
    scenario: CapturedScenario,
    root_origin: ConversationIdentity,
    root_round: AtomicUsize,
    operations: Mutex<HashMap<(ConversationIdentity, String), OperationId>>,
    scope_setup_issued: Notify,
    setup_requested: Notify,
    setup_ready: Notify,
    finish_parent: Notify,
    children: Mutex<HashMap<String, ChildRounds>>,
    parent_failed: watch::Sender<bool>,
    reply_children: watch::Sender<bool>,
    after_failure_reads: Notify,
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
                        CapturedScenario::Success => include_str!("embedded_later_failure_scope_setup.hs"),
                        CapturedScenario::FailureAfterReplies => include_str!("embedded_checkpoint_scope_setup.hs"),
                    }
                }))],
                2 => {
                    self.setup_requested.notify_one();
                    self.setup_ready.notified().await;
                    vec![harness::item::Item(json!({
                        "type":"custom_tool_call", "call_id":PENDING_CALL, "name":"haskell",
                        "input":match self.scenario {
                            CapturedScenario::Success => include_str!("embedded_captured_unfold_and_await.hs"),
                            CapturedScenario::FailureAfterReplies => include_str!("embedded_captured_unfold_await_then_fail.hs"),
                        }
                    }))]
                }
                3 if self.scenario == CapturedScenario::FailureAfterReplies => {
                    self.after_failure_reads.notified().await;
                    vec![harness::item::Item(json!({
                        "type":"custom_tool_call", "call_id":REUSE_CALL, "name":"haskell",
                        "input":include_str!("embedded_captured_unfold_reuse_after_failure.hs")
                    }))]
                }
                3 | 4
                    if (round == 3 && self.scenario == CapturedScenario::Success)
                        || (round == 4
                            && self.scenario == CapturedScenario::FailureAfterReplies) =>
                {
                    self.finish_parent.notified().await;
                    vec![harness::item::Item(json!({
                        "type":"message", "role":"assistant", "phase":"final_answer",
                        "content":[{"type":"output_text","text":"same-cell child replies received"}]
                    }))]
                }
                other => panic!("unexpected captured parent request {other}"),
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
            if round == 2 {
                let input = serde_json::to_value(&request.input).unwrap();
                let items = input.as_array().unwrap();
                let call_id = format!("captured-child-{path}");
                let returned: Vec<_> = items
                    .iter()
                    .filter(|item| {
                        item["type"] == "custom_tool_call_output" && item["call_id"] == call_id
                    })
                    .collect();
                assert_eq!(
                    returned.len(),
                    1,
                    "child follow-up must return exactly its original synchronous Haskell call"
                );
                let receipt: Value = serde_json::from_str(
                    returned[0]["output"]
                        .as_str()
                        .expect("returned Haskell receipt is encoded as JSON text"),
                )
                .expect("returned Haskell receipt is valid JSON");
                assert_eq!(receipt["status"], "replied", "{receipt}");
                assert_eq!(
                    receipt["items"].as_array().map(Vec::len),
                    Some(1),
                    "{receipt}"
                );
                assert_eq!(receipt["items"][0]["status"], "committed", "{receipt}");
                assert_eq!(receipt["publication"]["status"], "published", "{receipt}");
                assert_eq!(
                    receipt["items"][0]["terminalTransfer"], "replyAccepted",
                    "{receipt}"
                );
                assert!(
                    receipt["items"][0]["operations"]
                        .as_array()
                        .is_some_and(|operations| operations.iter().any(|operation| {
                            operation["effect"] == "reply"
                                && operation["disposition"] == "committed"
                        })),
                    "typed reply effect was not committed: {receipt}"
                );
            }
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
                        "name":"haskell", "input":"respond capturedGetter"
                    }))]
                }
                2 if self.scenario == CapturedScenario::FailureAfterReplies && ordinal < 2 => {
                    let mut failed = self.parent_failed.subscribe();
                    while !*failed.borrow_and_update() {
                        failed.changed().await.unwrap();
                    }
                    vec![harness::item::Item(json!({
                        "type":"custom_tool_call", "call_id":format!("captured-child-after-failure-{path}"),
                        "name":"haskell", "input":"(capturedValue, capturedGetter)"
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
            let _ = sink
                .send(harness::transport::sse::StreamEvent::ItemDone(item.clone()))
                .await;
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
    let item = response["items"]
        .as_array()
        .expect("WorkbenchResponse.items must be an array")
        .last()
        .expect("Haskell operation must have a result item");
    assert_eq!(item["status"], committed_item, "{response}");
    assert_eq!(item["output"], expected, "{response}");
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
    match tokio::time::timeout(
        settlement_budget,
        runtime.scheduler().wait(&claim.operation),
    )
    .await
    .unwrap_or_else(|_| {
        panic!("embedded Haskell operation {call_id} did not settle within {settlement_budget:?}")
    })
    .unwrap()
    {
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
    let mut campaign = test_campaign::TestCampaign::start_with_config(
        exomonad_actor::ResearchPolicy::default(),
        |admission| admission,
        |config| {
            config.backend = crate::exomonad::HostBackendOptions::Embedded;
            config.embedded = Some(settings.clone());
            test_campaign::configure_notebook_lookup_workspace(config);
        },
    )
    .await;
    let actor = campaign.actor.identity();
    let mut service =
        embedded_service::EmbeddedService::prepare(&campaign.config.run_root, &settings)
            .await
            .unwrap();
    let runtime = Arc::clone(&service.runtime);
    let root_origin = ConversationIdentity::Embedded {
        run: runtime_namespace(&campaign.config.run_root),
        actor: AgentPath("/root".into()),
        incarnation: actor.incarnation.0.to_string(),
    };
    let (requests_tx, mut requests_rx) = mpsc::unbounded_channel();
    let (reads_tx, mut reads_rx) = mpsc::unbounded_channel();
    let transport = Arc::new(CapturedHostTransport {
        runtime: runtime.clone(),
        scenario,
        root_origin: root_origin.clone(),
        root_round: AtomicUsize::new(0),
        operations: Mutex::new(HashMap::new()),
        scope_setup_issued: Notify::new(),
        setup_requested: Notify::new(),
        setup_ready: Notify::new(),
        finish_parent: Notify::new(),
        children: Mutex::new(HashMap::new()),
        parent_failed: watch::channel(false).0,
        reply_children: watch::channel(false).0,
        after_failure_reads: Notify::new(),
        finish_children: watch::channel(false).0,
        reads: reads_tx,
        requests: requests_tx,
    });
    service.set_test_transport(transport.clone());
    let (lifecycle_tx, lifecycle_rx) = mpsc::channel(32);
    let mut installation = campaign.root_installation.clone();
    installation.initial_user_message = Some("start checkpoint fixture".into());
    lifecycle_tx
        .send(LocalResidentDeployment::PolicyInstalled(Box::new(
            installation,
        )))
        .await
        .unwrap();
    let mut deployments = campaign.take_deployments();
    let forward = tokio::spawn(async move {
        while let Some(deployment) = deployments.recv().await {
            if lifecycle_tx.send(deployment).await.is_err() {
                break;
            }
        }
    });
    let (readiness_tx, mut readiness_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = watch::channel(None);
    let (_config_tx, config_rx) = watch::channel(campaign.config.clone());
    let fleet = InteractiveFleet {
        provider_forest: Arc::clone(&campaign.forest),
        root: campaign.actor.clone(),
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
        host_graph: {
            let forest = campaign.forest.clone();
            Arc::new(move || forest.inspect_host_graph())
        },
    };
    let scheduler = runtime.scheduler();
    // The provider records its operation before Harness admits it. Observe
    // settlement from before launch rather than waiting on an unadmitted ID.
    let mut setup_settlements = scheduler.operation_settlements();
    let host = tokio::spawn(run_interactive_applications(
        lifecycle_rx,
        Arc::new(Mutex::new(HashMap::new())),
        fleet,
        shutdown_rx,
        config_rx,
        Some(service),
    ));
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(30), readiness_rx.recv())
            .await
            .expect("production embedded host did not become ready"),
        Some(ActorHostReadiness::EmbeddedReady { .. })
    ));

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
                assert_eq!(reply["status"], "replied", "{reply}");
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
        CapturedScenario::Success => assert_committed_haskell_value(&result.unwrap(), "True"),
        CapturedScenario::FailureAfterReplies => {
            let failure = result.expect_err(
                "the original unfinished parent cell must fail after its child replies",
            );
            assert!(
                failure.contains("intentional captured parent Haskell execution failure"),
                "{failure}"
            );
            assert!(
                campaign.actor.terminal().get().is_none(),
                "the fixture fails a cell while its parent actor remains live"
            );
            eprintln!(
                "[captured-engine] actual parent execution failed after both typed child replies"
            );
            // Capturing a completed prefix retains it for children without
            // publishing the failed cell's private names into its parent.
            let policy = campaign.root_installation.policy.clone();
            let public_names = tokio::time::timeout(
                COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
                policy.dispatch_boxed(preflight_invocation(
                    "captured-parent-public-probe",
                    "captured-parent-public-probe",
                    "(x, getX)".into(),
                )),
            )
            .await
            .expect("earlier parent public names remain readable within the cell budget")
            .expect("earlier parent public names remain installed");
            assert_committed_haskell_value(&public_names, "(41, 42)");
            for name in ["capturedValue", "capturedGetter"] {
                let private_name = tokio::time::timeout(
                    COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
                    policy.dispatch_boxed(preflight_invocation(
                        &format!("captured-parent-private-probe-{name}"),
                        &format!("captured-parent-private-probe-{name}"),
                        format!("({name} :: Int)"),
                    )),
                )
                .await
                .expect("failed-cell private name probe settles within the cell budget")
                .expect("a missing private name returns a compile rejection");
                assert_preflight_rejection(&private_name);
                assert!(
                    private_name["items"].as_array().unwrap().iter().any(|item| {
                        item["diagnostics"].as_array().is_some_and(|diagnostics| {
                            diagnostics.iter().any(|diagnostic| {
                                diagnostic["severity"] == "error"
                                    && diagnostic["message"].as_str().is_some_and(|message| {
                                        message.contains(name)
                                            && message.to_ascii_lowercase().contains("not in scope")
                                    })
                            })
                        })
                    }),
                    "failed cell leaked {name}, or its probe failed for an unrelated reason: {private_name}"
                );
            }
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
                assert_committed_haskell_value(&value, "(41, 42)");
                retained.insert(origin);
            }
            assert_eq!(retained.len(), 2);
            transport.after_failure_reads.notify_one();
            tokio::time::timeout(Duration::from_secs(120), requests_rx.recv())
                .await
                .expect("the failed cell's retained capture did not admit another child")
                .unwrap();
            let reused = embedded_operation(
                &runtime,
                &transport.operation(&root_origin, REUSE_CALL),
                COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
            )
            .await
            .unwrap();
            assert_committed_haskell_value(&reused, "True");
            eprintln!(
                "[captured-engine] retained children and independently reused capture succeeded"
            );
            transport.finish_children.send_replace(true);
        }
    }
    let expected_children = if scenario == CapturedScenario::Success {
        2
    } else {
        3
    };
    assert_eq!(
        transport.children.lock().len(),
        expected_children,
        "refused branches must not launch children"
    );
    transport.finish_parent.notify_one();
    shutdown_tx.send_replace(Some(NativeRetirement::Terminate));
    let result = tokio::time::timeout(Duration::from_secs(30), host)
        .await
        .expect("production host did not terminate")
        .expect("production host task panicked");
    assert!(result.is_ok(), "production host cleanup failed: {result:?}");
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    forward.abort();
    if let Err(error) = forward.await {
        assert!(error.is_cancelled());
    }
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
        result = policy.dispatch_boxed(invocation) => result.expect("actual admitted Haskell invocation settles"),
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
            config.backend = crate::exomonad::HostBackendOptions::Embedded;
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
        .dispatch_boxed(exomonad_tool::ToolInvocation {
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

    let valid = source.replace("pure (True :: Int)", "pure (42 :: Int)");
    let mut control = tokio::spawn(async move {
        policy
            .dispatch_boxed(preflight_invocation(
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
    let committed = tokio::time::timeout(Duration::from_secs(120), control)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
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
