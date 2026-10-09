use super::super::{command_test_support::TestCommands, test_campaign::TestCampaign};
use super::*;
use harness::{
    item::{Item, ToolKind},
    model::{CallId, RequestId},
    store::TerminalOutcome,
    turn::JobOutput,
};
use std::time::Duration;

struct HeldDispatch {
    dispatcher: Arc<EmbeddedDispatcher>,
    dispatched: AtomicU64,
    dropped: Arc<AtomicBool>,
}

struct WaiterDrop(Arc<AtomicBool>);
impl Drop for WaiterDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl Provider for HeldDispatch {
    fn tools(&self) -> Vec<Value> {
        self.dispatcher.tools()
    }

    fn cancellation_owner(&self) -> Option<Arc<dyn CancellationOwner>> {
        Some(self.dispatcher.clone())
    }

    async fn call(&self, _: &str, _: Value) -> Result<Value, ProviderError> {
        Err(ProviderError::Tool(
            "fixture requires exact custom context".into(),
        ))
    }

    async fn call_custom_with_context(
        &self,
        name: &str,
        input: String,
        context: CallContext,
    ) -> Result<Value, ProviderError> {
        let _drop = WaiterDrop(self.dropped.clone());
        self.dispatched.fetch_add(1, Ordering::SeqCst);
        let reply = self
            .dispatcher
            .call_custom_with_context(name, input, context)
            .await;
        // The actual actor reply is ready, but its scheduler waiter cannot win
        // settlement before the independent cancellation owner returns it.
        std::future::pending::<()>().await;
        reply
    }
}

