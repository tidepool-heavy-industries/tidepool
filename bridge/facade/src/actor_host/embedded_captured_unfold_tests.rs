use super::*;
use async_trait::async_trait;
use harness::{
    engine::ResponsesTransport,
    model::{AgentPath, CallId, ConversationIdentity},
    transport::{ResponsesRequest, ResponsesTurn, TransportError, Usage},
    turn::JobOutput,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

const PENDING_CALL: &str = "captured-unfold-and-await";
const REUSE_CALL: &str = "reuse-failed-cell-capture";

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
    setup_requested: Notify,
    setup_ready: Notify,
    finish_parent: Notify,
    children: Mutex<HashMap<String, ChildRounds>>,
    parent_failed: watch::Sender<bool>,
    after_failure_reads: Notify,
    finish_children: watch::Sender<bool>,
    reads: mpsc::UnboundedSender<(ConversationIdentity, String)>,
    requests: mpsc::UnboundedSender<ResponsesRequest>,
}

#[async_trait]
impl ResponsesTransport for CapturedHostTransport {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
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
                    let claim = claims
                        .iter()
                        .find(|claim| {
                            claim.operation.origin == self.root_origin
                                && claim.request == claim.operation.request
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
                        assert!(original.iter().any(|claim| claim.operation.origin == self.root_origin
                            && claim.request == claim.operation.request && claim.state == harness::store::ClaimState::Settled),
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
                    vec![harness::item::Item(json!({
                        "type":"custom_tool_call", "call_id":format!("captured-child-{path}"),
                        "name":"haskell", "input":"respond getX"
                    }))]
                }
                2 if self.scenario == CapturedScenario::FailureAfterReplies && ordinal < 2 => {
                    let mut failed = self.parent_failed.subscribe();
                    while !*failed.borrow_and_update() {
                        failed.changed().await.unwrap();
                    }
                    vec![harness::item::Item(json!({
                        "type":"custom_tool_call", "call_id":format!("captured-child-after-failure-{path}"),
                        "name":"haskell", "input":"(x, getX)"
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
        Ok(ResponsesTurn {
            response_id,
            items,
            usage: Usage::default(),
        })
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
    origin: &ConversationIdentity,
    call_id: &str,
) -> Result<Value, String> {
    let call = CallId(call_id.to_owned());
    let claim = runtime
        .store()
        .claims(&call)
        .unwrap()
        .into_iter()
        .find(|claim| &claim.operation.origin == origin)
        .unwrap_or_else(|| panic!("embedded Haskell operation {call_id} was not admitted"));
    match tokio::time::timeout(
        Duration::from_secs(90),
        runtime.scheduler().wait(&claim.operation),
    )
    .await
    .unwrap_or_else(|_| panic!("embedded Haskell operation {call_id} did not settle"))
    .unwrap()
    {
        JobOutput::Completed(result) => result,
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
        asset_root: assets,
        session_secret_file: secret_file,
        codex_auth_file: auth_file,
        context_capacity_tokens: 200_000,
        concurrent_jobs: 3,
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
        setup_requested: Notify::new(),
        setup_ready: Notify::new(),
        finish_parent: Notify::new(),
        children: Mutex::new(HashMap::new()),
        parent_failed: watch::channel(false).0,
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
        transport.setup_requested.notified(),
    )
    .await
    .expect("root Engine did not request a turn after scope setup");
    let setup = embedded_operation(&runtime, &root_origin, "captured-scope-setup")
        .await
        .unwrap();
    assert_committed_haskell_value(&setup, "True");
    transport.setup_ready.notify_one();
    let mut sessions = std::collections::HashSet::new();
    for _ in 0..2 {
        let request = tokio::time::timeout(Duration::from_secs(120), requests_rx.recv())
            .await
            .expect("captured children did not start while the parent call was pending")
            .unwrap();
        sessions.insert(request.session_id);
    }
    assert_eq!(sessions.len(), 2);
    let result = embedded_operation(&runtime, &root_origin, PENDING_CALL).await;
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
                let value = embedded_operation(&runtime, &origin, &call).await.unwrap();
                assert_committed_haskell_value(&value, "(41, 42)");
                retained.insert(origin);
            }
            assert_eq!(retained.len(), 2);
            transport.after_failure_reads.notify_one();
            tokio::time::timeout(Duration::from_secs(120), requests_rx.recv())
                .await
                .expect("the failed cell's retained capture did not admit another child")
                .unwrap();
            let reused = embedded_operation(&runtime, &root_origin, REUSE_CALL)
                .await
                .unwrap();
            assert_committed_haskell_value(&reused, "True");
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
