use super::super::test_campaign::{TestCampaign, commit_workspace, dispatch_haskell_script};
use super::*;
use harness::{
    item::Item,
    transport::{ResponsesRequest, ResponsesTurn, TransportError, Usage},
};
use serde_json::json;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Notify;

struct Offline;
impl Auth for Offline {
    fn access(&self) -> Result<(String, String), TransportError> {
        panic!("offline fixture must not read credentials")
    }
}

#[derive(Default)]
struct ScriptState {
    requests: Mutex<Vec<ResponsesRequest>>,
    parked: Notify,
    release: Notify,
    dropped: AtomicUsize,
    hold_nested: AtomicBool,
}
#[derive(Clone)]
struct Script(Arc<ScriptState>);
struct ParkedRequest(Arc<ScriptState>);
impl Drop for ParkedRequest {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
fn turn(items: Vec<Item>) -> ResponsesTurn {
    ResponsesTurn {
        response_id: uuid::Uuid::new_v4().to_string(),
        items,
        usage: Usage {
            reported: true,
            input_tokens: 1,
            output_tokens: 1,
            ..Usage::default()
        },
    }
}
fn final_turn(answer: &str) -> ResponsesTurn {
    turn(vec![Item(
        json!({"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":answer}]}),
    )])
}
#[async_trait::async_trait]
impl ResponsesTransport for Script {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        let instructions = request.instructions.clone();
        let resumed = request
            .input
            .iter()
            .any(|item| item.0["type"] == "function_call_output");
        self.0.requests.lock().unwrap().push(request);
        if instructions == "parked"
            || (instructions == "nested" && self.0.hold_nested.load(Ordering::Acquire))
        {
            let _owner = ParkedRequest(self.0.clone());
            self.0.parked.notify_one();
            self.0.release.notified().await;
            return Ok(final_turn("released"));
        }
        Ok(match (instructions.as_str(), resumed) {
            ("callback" | "nested-outer", false) => turn(vec![Item(
                json!({"type":"function_call","call_id":"echo-1","name":"echo","arguments":"{\"message\":\"hello\"}"}),
            )]),
            ("nested", _) => final_turn("nested answer"),
            ("callback" | "nested-outer", _) => final_turn("done"),
            ("budget", _) => final_turn("allowed"),
            _ => panic!("unexpected fixture instructions {instructions}"),
        })
    }
}
fn configure(config: &mut ActorHostConfig) {
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(&authored).unwrap();
    std::fs::write(
        authored.join("ModelFixture.hs"),
        tidepool_testing::fixture_source("bridge/facade/src/actor_host/fixtures/cell_model.hs"),
    )
    .unwrap();
    crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
        project.haskell.source_roots = vec![".".into()];
        project.haskell.modules = vec!["ModelFixture".into()];
    });
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(
            &config.workspace,
            &config.run_directory.path(),
        )
        .unwrap(),
    );
}
type HeldBinding = Arc<Mutex<Option<Arc<dyn CellModelBinding>>>>;
struct HoldingFactory {
    inner: EmbeddedCellModelFactory<Offline, Script>,
    retained: HeldBinding,
}
impl CellModelFactory for HoldingFactory {
    fn bind(
        &self,
        execution: &WorkbenchExecutionId,
        principal: PrincipalId,
        descriptor: &ActorDescriptor,
    ) -> Arc<dyn CellModelBinding> {
        let binding = self.inner.bind(execution, principal, descriptor);
        let mut retained = self.retained.lock().unwrap();
        if retained.is_none() {
            *retained = Some(binding.clone());
        }
        binding
    }
}
async fn campaign() -> (TestCampaign, Arc<Store>, Arc<ScriptState>, HeldBinding) {
    let store = Arc::new(Store::memory().unwrap());
    let script = Arc::new(ScriptState::default());
    let transport = script.clone();
    let retained = Arc::new(Mutex::new(None));
    let factory = Arc::new(HoldingFactory {
        inner: EmbeddedCellModelFactory::<Offline, _>::new(
            store.clone(),
            Arc::new(JobScheduler::new(4).unwrap()),
            "test-model".into(),
            Effort::Low,
            Arc::new(move || Script(transport.clone())),
        ),
        retained: retained.clone(),
    });
    let campaign = TestCampaign::start_with_model_factory(
        |admission| admission,
        configure,
        None,
        Some(factory),
    )
    .await;
    (campaign, store, script, retained)
}
async fn stop(campaign: TestCampaign) {
    campaign.forest.shutdown().await;
    tokio::time::timeout(std::time::Duration::from_secs(30), campaign.hosted)
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn resident_model_callback_and_hook_keep_caller_effects_and_retained_result() {
    let (campaign, store, script, _) = campaign().await;
    let capability = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        "pure ModelFixture.modelCapabilityKeys",
    )
    .await;
    assert_eq!(
        capability["items"][0]["output"].as_str().unwrap().trim(),
        "[EffectModelCall]"
    );
    let result = dispatch_haskell_script(
        campaign.root_installation.policy.as_ref(),
        "ModelFixture.callbackCell",
    )
    .await;
    let output = result["items"][0]["output"].as_str().unwrap();
    assert!(output.contains("Right \"done\""), "{result}");
    assert!(
        output.contains("model callback") && output.contains("model hook"),
        "{result}"
    );
    let requests = script.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.model == "test-model" && request.pinned_effort == Effort::Low)
    );
    drop(requests);
    let events = store.events(None).unwrap();
    let retained = events
        .iter()
        .filter(|event| event.kind == "model_tool_result")
        .collect::<Vec<_>>();
    assert_eq!(retained.len(), 1);
    let payload: serde_json::Value = serde_json::from_str(&retained[0].payload).unwrap();
    assert_eq!(payload["output"]["type"], "function_call_output");
    assert!(
        payload["output"]["output"]
            .as_str()
            .unwrap()
            .contains("hello")
    );
    stop(campaign).await;
}
#[tokio::test]
async fn resident_nested_model_and_separate_cell_budget_use_original_execution_owner() {
    let (campaign, _, script, _) = campaign().await;
    let endpoint = campaign.root_installation.policy.as_ref();
    let nested = dispatch_haskell_script(endpoint, "ModelFixture.nestedCell").await;
    assert!(
        nested["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("Right \"done\""),
        "{nested}"
    );
    assert_eq!(script.requests.lock().unwrap().len(), 3);
    let limited = dispatch_haskell_script(endpoint, "ModelFixture.nestedBudgetCell").await;
    assert!(
        limited["items"][0]["output"]
            .as_str()
            .unwrap()
            .contains("ModelBudgetExceeded ProviderRequests"),
        "{limited}"
    );
    assert_eq!(
        script.requests.lock().unwrap().len(),
        5,
        "outer invocation exhaustion latches the shared cell allowance"
    );
    for _ in 0..2 {
        let receipt = dispatch_haskell_script(endpoint, "ModelFixture.budgetCell").await;
        let output = receipt["items"][0]["output"].as_str().unwrap();
        assert!(
            output.contains("(16,") && output.contains("ModelBudgetExceeded ProviderRequests"),
            "{receipt}"
        );
    }
    assert_eq!(
        script.requests.lock().unwrap().len(),
        37,
        "second cell receives its own sixteen-request host allowance"
    );
    stop(campaign).await;
}
#[tokio::test]
async fn resident_parked_model_allows_another_cell_and_cancels_without_late_provider_work() {
    use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};
    let (campaign, store, script, retained) = campaign().await;
    script.hold_nested.store(true, Ordering::Release);
    let endpoint = campaign.root_installation.policy.clone();
    let context = ToolInvocationContext::external(
        "model-test".into(),
        "parked-turn".into(),
        "parked-call".into(),
        Some("parked-operation".into()),
        Some("haskell".into()),
    );
    let task = tokio::spawn(endpoint.dispatch_boxed(ToolInvocation {
        name: exomonad_actor::HASKELL_TOOL.into(),
        arguments: ToolArguments::Raw("ModelFixture.nestedCell".into()),
        context: Some(context.clone()),
    }));
    tokio::time::timeout(
        std::time::Duration::from_secs(180),
        script.parked.notified(),
    )
    .await
    .unwrap();
    let receipt = tokio::time::timeout(
        std::time::Duration::from_secs(180),
        dispatch_haskell_script(endpoint.as_ref(), "pure (42 :: Int)"),
    )
    .await
    .unwrap();
    assert_eq!(receipt["items"][0]["output"].as_str().unwrap().trim(), "42");
    let cancelled = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        endpoint.cancel_workbench_boxed(context),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(
            cancelled,
            exomonad_actor::WorkbenchCancellationOutcome::Cancelled { .. }
        ),
        "{cancelled:?}"
    );
    let held = retained
        .lock()
        .unwrap()
        .clone()
        .expect("original binding retained");
    assert!(Arc::strong_count(&held) >= 2);
    let receipts = store
        .events(None)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "model_invocation_receipt")
        .map(|event| serde_json::from_str::<serde_json::Value>(&event.payload).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        receipts.len(),
        2,
        "outer callback and nested model must settle before cancellation reply"
    );
    assert!(
        receipts
            .iter()
            .all(|receipt| receipt["outcome"]["kind"] == "cancelled")
    );
    let _actual = tokio::time::timeout(std::time::Duration::from_secs(30), task)
        .await
        .unwrap()
        .unwrap();
    script.release.notify_waiters();
    assert_eq!(script.requests.lock().unwrap().len(), 2);
    assert_eq!(script.dropped.load(Ordering::SeqCst), 1);
    stop(campaign).await;
}

