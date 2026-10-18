use super::super::{command_test_support::TestCommands, test_campaign::TestCampaign};
use super::*;
use harness::{
    item::{Item, ToolKind},
    model::{CallId, RequestId},
    store::TerminalOutcome,
    turn::JobOutput,
};
use std::{sync::atomic::AtomicBool, time::Duration};

struct HeldDispatch {
    dispatcher: Arc<EmbeddedDispatcher>,
    dispatched: Arc<AtomicU64>,
    dropped: Arc<AtomicBool>,
    scenario_lifetime: watch::Receiver<()>,
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
        self.dispatcher.cancellation_owner()
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
        // Hold the ordinary waiter until the owner wins. Scenario unwinding
        // closes this observer so a task receiving actor cleanup can terminate.
        let _ = self.scenario_lifetime.clone().changed().await;
        reply
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CancellationOrder {
    OwnerFirst,
    FallbackFirst,
}

struct CancellationRace {
    order: CancellationOrder,
    abandon_dispatch: tokio::sync::Notify,
    native_entry: tokio::sync::Notify,
    fallback_finished: tokio::sync::Notify,
    native_causes: Mutex<Vec<exomonad_actor::WorkbenchCancellationCause>>,
    intents: Mutex<Vec<harness::provider::CancellationCause>>,
}

/// Fault only the dispatch observer after actual actor admission. The exact
/// endpoint, operation, completion owner and cancellation endpoint remain real.
#[derive(Clone)]
struct AbandonedDispatchEndpoint {
    inner: Arc<dyn exomonad_actor::ResidentToolEndpoint>,
    race: Arc<CancellationRace>,
}
impl exomonad_actor::ResidentToolEndpoint for AbandonedDispatchEndpoint {
    fn snapshot_for_request(
        &self,
    ) -> Result<Arc<dyn exomonad_actor::ResidentToolEndpoint>, ResidentToolError> {
        Ok(Arc::new(Self {
            inner: self.inner.snapshot_for_request()?,
            race: self.race.clone(),
        }))
    }
    fn tools(&self) -> &[exomonad_tool::HostedTool] {
        self.inner.tools()
    }
    fn instructions(&self) -> Option<&str> {
        self.inner.instructions()
    }
    fn request_instructions(&self) -> Option<&str> {
        self.inner.request_instructions()
    }
    fn request_generation(&self) -> Option<tidepool_runtime::session::NotebookGenerationId> {
        self.inner.request_generation()
    }
    fn retained_operation(
        &self,
        context: ToolInvocationContext,
    ) -> Result<exomonad_actor::HostedOperationSettlement, ResidentToolError> {
        self.inner.retained_operation(context)
    }
    fn dispatch_boxed(
        &self,
        invocation: exomonad_tool::ToolInvocation,
    ) -> exomonad_actor::ResidentToolDispatchFuture {
        self.inner.dispatch_boxed(invocation)
    }
    fn dispatch_validated_with_context_boxed(
        &self,
        invocation: exomonad_tool::ToolInvocation,
        arguments: Result<ToolArguments, ResidentToolError>,
        capture: Option<Arc<dyn HostedCheckpointCapture>>,
        context: Option<Arc<dyn exomonad_actor::HostedContextBinding>>,
    ) -> exomonad_actor::ResidentToolDispatchFuture {
        let actual = self
            .inner
            .dispatch_validated_with_context_boxed(invocation, arguments, capture, context);
        let race = self.race.clone();
        Box::pin(async move {
            tokio::select! {
                result = actual => result,
                _ = race.abandon_dispatch.notified() => Err(ResidentToolError::Unavailable("dispatch observer abandoned after native admission".into())),
            }
        })
    }
    fn cancel_workbench_boxed(
        &self,
        context: ToolInvocationContext,
        cause: exomonad_actor::WorkbenchCancellationCause,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<WorkbenchCancellationOutcome, ResidentToolError>,
                > + Send
                + 'static,
        >,
    > {
        let actual = self.inner.cancel_workbench_boxed(context, cause);
        let race = self.race.clone();
        Box::pin(async move {
            race.native_causes.lock().unwrap().push(cause);
            race.native_entry.notify_one();
            actual.await
        })
    }
    fn abort_boxed(
        &self,
        boundary: exomonad_actor::ContextCheckpointBoundary,
    ) -> exomonad_actor::ResidentToolFuture {
        self.inner.abort_boxed(boundary)
    }
    fn complete_boxed(
        &self,
        boundary: exomonad_actor::ContextCheckpointBoundary,
    ) -> exomonad_actor::ResidentToolFuture {
        self.inner.complete_boxed(boundary)
    }
}

struct OrderedCancellationOwner {
    actual: Arc<dyn CancellationOwner>,
    race: Arc<CancellationRace>,
}
#[async_trait::async_trait]
impl CancellationOwner for OrderedCancellationOwner {
    fn cancellation_wait(&self) -> harness::provider::CancellationWait {
        harness::provider::CancellationWait::OwnerSettlement
    }
    async fn cancel(
        &self,
        operation: &OperationId,
        handle: &JobHandle,
        cause: harness::provider::CancellationCause,
    ) -> CancellationAcknowledgment {
        self.race.intents.lock().unwrap().push(cause);
        if self.race.order == CancellationOrder::FallbackFirst {
            self.race.abandon_dispatch.notify_one();
            self.race.native_entry.notified().await;
            assert_eq!(
                self.race.native_causes.lock().unwrap().len(),
                1,
                "actual fallback reached native endpoint before owner forwarding"
            );
        }
        let actual = self.actual.cancel(operation, handle, cause);
        tokio::pin!(actual);
        let acknowledgment = if self.race.order == CancellationOrder::OwnerFirst {
            let first = futures_util::poll!(&mut actual);
            assert_eq!(
                self.race.native_causes.lock().unwrap().len(),
                1,
                "actual owner reached native endpoint before dispatch observer abandonment"
            );
            self.race.abandon_dispatch.notify_one();
            match first {
                std::task::Poll::Ready(ack) => ack,
                std::task::Poll::Pending => actual.await,
            }
        } else {
            actual.await
        };
        self.race.fallback_finished.notified().await;
        acknowledgment
    }
}

struct RaceDispatch {
    dispatcher: Arc<EmbeddedDispatcher>,
    owner: Arc<OrderedCancellationOwner>,
    dispatched: Arc<AtomicU64>,
    dropped: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl Provider for RaceDispatch {
    fn tools(&self) -> Vec<Value> {
        self.dispatcher.tools()
    }
    fn cancellation_owner(&self) -> Option<Arc<dyn CancellationOwner>> {
        Some(self.owner.clone())
    }
    async fn call(&self, _: &str, _: Value) -> Result<Value, ProviderError> {
        unreachable!("exact completion entry required")
    }
    async fn complete_call(
        &self,
        name: &str,
        input: harness::item::ToolInput,
        context: CallContext,
    ) -> ProviderCompletion {
        let _drop = WaiterDrop(self.dropped.clone());
        self.dispatched.fetch_add(1, Ordering::SeqCst);
        let completion = self.dispatcher.complete_call(name, input, context).await;
        self.owner.race.fallback_finished.notify_one();
        completion
    }
}

#[tokio::test]
async fn cancelled_hosted_cell_delivers_performed_prefix_once_before_waiter_abort() {
    cancelled_prefix_with_order(None).await;
}

#[tokio::test]
async fn actual_owner_first_completion_retains_cancellation_intent_and_native_receipt() {
    cancelled_prefix_with_order(Some(CancellationOrder::OwnerFirst)).await;
}

#[tokio::test]
async fn actual_fallback_first_completion_and_owner_forward_same_issued_intent() {
    cancelled_prefix_with_order(Some(CancellationOrder::FallbackFirst)).await;
}

async fn cancelled_prefix_with_order(order: Option<CancellationOrder>) {
    let campaign = TestCampaign::start().await;
    campaign
        .run_scenario(move |campaign| {
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
                let race = order.map(|order| {
                    Arc::new(CancellationRace {
                        order,
                        abandon_dispatch: Default::default(),
                        native_entry: Default::default(),
                        fallback_finished: Default::default(),
                        native_causes: Default::default(),
                        intents: Default::default(),
                    })
                });
                let mut request_installation = campaign.root_installation.clone();
                if let Some(race) = &race {
                    request_installation.policy = Arc::new(AbandonedDispatchEndpoint {
                        inner: request_installation.policy.clone(),
                        race: race.clone(),
                    });
                }
                let installation =
                    EmbeddedPolicyInstallation::from_installation(&request_installation);
                let dispatcher = Arc::new(EmbeddedDispatcher {
                    attachment: super::super::provider_attachment::ProviderAttachment::admit(
                        campaign.forest.clone(),
                        campaign.actor.identity(),
                    )
                    .await
                    .unwrap(),
                    identity: identity.clone(),
                    issuer: campaign.actor.identity(),
                    snapshot: Arc::new(installation.request_snapshot().unwrap()),
                    store: store.clone(),
                    inference_defaults: installation.inference_defaults(),
                    conversation_permission: installation.conversation_permission().cloned(),
                });
                let dispatched = Arc::new(AtomicU64::new(0));
                let dropped = Arc::new(AtomicBool::new(false));
                let (_scenario_lifetime, scenario_lifetime) = watch::channel(());
                let provider: Arc<dyn Provider> = if let Some(race) = &race {
                    assert!(
                        !dispatcher.snapshot.context_read_write("haskell").unwrap(),
                        "fixture must use the real read-only async tool declaration"
                    );
                    let actual = dispatcher.cancellation_owner().unwrap();
                    Arc::new(RaceDispatch {
                        owner: Arc::new(OrderedCancellationOwner {
                            actual,
                            race: race.clone(),
                        }),
                        dispatcher,
                        dispatched: dispatched.clone(),
                        dropped: dropped.clone(),
                    })
                } else {
                    Arc::new(HeldDispatch {
                        dispatcher,
                        dispatched: dispatched.clone(),
                        dropped: dropped.clone(),
                        scenario_lifetime,
                    })
                };
                let scheduler = JobScheduler::new(1).unwrap();
                scheduler
                    .start_operation(
                        provider.clone(),
                        operation.clone(),
                        identity.actor.clone(),
                        Some(request),
                        "haskell".into(),
                        harness::item::ToolInput::Custom(tidepool_testing::fixture_source(
                            "exomonad/host/src/actor_host/embedded_cancelled_prefix.hs",
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
                let cancel = async {
                    match order {
                        Some(_) => {
                            scheduler
                                .cancel_with_cause(
                                    &operation,
                                    harness::provider::CancellationCause::DeliveredInput,
                                )
                                .await
                        }
                        None => scheduler.cancel(&operation).await,
                    }
                };
                let settlement = tokio::time::timeout(Duration::from_secs(30), cancel)
                    .await
                    .expect("actual owner and completion observer settle in forced order")
                    .unwrap();
                let output = match settlement {
                    Some(settlement) => {
                        assert_eq!(settlement.operation, operation);
                        settlement.output
                    }
                    None if race.is_some() => scheduler.wait(&operation).await.unwrap(),
                    None => panic!("cancellation owner must win before the held legacy waiter"),
                };
                if let Some(race) = &race {
                    assert_eq!(
                        *race.intents.lock().unwrap(),
                        vec![harness::provider::CancellationCause::DeliveredInput]
                    );
                    let causes = race.native_causes.lock().unwrap();
                    assert!(!causes.is_empty());
                    assert!(causes.iter().all(|cause| *cause
                        == exomonad_actor::WorkbenchCancellationCause::DeliveredInput));
                    if order == Some(CancellationOrder::FallbackFirst) {
                        assert_eq!(
                            causes.len(),
                            2,
                            "actual fallback and owner both forwarded to retained native endpoint"
                        );
                    }
                }
                let JobOutput::CancelledWithReceipt(receipt) = &output else {
                    panic!(
                        "confirmed cancellation must retain its owner receipt: {:?}",
                        output
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
                        execution: native_execution,
                        reply: native_reply,
                    },
                ) = &native_terminal
                else {
                    panic!("full cancelled native receipt required: {native_terminal:?}");
                };
                let native_failure = native_reply
                    .as_ref()
                    .expect_err("native interruption retains its full failure receipt");
                assert!(native_failure
                    .receipts()
                    .iter()
                    .any(|receipt| !receipt.output.is_empty()));
                assert!(native_failure
                    .receipts()
                    .iter()
                    .flat_map(|receipt| &receipt.operations)
                    .any(|operation| {
                        operation.disposition
                            == tidepool_runtime::session::WorkbenchOperationDisposition::Committed
                    }));
                assert!(
                    !failure.message().contains("unreachable suffix"),
                    "the suffix must not execute: {failure}"
                );
                let item = Item::tool_output(&operation.call, ToolKind::Custom, &output);
                assert_eq!(item.0["type"], "custom_tool_call_output");
                assert_eq!(item.0["call_id"], operation.call.0);
                let rendered_output: Value =
                    serde_json::from_str(item.0["output"].as_str().unwrap()).unwrap();
                assert_eq!(rendered_output["error"], "job cancelled");
                assert_eq!(
                    rendered_output["receipt"]["failure"]["originalOperation"],
                    json!(operation)
                );
                assert!(matches!(
                    TerminalOutcome::from(&output),
                    TerminalOutcome::CancelledWithReceipt(_)
                        | TerminalOutcome::CancelledWithValueReceipt
                ));
                assert_eq!(scheduler.wait(&operation).await.unwrap(), output);
                if race.is_none() {
                    assert_eq!(
                        scheduler.provider_completion(&operation).await.unwrap(),
                        None
                    );
                } else if let Some(completion) =
                    scheduler.invocation_completion(&operation).await.unwrap()
                {
                    assert_eq!(completion.output, output);
                }
                assert!(dropped.load(Ordering::SeqCst));
                assert_eq!(scheduler.cancel(&operation).await.unwrap(), None);
                for observation in [
                    owner.terminal(),
                    campaign
                        .root_installation
                        .policy
                        .retained_operation(context)
                        .unwrap()
                        .terminal(),
                ] {
                    let exomonad_actor::HostedOperationTerminal::Settled(
                        WorkbenchCancellationOutcome::Cancelled { execution, reply },
                    ) = observation
                    else {
                        panic!("inspection lost the cancelled native receipt: {observation:?}");
                    };
                    assert_eq!(&execution, native_execution);
                    assert_eq!(&reply, native_reply);
                }
                assert_eq!(dispatched.load(Ordering::SeqCst), 1);
                assert_eq!(
                    backend.executions(),
                    1,
                    "cancellation receipt inspection must not replay the prefix"
                );
            })
        })
        .await;
}
struct DiagnosticFailureOffline;
impl harness::transport::Auth for DiagnosticFailureOffline {
    fn admission(
        &self,
    ) -> Result<harness::transport::NativeTransportAdmission, harness::transport::TransportError>
    {
        Err(harness::transport::TransportError::InvalidAdmission)
    }

    fn access(&self) -> Result<String, harness::transport::TransportError> {
        panic!("offline diagnostic transport cannot request credentials")
    }
}

#[derive(Clone)]
struct DiagnosticFailureTransport {
    requests: Arc<AtomicU64>,
    native_call: mpsc::UnboundedSender<RequestId>,
    fail: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl harness::engine::ResponsesTransport for DiagnosticFailureTransport {
    fn response_contract(
        &self,
    ) -> Result<
        (
            harness::transport::ResponsesRoute,
            harness::transport::ResponsesProtocol,
        ),
        harness::transport::TransportError,
    > {
        Ok((
            harness::transport::ResponsesRoute::ChatGptPlan,
            harness::transport::ResponsesProtocol::Standard,
        ))
    }
    async fn create(
        &self,
        request: harness::transport::PreparedProviderRequest,
    ) -> Result<harness::transport::ResponsesTurn, harness::transport::TransportError> {
        match self.requests.fetch_add(1, Ordering::SeqCst) {
            0 => {
                self.native_call.send(request.request_id().clone()).unwrap();
                Ok(harness::transport::ResponsesTurn {
                    response_id: "native-before-diagnostic".into(),
                    items: vec![Item(
                        json!({"type":"custom_tool_call", "call_id":"diagnostic-pending-cell", "name":"haskell", "input":tidepool_testing::fixture_source("exomonad/host/src/actor_host/embedded_cancelled_prefix.hs"), "async":true}),
                    )],
                    usage: Default::default(),
                })
            }
            1 => {
                self.fail.notified().await;
                Err(harness::transport::TransportError::ProviderStreamFailure {
                    event: harness::transport::ProviderStreamFailureEvent::Error,
                    diagnostic: Some(harness::transport::HttpDiagnostic {
                        code: Some("invalid_tools".into()),
                        message: Some("unsupported tool schema".into()),
                        ..Default::default()
                    }),
                })
            }
            other => panic!("failure history unexpectedly retried provider request {other}"),
        }
    }
}

#[tokio::test]
async fn provider_diagnostic_and_block_write_refusal_retain_native_cleanup_cause() {
    let campaign = TestCampaign::start().await;
    campaign.run_scenario(|campaign| Box::pin(async move {
        let actor = campaign.actor.identity();
        let race = Arc::new(CancellationRace {
            order: CancellationOrder::OwnerFirst,
            abandon_dispatch: Default::default(), native_entry: Default::default(), fallback_finished: Default::default(),
            native_causes: Default::default(), intents: Default::default(),
        });
        let mut installation = campaign.root_installation.clone();
        installation.policy = Arc::new(AbandonedDispatchEndpoint { inner: installation.policy.clone(), race: race.clone() });
        let files = tempfile::tempdir().unwrap();
        let secret = files.path().join("session-secret");
        std::fs::write(&secret, "diagnostic-history-secret-is-long-enough").unwrap();
        let auth = files.path().join("chatgpt-auth.json");
        std::fs::write(&auth, "{}").unwrap();
        let settings = crate::exomonad::EmbeddedLaunchConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https, public_origin: None,
            asset_root: super::super::test_campaign::hosted_browser_assets(),
            browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret, session_secret_file: Some(secret),
            provider: exomonad_actor::Backend::OpenAIPlan,
        inference_limits: Default::default(), credential_file: auth,
            context_capacity_tokens: 200_000, concurrent_jobs: 2,
        };
        let mut service = campaign.prepare_engine_component_service(&settings).await.unwrap();
        let embedded = super::super::embedded_service::attach_actor(
            &service, super::super::provider_attachment::ProviderAttachment::admit(campaign.forest.clone(), actor).await.unwrap(),
            campaign.session_root.path(), AgentPath("/root".into()), None, installation,
            Some("exercise diagnostic persistence and native cleanup".into()),
        ).await.unwrap();
        let conversation = embedded.conversation.clone();
        let identity = conversation.identity().clone();
        let sql = rusqlite::Connection::open(campaign.session_root.path().join("harness/store.sqlite")).unwrap();
        sql.execute_batch("CREATE TRIGGER diagnostic_block_fault BEFORE UPDATE ON embedded_inference WHEN json_extract(NEW.state,'$.state')='blocked' BEGIN SELECT RAISE(ABORT,'diagnostic block write refused'); END;").unwrap();
        // block_tx uses an upsert; cover its insertion branch independently too.
        sql.execute_batch("CREATE TRIGGER diagnostic_block_insert_fault BEFORE INSERT ON embedded_inference WHEN json_extract(NEW.state,'$.state')='blocked' BEGIN SELECT RAISE(ABORT,'diagnostic block write refused'); END;").unwrap();
        let (native_call, mut native_calls) = mpsc::unbounded_channel();
        let transport = DiagnosticFailureTransport { requests: Arc::new(AtomicU64::new(0)), native_call, fail: Arc::new(tokio::sync::Notify::new()) };
        let engine_transport = transport.clone();
        let (lifecycle, _) = watch::channel((Some(actor), harness::server::HostActorLifecycle::Waiting));
        let runtime = service.runtime.clone();
        let mut running = tokio::spawn(async move {
            super::super::embedded_service::drive_conversation_with_transport::<DiagnosticFailureOffline, _>(
                embedded.driver, runtime, &settings, "offline-diagnostic".into(), harness::model::Effort::Low,
                "native diagnostic failure history".into(), embedded.cancellation_rx, lifecycle, actor, engine_transport,
            ).await
        });
        let request = tokio::time::timeout(Duration::from_secs(30), native_calls.recv()).await.unwrap().unwrap();
        let backend = TestCommands::completed("diagnostic-prefix");
        loop {
            let deployment = campaign.next_deployment("diagnostic prefix backend", Duration::from_secs(120), |event| match event {
                exomonad_actor::LocalResidentDeployment::CommandBackend(request) => Ok(request), other => Err(other),
            }).await;
            match deployment.purpose {
                exomonad_actor::command_jobs::CommandBackendPurpose::Command => { deployment.supply(Ok(backend.clone())); break; }
                exomonad_actor::command_jobs::CommandBackendPurpose::SourceProbe => deployment.supply(Ok(TestCommands::completed("/work/tree\n0123456789abcdef0123456789abcdef01234567\nclean\n"))),
            }
        }
        let operation = OperationId { origin: ConversationIdentity::Embedded { run: identity.run.clone(), actor: identity.actor.clone(), incarnation: identity.incarnation.clone() }, request, call: CallId("diagnostic-pending-cell".into()) };
        let context = ToolInvocationContext::original(original_operation(&identity, &operation).unwrap());
        tokio::time::timeout(Duration::from_secs(60), async {
            while campaign.actor.hosted_workbench_waiting(&context).is_none() { tokio::time::sleep(Duration::from_millis(5)).await; }
        }).await.expect("native cell reaches sleep before provider failure");
        assert_eq!(backend.executions(), 1);
        transport.fail.notify_one();
        let error = tokio::time::timeout(Duration::from_secs(60), &mut running).await.expect("driver joins native cleanup").unwrap().unwrap_err();
        let super::super::embedded_service::EmbeddedDriverError::Round(report) = error else { panic!("actual Harness failure report required: {error:?}") };
        assert!(matches!(report.primary_error(), Some(harness::engine::EngineError::Transport(harness::transport::TransportError::ProviderStreamFailure { diagnostic: Some(diagnostic), .. })) if diagnostic.code.as_deref() == Some("invalid_tools") && diagnostic.message.as_deref() == Some("unsupported tool schema")));
        assert!(report.secondary_failures().iter().any(|failure| failure.stage() == harness::engine::RoundFailureStage::InferenceBlockRecording && failure.source().to_string().contains("diagnostic block write refused")));
        let assessment = report.assessment();
        assert_eq!(assessment.execution(), harness::engine::RoundExecution::Failed);
        assert_eq!(assessment.cleanup(), harness::engine::RoundCleanup::Confirmed);
        assert!(assessment.outstanding_operations().is_empty());
        assert!(assessment.inference_obligations().iter().any(|obligation| matches!(obligation, harness::engine::InferenceObligation::PreserveUnsettled { .. })));
        assert!(!assessment.inference_obligations().iter().any(|obligation| matches!(obligation, harness::engine::InferenceObligation::RecoverBlock { .. })));
        assert!(!race.native_causes.lock().unwrap().is_empty());
        assert!(race.native_causes.lock().unwrap().iter().all(|cause| *cause == exomonad_actor::WorkbenchCancellationCause::ProviderFailureCleanup));
        let native = campaign.root_installation.policy.retained_operation(context).unwrap().terminal();
        assert!(matches!(native, exomonad_actor::HostedOperationTerminal::Settled(WorkbenchCancellationOutcome::Cancelled { .. })), "{native:?}");
        let claims = service.runtime.store().claims_for_operation(&operation).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].state, harness::store::ClaimState::Settled);
        assert!(matches!(service.runtime.store().embedded_inference_state(&identity).unwrap(), harness::embedding::HostInferenceState::Running));
        assert_eq!(transport.requests.load(Ordering::SeqCst), 2);
        sql.execute_batch("DROP TRIGGER diagnostic_block_fault; DROP TRIGGER diagnostic_block_insert_fault;").unwrap();
        service.shutdown().await.unwrap();
    })).await;
}
