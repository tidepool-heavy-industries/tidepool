use super::super::test_campaign::{
    commit_workspace, committed_display_text, dispatch_haskell_script, TestCampaign,
};
use super::*;
use harness::{
    item::Item,
    transport::{ResponsesRequest, ResponsesTurn, TransportError, Usage},
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex,
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
async fn displayed(
    campaign: &mut TestCampaign,
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    source: &str,
) -> serde_json::Value {
    let store = super::super::display_output::open_run_store(campaign.session_root.path()).unwrap();
    campaign
        .drive_actor_output(&store, dispatch_haskell_script(endpoint, source))
        .await
}

fn same_terminal_reply(
    owner: &exomonad_actor::KernelWorkbenchReply,
    dispatch: &Result<exomonad_actor::ResidentToolResponse, exomonad_actor::ResidentToolError>,
) -> bool {
    use exomonad_actor::{ResidentToolError, ResidentToolResponse};
    match (owner, dispatch) {
        (Ok(expected), Ok(ResidentToolResponse::Workbench(actual))) => expected == actual,
        (Ok(expected), Ok(ResidentToolResponse::Value(actual))) => {
            serde_json::to_value(expected).unwrap() == *actual
        }
        (Err(expected), Err(ResidentToolError::Invocation(actual))) => expected == actual,
        _ => false,
    }
}

#[test]
fn cancellation_terminal_comparison_refuses_mismatched_owner_results() {
    use exomonad_actor::{
        ActorId, ActorRef, KernelInvocationFailure, ResidentToolError, ResidentToolResponse,
    };
    use tidepool_runtime::session::{WorkbenchResponse, WorkbenchRunStatus};
    let response = WorkbenchResponse {
        status: WorkbenchRunStatus::Committed,
        summary: None,
        items: Vec::new(),
        next_index: 1,
        total: 1,
        publication: None,
    };
    let failure = KernelInvocationFailure::Cancelled {
        actor: ActorRef::first(ActorId(42)),
    };
    let successful_owner = Ok(response.clone());
    let failed_owner = Err(failure.clone());
    let typed = Ok(ResidentToolResponse::Workbench(response.clone()));
    let projected = Ok(ResidentToolResponse::Value(
        serde_json::to_value(&response).unwrap(),
    ));
    let failed = Err(ResidentToolError::Invocation(failure));
    assert!(same_terminal_reply(&successful_owner, &typed));
    assert!(same_terminal_reply(&successful_owner, &projected));
    assert!(same_terminal_reply(&failed_owner, &failed));
    assert!(!same_terminal_reply(&successful_owner, &failed));
    assert!(!same_terminal_reply(&failed_owner, &typed));
    let mut changed = response;
    changed.status = WorkbenchRunStatus::Completed;
    assert!(!same_terminal_reply(
        &successful_owner,
        &Ok(ResidentToolResponse::Workbench(changed)),
    ));
    assert!(!same_terminal_reply(
        &failed_owner,
        &Err(ResidentToolError::Invocation(
            KernelInvocationFailure::Cancelled {
                actor: ActorRef::first(ActorId(43)),
            }
        )),
    ));
}

#[tokio::test]
async fn resident_model_callback_and_hook_keep_caller_effects_and_retained_result() {
    let (campaign, store, script, _) = campaign().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let endpoint = campaign.root_installation.policy.clone();
                let capability = displayed(
                    campaign,
                    endpoint.as_ref(),
                    "display (case ModelFixture.modelCapabilityKeys of { [Tidepool.Effects.Core.EffectModelCall] -> True; _ -> False })",
                )
                .await;
                assert_eq!(committed_display_text(&capability), "True");
                let result = displayed(
                    campaign,
                    endpoint.as_ref(),
                    "modelCallbackResult <- ModelFixture.callbackCell\ndisplay (case fst modelCallbackResult of { Right answer -> answer == \"done\"; _ -> False })",
                )
                .await;
                assert_eq!(committed_display_text(&result), "True");
                let output_lines = result["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|item| item["output"].as_str().unwrap().lines())
                    .collect::<Vec<_>>();
                for expected in ["model callback", "model hook"] {
                    assert_eq!(
                        output_lines.iter().filter(|line| **line == expected).count(),
                        1,
                        "the real caller Console effect must run once: {result}"
                    );
                }
                let requests = script.requests.lock().unwrap();
                assert_eq!(requests.len(), 2);
                assert!(requests
                    .iter()
                    .all(|request| request.model == "test-model"
                        && request.pinned_effort == Effort::Low));
                drop(requests);
                let events = store.events(None).unwrap();
                let retained = events
                    .iter()
                    .filter(|event| event.kind == "model_tool_result")
                    .collect::<Vec<_>>();
                assert_eq!(retained.len(), 1);
                let payload: serde_json::Value =
                    serde_json::from_str(&retained[0].payload).unwrap();
                assert_eq!(payload["output"]["type"], "function_call_output");
                assert!(payload["output"]["output"]
                    .as_str()
                    .unwrap()
                    .contains("hello"));
            })
        })
        .await;
}
#[tokio::test]
async fn resident_nested_model_and_separate_cell_budget_use_original_execution_owner() {
    let (campaign, _, script, _) = campaign().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let endpoint = campaign.root_installation.policy.clone();
                let nested = displayed(
                    campaign,
                    endpoint.as_ref(),
                    "import qualified Tidepool.Model as ModelApi\nnestedModelOutcome <- ModelFixture.nestedCell\ndisplay (case nestedModelOutcome of { Right answer -> answer == \"done\"; _ -> False })",
                )
                .await;
                assert_eq!(committed_display_text(&nested), "True");
                assert_eq!(script.requests.lock().unwrap().len(), 3);
                let limited = displayed(
                    campaign,
                    endpoint.as_ref(),
                    "limitedNestedModelOutcome <- ModelFixture.nestedBudgetCell\ndisplay (case limitedNestedModelOutcome of { Left (ModelApi.ModelBudgetExceeded ModelApi.ProviderRequests) -> True; _ -> False })",
                )
                .await;
                assert_eq!(committed_display_text(&limited), "True");
                assert_eq!(
                    script.requests.lock().unwrap().len(),
                    5,
                    "outer invocation exhaustion latches the shared cell allowance"
                );
                for _ in 0..2 {
                    let receipt = displayed(
                        campaign,
                        endpoint.as_ref(),
                        "modelBudgetResult <- ModelFixture.budgetCell\ndisplay (case modelBudgetResult of { (16, Left (ModelApi.ModelBudgetExceeded ModelApi.ProviderRequests)) -> True; _ -> False })",
                    )
                    .await;
                    assert_eq!(committed_display_text(&receipt), "True");
                }
                assert_eq!(
                    script.requests.lock().unwrap().len(),
                    37,
                    "second cell receives its own sixteen-request host allowance"
                );
            })
        })
        .await;
}
#[tokio::test]
async fn resident_parked_model_allows_another_cell_and_cancels_without_late_provider_work() {
    use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};
    let (campaign, store, script, retained) = campaign().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                script.hold_nested.store(true, Ordering::Release);
                let endpoint = campaign.root_installation.policy.clone();
                let context = ToolInvocationContext::external(
                    "model-test".into(),
                    "parked-turn".into(),
                    "parked-call".into(),
                    Some("parked-call".into()),
                    None,
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
                    displayed(campaign, endpoint.as_ref(), "display (42 :: Int)"),
                )
                .await
                .unwrap();
                assert_eq!(committed_display_text(&receipt), "42");
                assert!(!task.is_finished(), "the original dispatch remains pending");
                assert!(campaign.actor.hosted_workbench_waiting(&context).is_some());
                assert_eq!(script.dropped.load(Ordering::SeqCst), 0);
                let cancelled = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    endpoint.cancel_workbench_boxed(context),
                )
                .await
                .unwrap()
                .unwrap();
                let exomonad_actor::WorkbenchCancellationOutcome::Cancelled {
                    execution,
                    reply: owner_reply,
                } = cancelled
                else {
                    panic!("the actual parked model cell must cancel: {cancelled:?}");
                };
                let native_items = match &owner_reply {
                    Ok(response) => response.items.as_slice(),
                    Err(failure) => failure.receipts(),
                };
                assert!(native_items
                    .iter()
                    .flat_map(|item| &item.operations)
                    .all(|operation| operation.id.execution == execution));
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
                assert!(receipts
                    .iter()
                    .all(|receipt| receipt["outcome"]["kind"] == "cancelled"));
                let actual = tokio::time::timeout(std::time::Duration::from_secs(30), task)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    same_terminal_reply(&owner_reply, &actual),
                    "dispatch must return the exact cancellation owner's terminal reply: owner={owner_reply:?}, dispatch={actual:?}"
                );
                script.release.notify_waiters();
                assert_eq!(script.requests.lock().unwrap().len(), 2);
                assert_eq!(script.dropped.load(Ordering::SeqCst), 1);
            })
        })
        .await;
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
    assert!(factory
        .policy(&descriptor.with_model(Some(Model::Alias("not-admitted".into()))))
        .is_err());
}