#[tokio::test]
async fn cancelled_hosted_cell_delivers_performed_prefix_once_before_waiter_abort() {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(|campaign| {
            Box::pin(async move {
                let identity = HostIdentity {
                    run: "cancelled-prefix-run".into(),
                    actor: AgentPath("/root".into()),
                    incarnation: campaign.actor.identity().incarnation.0.to_string(),
                };
                let request = RequestId("cancelled-prefix-request".into());
                let operation = OperationId {
                    origin: ConversationIdentity::Embedded {
                        run: identity.run.clone(),
                        actor: identity.actor.clone(),
                        incarnation: identity.incarnation.clone(),
                    },
                    request: request.clone(),
                    call: CallId("cancelled-prefix-cell".into()),
                };
                let store = Arc::new(Store::memory().unwrap());
                let installation =
                    EmbeddedPolicyInstallation::from_installation(&campaign.root_installation);
                let dispatcher = Arc::new(EmbeddedDispatcher {
                    identity: identity.clone(),
                    issuer: campaign.actor.identity(),
                    snapshot: Arc::new(installation.request_snapshot().unwrap()),
                    store: store.clone(),
                    context_models: Arc::new(OnceLock::new()),
                });
                let provider = Arc::new(HeldDispatch {
                    dispatcher,
                    dispatched: AtomicU64::new(0),
                    dropped: Arc::new(AtomicBool::new(false)),
                });
                let scheduler = JobScheduler::new(1).unwrap();
                scheduler
                    .start_operation(
                        provider.clone(),
                        operation.clone(),
                        identity.actor.clone(),
                        Some(request),
                        "haskell".into(),
                        harness::item::ToolInput::Custom(tidepool_testing::fixture_source(
                            "bridge/facade/src/actor_host/embedded_cancelled_prefix.hs",
                        )),
                    )
                    .await
                    .unwrap();
                let backend = TestCommands::completed("cancellation-prefix");
                loop {
                    let request = campaign
                        .next_deployment(
                            "prefix command backend",
                            Duration::from_secs(120),
                            |event| match event {
                                exomonad_actor::LocalResidentDeployment::CommandBackend(
                                    request,
                                ) => Ok(request),
                                other => Err(other),
                            },
                        )
                        .await;
                    match request.purpose {
                        exomonad_actor::command_jobs::CommandBackendPurpose::Command => {
                            request.supply(Ok(backend.clone()));
                            break;
                        }
                        exomonad_actor::command_jobs::CommandBackendPurpose::SourceProbe => {
                            request.supply(Ok(TestCommands::completed(
                                "/work/tree\n0123456789abcdef0123456789abcdef01234567\nclean\n",
                            )));
                        }
                    }
                }
                let context = ToolInvocationContext {
                    origin: exomonad_tool::ToolInvocationOrigin::Model(
                        original_operation(&identity, &operation).unwrap(),
                    ),
                    call_id: operation.call.0.clone(),
                    namespace: None,
                };
                tokio::time::timeout(Duration::from_secs(60), async {
                    loop {
                        if campaign.actor.hosted_workbench_waiting(&context).is_some() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("the performed prefix must reach its next native sleep");
                assert_eq!(backend.executions(), 1);
                let settlement = scheduler
                    .cancel(&operation)
                    .await
                    .unwrap()
                    .expect("cancellation owner wins before held waiter");
                assert_eq!(settlement.operation, operation);
                let JobOutput::CancelledWithReceipt(receipt) = &settlement.output else {
                    panic!(
                        "confirmed cancellation must retain its owner receipt: {:?}",
                        settlement.output
                    );
                };
                let failure = receipt
                    .as_ref()
                    .expect_err("native sleep interruption retains the partial failure receipt");
                assert!(failure.message().len() <= TOOL_ERROR_MESSAGE_BYTE_BUDGET);
                assert!(failure.message().contains("receipt output omitted"));
                let metadata = failure.metadata().expect("bounded native failure evidence");
                assert_eq!(metadata["originalOperation"], json!(operation));
                assert!(metadata["items"].as_array().unwrap().iter().any(|item| {
                    item["operations"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|operation| operation["disposition"] == "committed")
                }));
                // Inspect the actual native owner after projection, rather than
                // reconstruct complete evidence from the bounded tool output.
                let owner = campaign
                    .root_installation
                    .policy
                    .retained_operation(context.clone())
                    .expect("issued native settlement remains inspectable");
                let native_terminal = owner.terminal();
                let exomonad_actor::HostedOperationTerminal::Settled(
                    WorkbenchCancellationOutcome::Cancelled {
                        reply: Err(native_failure),
                        ..
                    },
                ) = &native_terminal
                else {
                    panic!("full cancelled native receipt required: {native_terminal:?}");
                };
                assert!(
                    native_failure
                        .receipts()
                        .iter()
                        .any(|receipt| !receipt.output.is_empty())
                );
                assert!(
                    native_failure
                        .receipts()
                        .iter()
                        .flat_map(|receipt| &receipt.operations)
                        .any(|operation| {
                            operation.disposition
                            == tidepool_runtime::session::WorkbenchOperationDisposition::Committed
                        })
                );
                assert!(
                    !failure.message().contains("unreachable suffix"),
                    "the suffix must not execute: {failure}"
                );
                let item = Item::tool_output(&operation.call, ToolKind::Custom, &settlement.output);
                assert_eq!(item.0["type"], "custom_tool_call_output");
                assert_eq!(item.0["call_id"], operation.call.0);
                let output: Value =
                    serde_json::from_str(item.0["output"].as_str().unwrap()).unwrap();
                assert_eq!(output["error"], "job cancelled");
                assert_eq!(
                    output["receipt"]["failure"]["originalOperation"],
                    json!(operation)
                );
                assert!(matches!(
                    TerminalOutcome::from(&settlement.output),
                    TerminalOutcome::CancelledWithReceipt(_)
                ));
                assert_eq!(scheduler.wait(&operation).await.unwrap(), settlement.output);
                assert_eq!(
                    scheduler.provider_completion(&operation).await.unwrap(),
                    None
                );
                assert!(provider.dropped.load(Ordering::SeqCst));
                assert_eq!(scheduler.cancel(&operation).await.unwrap(), None);
                assert_eq!(owner.terminal(), native_terminal);
                assert_eq!(
                    campaign
                        .root_installation
                        .policy
                        .retained_operation(context)
                        .unwrap()
                        .terminal(),
                    native_terminal
                );
                assert_eq!(provider.dispatched.load(Ordering::SeqCst), 1);
                assert_eq!(
                    backend.executions(),
                    1,
                    "cancellation receipt inspection must not replay the prefix"
                );
            })
        })
        .await;
}