#[tokio::test]
async fn cell_model_policy_matches_admitted_embedded_alias_and_effort() {
    use exomonad_actor::{ActorPlacement, Model};
    let storage = tempfile::tempdir().unwrap();
    let workspace = serde_json::from_value(json!({
        "include": [], "modules": [], "prompts": {}, "models": {"fast":"admitted-model"},
        "files": {}, "config": "", "library_identity": ""
    }))
    .unwrap();
    let config = ActorHostConfig {
        systemd_slice: None,
        source_exclude: vec![],
        source_import: Default::default(),
        command_resources: None,
        exomonad_executable: Default::default(),
        workspace_inputs: Some(workspace),
        workspace: Default::default(),
        haskell_root: Default::default(),
        run_directory: tidepool_atomic_write::DirectoryAnchor::open_existing(storage.path())
            .unwrap(),
        root_binding_path: Default::default(),

        embedded: None,
        tmux_session: String::new(),
        model: "default-model".into(),
        effort: exomonad_actor::ForkEffort::Medium,

        pane_environment: Default::default(),
        jev: None,
    };
    let factory = EmbeddedCellModelFactory::<Offline, Script>::new(
        Arc::new(Store::memory().unwrap()),
        Arc::new(JobScheduler::new(1).unwrap()),
        config.model.clone(),
        Effort::Medium,
        Arc::new(|| panic!("policy does not start provider work")),
    )
    .with_launch_config(&config);
    let descriptor = ActorDescriptor::new(
        "policy-cell",
        ActorPlacement {
            session: tidepool_repr::SessionId(1),
            resource_scope: tidepool_codegen::suspension::RealmId(1),
            lexical_scope: tidepool_codegen::scope::ScopeId::ROOT,
        },
    )
    .with_model(Some(Model::Alias("fast".into())));
    let policy = factory.policy(&descriptor).unwrap();
    assert_eq!(
        policy.models,
        [super::super::resolve_model(&config, descriptor.model().unwrap()).unwrap()]
    );
    assert!(matches!(
        (
            descriptor.fork_effort().unwrap_or(config.effort),
            policy.default_effort
        ),
        (exomonad_actor::ForkEffort::Medium, Effort::Medium)
    ));
    assert_eq!(policy.efforts, [Effort::Medium]);
    let binding = factory.bind(
        &WorkbenchExecutionId::from_digest([1; 16]),
        PrincipalId::new(1, 1),
        &descriptor,
    );
    binding.cancel();
    binding.cancel();
    let override_policy = factory
        .policy(&descriptor.clone().with_fork_effort(Some(ForkEffort::High)))
        .unwrap();
    assert_eq!(override_policy.efforts, [Effort::High]);
    assert_eq!(
        (
            policy.limits.requests,
            policy.limits.tools,
            policy.limits.reported_tokens,
            policy.limits.seconds
        ),
        (16, 64, 128_000, 300)
    );
    assert!(
        factory
            .policy(&descriptor.with_model(Some(Model::Alias("not-admitted".into()))))
            .is_err()
    );
}
