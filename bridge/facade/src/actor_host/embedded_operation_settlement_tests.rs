use super::*;
use crate::actor_host::embedded_harness::{EmbeddedHostActor, EmbeddedRoundControl};
use exomonad_actor::{
    ActorRef, ActorTerminal, KernelBehavior, KernelBehaviorError, KernelContext,
    KernelInvocationFailure, KernelStep, MailboxValue,
};
use futures_util::future::BoxFuture;
use harness::{
    embedding::{HostActor, HostIdentity},
    model::{AgentPath, CallId, ConversationIdentity, OperationId, RequestId},
    store::Store,
};

// These admission tests have no machine or execution authority. A real actor
// supplies the mailbox/lifecycle owner; reaching its native workbench is a bug.
struct AdmissionSentinel;
impl KernelBehavior for AdmissionSentinel {
    fn start<'a>(
        &'a mut self,
        _: &'a KernelContext,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async { Ok(KernelStep::Continue(())) })
    }
    fn cast<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: ActorRef,
        _: MailboxValue,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        panic!("no mailbox effects admitted")
    }
    fn call<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: ActorRef,
        _: exomonad_actor::CallAncestry,
        _: MailboxValue,
    ) -> BoxFuture<'a, Result<KernelStep<MailboxValue>, KernelBehaviorError>> {
        panic!("no mailbox effects admitted")
    }
    fn tool<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: ToolInvocation,
        _: Option<Arc<dyn HostedCheckpointCapture>>,
    ) -> BoxFuture<'a, Result<KernelStep<Value>, KernelInvocationFailure>> {
        panic!("no native tool admitted")
    }
    fn workbench<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: exomonad_actor::ActorWorkbenchInvocation,
        _: Option<Arc<exomonad_actor::WorkbenchExecutionControl>>,
    ) -> BoxFuture<
        'a,
        Result<KernelStep<tidepool_runtime::session::WorkbenchResponse>, KernelInvocationFailure>,
    > {
        panic!("no native workbench admitted")
    }
    fn external_application_failed<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: exomonad_actor::ExternalApplicationFailure,
    ) -> BoxFuture<'a, exomonad_actor::ExternalFailureDisposition> {
        panic!("no external application admitted")
    }
    fn shutdown<'a>(
        &'a mut self,
        _: &'a KernelContext,
        _: &'a ActorTerminal,
    ) -> BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async { Ok(()) })
    }
    fn stopped<'a>(&'a mut self, _: &'a KernelContext, _: &'a ActorTerminal) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
    fn child_exited(&mut self, _: exomonad_actor::ChildExitNotice) {
        panic!("no children admitted")
    }
}

fn operation(actor: ActorRef, call: &str) -> (HostIdentity, OperationId, ToolInvocationContext) {
    let identity = HostIdentity {
        run: "admission".into(),
        actor: AgentPath("/root".into()),
        incarnation: actor.incarnation.0.to_string(),
    };
    let operation = OperationId {
        origin: ConversationIdentity::Embedded {
            run: identity.run.clone(),
            actor: identity.actor.clone(),
            incarnation: identity.incarnation.clone(),
        },
        request: RequestId("request".into()),
        call: CallId(call.into()),
    };
    let invocation = ToolInvocationContext {
        origin: exomonad_tool::ToolInvocationOrigin::Model(exomonad_tool::OriginalOperation {
            origin: exomonad_tool::ConversationOrigin::Embedded {
                run: identity.run.clone(),
                actor: identity.actor.0.clone(),
                incarnation: identity.incarnation.clone(),
            },
            request_id: "request".into(),
            call_id: call.into(),
        }),
        call_id: call.into(),
        namespace: None,
    };
    (identity, operation, invocation)
}

#[tokio::test]
async fn schema_argument_and_selected_tool_refusals_acknowledge_only_issued_operations() {
    let (actor, task) = exomonad_actor::spawn_local_actor(None, AdmissionSentinel)
        .await
        .unwrap();
    let tools = vec![HostedTool::Function(exomonad_tool::ToolDeclaration {
        schedule: Default::default(),
        implementation: Default::default(),
        effect_keys: Vec::new(),
        name: "lookup".into(),
        description: "object input".into(),
        input_schema: json!({"type":"object","properties":{}}),
        output_schema: None,
        kind: exomonad_tool::ToolKind::Call,
    })];
    let policy: Arc<dyn ResidentToolEndpoint> = Arc::new(
        exomonad_actor::ResidentInteractivePolicy::local_with_tools(actor.clone(), tools),
    );
    let installation = Arc::new(EmbeddedPolicyInstallation::new(
        actor.identity(),
        policy.clone(),
    ));
    let projection = project_tools(policy.tools()).unwrap();
    let snapshot = EmbeddedPolicySnapshot {
        actor: actor.identity(),
        policy: policy.clone(),
        manifest: projection.manifest,
        schemas: projection.schemas,
    };
    let (identity, _, _) = operation(actor.identity(), "schema");
    let (wakes, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let host = EmbeddedHostActor::new(
        identity,
        actor.clone(),
        installation,
        Arc::new(Store::memory().unwrap()),
        wakes,
        Arc::new(EmbeddedRoundControl::default()),
    )
    .unwrap();
    for (call, name, arguments) in [
        (
            "schema",
            "lookup",
            ToolArguments::Structured(json!("not an object")),
        ),
        (
            "argument",
            "lookup",
            ToolArguments::Raw("wrong kind".into()),
        ),
        (
            "selected",
            "unknown",
            ToolArguments::Raw("unknown tool".into()),
        ),
    ] {
        let (_, operation, invocation) = operation(actor.identity(), call);
        assert!(matches!(
            snapshot
                .dispatch(name.into(), arguments, invocation.clone(), None, None)
                .await,
            Err(ResidentToolError::InvalidInvocation(_))
        ));
        let owner = policy.retained_operation(invocation).unwrap();
        assert!(matches!(
            owner.finalization(),
            exomonad_actor::HostedOperationFinalization::Settled(Ok(
                exomonad_actor::ProviderFinalizationKind::NotAdmitted
            ))
        ));
        host.output_committed(&operation).await.unwrap();
        host.output_committed(&operation).await.unwrap();
        host.output_aborted(&operation).await.unwrap();
    }
    let (_, unknown, _) = operation(actor.identity(), "never-issued");
    assert!(host.output_committed(&unknown).await.is_err());
    actor
        .shutdown(ActorTerminal {
            kind: exomonad_actor::ActorExitKind::Cancelled,
            summary: "admission test complete".into(),
            diagnostic: None,
        })
        .await
        .unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn sealed_and_retired_mailboxes_keep_no_admission_acknowledgement() {
    let (actor, task) = exomonad_actor::spawn_local_actor(None, AdmissionSentinel)
        .await
        .unwrap();
    let policy: Arc<dyn ResidentToolEndpoint> = Arc::new(
        exomonad_actor::ResidentInteractivePolicy::local(actor.clone()),
    );
    let installation = Arc::new(EmbeddedPolicyInstallation::new(
        actor.identity(),
        policy.clone(),
    ));
    let projection = project_tools(policy.tools()).unwrap();
    let snapshot = EmbeddedPolicySnapshot {
        actor: actor.identity(),
        policy: policy.clone(),
        manifest: projection.manifest,
        schemas: projection.schemas,
    };
    let (identity, _, _) = operation(actor.identity(), "sealed");
    let (wakes, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let host = EmbeddedHostActor::new(
        identity,
        actor.clone(),
        installation,
        Arc::new(Store::memory().unwrap()),
        wakes,
        Arc::new(EmbeddedRoundControl::default()),
    )
    .unwrap();
    actor.seal_hosted_work().await.unwrap();
    let (_, sealed, invocation) = operation(actor.identity(), "sealed");
    assert!(matches!(
        snapshot
            .dispatch(
                "status".into(),
                ToolArguments::Structured(json!({})),
                invocation,
                None,
                None
            )
            .await,
        Err(ResidentToolError::Invocation(
            KernelInvocationFailure::Rejected { .. }
        ))
    ));
    host.output_committed(&sealed).await.unwrap();
    actor
        .shutdown(ActorTerminal {
            kind: exomonad_actor::ActorExitKind::Cancelled,
            summary: "retire before next admission".into(),
            diagnostic: None,
        })
        .await
        .unwrap();
    task.await.unwrap();
    let (_, retired, invocation) = operation(actor.identity(), "retired");
    assert!(matches!(
        snapshot
            .dispatch(
                "status".into(),
                ToolArguments::Structured(json!({})),
                invocation,
                None,
                None
            )
            .await,
        Err(ResidentToolError::Invocation(
            KernelInvocationFailure::Rejected { .. }
        ))
    ));
    host.output_committed(&retired).await.unwrap();
    host.output_aborted(&retired).await.unwrap();
}

struct Offline;
impl harness::transport::Auth for Offline {
    fn access(&self) -> Result<(String, String), harness::transport::TransportError> {
        panic!("the deterministic transport must not request credentials")
    }
}

// Immutable schema declarations need no compiled installation for a refused
// input. Valid dispatch is forbidden: only the actual native client's
// NotAdmitted issuance and acknowledgement are exercised by this fixture.
#[derive(Clone)]
struct ValidationOnlyEndpoint(Arc<dyn ResidentToolEndpoint>);
impl ResidentToolEndpoint for ValidationOnlyEndpoint {
    fn snapshot_for_request(&self) -> Result<Arc<dyn ResidentToolEndpoint>, ResidentToolError> {
        Ok(Arc::new(self.clone()))
    }
    fn tools(&self) -> &[HostedTool] {
        self.0.tools()
    }
    fn instructions(&self) -> Option<&str> {
        None
    }
    fn dispatch_boxed(&self, _: ToolInvocation) -> exomonad_actor::ResidentToolDispatchFuture {
        panic!("the schema-only fixture must not admit valid input")
    }
    fn dispatch_validated_with_context_boxed(
        &self,
        invocation: ToolInvocation,
        arguments: Result<ToolArguments, ResidentToolError>,
        capture: Option<Arc<dyn HostedCheckpointCapture>>,
        context: Option<Arc<dyn exomonad_actor::HostedContextBinding>>,
    ) -> exomonad_actor::ResidentToolDispatchFuture {
        assert!(matches!(
            &arguments,
            Err(ResidentToolError::InvalidInvocation(_))
        ));
        self.0
            .dispatch_validated_with_context_boxed(invocation, arguments, capture, context)
    }
    fn retained_operation(
        &self,
        invocation: ToolInvocationContext,
    ) -> Result<exomonad_actor::HostedOperationSettlement, ResidentToolError> {
        self.0.retained_operation(invocation)
    }
    fn complete_boxed(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> ResidentToolFuture {
        self.0.complete_boxed(boundary)
    }
    fn abort_boxed(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> ResidentToolFuture {
        self.0.abort_boxed(boundary)
    }
}

#[derive(Clone)]
struct SchemaRefusalTransport(Arc<std::sync::atomic::AtomicUsize>);

fn schema_refusal_parameters() -> Value {
    json!({
        "type":"object","properties":{"choice":{"anyOf":[
            {"type":"object","properties":{"value":{"type":"string"}},"required":[]},
            {"type":"object","properties":{"value":{"type":["string","null"]}},"required":["value"]}
        ]}},"required":["choice"]
    })
}

fn schema_refusal_input() -> Value {
    json!({"choice":{"value":null}})
}

#[async_trait::async_trait]
impl harness::engine::ResponsesTransport for SchemaRefusalTransport {
    async fn create(
        &self,
        _: harness::transport::ResponsesRequest,
    ) -> Result<harness::transport::ResponsesTurn, harness::transport::TransportError> {
        assert_eq!(self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst), 0);
        Ok(harness::transport::ResponsesTurn {
            response_id: "schema-refusal".into(),
            items: vec![harness::item::Item(json!({
                "type":"function_call", "call_id":"schema", "name":"lookup",
                "arguments":serde_json::to_string(&schema_refusal_input()).unwrap()
            }))],
            usage: Default::default(),
        })
    }
}

// Close in the valid window after the real facade acknowledges a durable
// NotAdmitted result. This driver supplies no native execution/retirement proof.
struct CloseAfterAcknowledgement {
    inner: Arc<EmbeddedHostActor>,
    actor: exomonad_actor::LocalActorRef,
    policy: Arc<dyn ResidentToolEndpoint>,
    store: Arc<Store>,
    retained: std::sync::Mutex<
        Option<(
            OperationId,
            harness::store::RecordedToolOutput,
            Vec<harness::store::Claim>,
        )>,
    >,
}

#[async_trait::async_trait]
impl HostActor for CloseAfterAcknowledgement {
    fn identity(&self) -> &HostIdentity {
        self.inner.identity()
    }
    fn admit(
        &self,
    ) -> Result<Box<dyn harness::embedding::AdmissionGuard>, harness::embedding::EmbeddedError>
    {
        self.inner.admit()
    }
    fn tool_surface(
        &self,
    ) -> Result<Arc<harness::embedding::ToolSurface>, harness::embedding::EmbeddedError> {
        self.inner.tool_surface()
    }
    async fn wake(&self, envelope_id: i64) -> Result<(), String> {
        self.inner.wake(envelope_id).await
    }
    async fn control(
        &self,
        control: harness::embedding::HostControl,
    ) -> Result<Value, harness::embedding::HostControlError> {
        self.inner.control(control).await
    }
    async fn output_committed(&self, operation: &OperationId) -> Result<(), String> {
        self.inner.output_committed(operation).await?;
        let invocation = ToolInvocationContext {
            origin: exomonad_tool::ToolInvocationOrigin::Model(exomonad_tool::OriginalOperation {
                origin: exomonad_tool::ConversationOrigin::Embedded {
                    run: self.identity().run.clone(),
                    actor: self.identity().actor.0.clone(),
                    incarnation: self.identity().incarnation.clone(),
                },
                request_id: operation.request.0.clone(),
                call_id: operation.call.0.clone(),
            }),
            call_id: operation.call.0.clone(),
            namespace: None,
        };
        assert!(matches!(
            self.policy
                .retained_operation(invocation)
                .unwrap()
                .finalization(),
            exomonad_actor::HostedOperationFinalization::Settled(Ok(
                exomonad_actor::ProviderFinalizationKind::NotAdmitted
            ))
        ));
        let output = self
            .store
            .replay_tool_output_operation(operation)
            .unwrap()
            .unwrap();
        assert!(matches!(
            output.terminal,
            harness::store::TerminalOutcome::Failure(_)
        ));
        let claims = self.store.claims_for_operation(operation).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].state, harness::store::ClaimState::Settled);
        assert!(self
            .retained
            .lock()
            .unwrap()
            .replace((operation.clone(), output, claims))
            .is_none());
        let stopped = self
            .actor
            .shutdown_with_cleanup(ActorTerminal {
                kind: exomonad_actor::ActorExitKind::Cancelled,
                summary: "close after acknowledged schema refusal".into(),
                diagnostic: None,
            })
            .await
            .unwrap();
        // This real generic actor owns its hook and children, but no resident
        // realm. Preserve Unsupported rather than certifying native resources.
        assert_eq!(stopped.cleanup.actor(), self.actor.identity());
        assert_eq!(
            stopped.cleanup.hook(),
            &exomonad_actor::CleanupComponentOutcome::Confirmed
        );
        assert_eq!(
            stopped.cleanup.children(),
            &exomonad_actor::CleanupComponentOutcome::Confirmed
        );
        assert_eq!(
            stopped.cleanup.realm(),
            &exomonad_actor::CleanupComponentOutcome::Unsupported
        );
        assert!(!stopped.cleanup.is_confirmed(), "{stopped:?}");
        assert_eq!(
            stopped.terminal.kind,
            exomonad_actor::ActorExitKind::Cancelled
        );
        assert_eq!(
            self.actor.terminal().cleanup().as_ref(),
            Some(&stopped.cleanup)
        );
        assert_eq!(
            self.actor.terminal().get().as_ref(),
            Some(&stopped.terminal)
        );
        Ok(())
    }
    async fn output_aborted(&self, operation: &OperationId) -> Result<(), String> {
        self.inner.output_aborted(operation).await
    }
}

#[tokio::test]
async fn acknowledged_schema_refusal_then_native_close_cancels_only_the_successor_request() {
    // The schema decoder projects optional nulls; overlapping alternatives
    // must refuse this ambiguous input before native argument admission.
    let schema = harness::finalize::FunctionToolSchema::new(schema_refusal_parameters()).unwrap();
    assert!(matches!(
        schema.decode_arguments(schema_refusal_input()),
        Err(harness::finalize::FinalizeError::AmbiguousArguments(_))
    ));
    let (actor, task) = exomonad_actor::spawn_local_actor(None, AdmissionSentinel)
        .await
        .unwrap();
    let tools = vec![HostedTool::Function(exomonad_tool::ToolDeclaration {
        schedule: exomonad_tool::ToolScheduling::BeforeNextInference,
        implementation: Default::default(),
        effect_keys: Vec::new(),
        name: "lookup".into(),
        description: "object input".into(),
        input_schema: schema_refusal_parameters(),
        output_schema: None,
        kind: exomonad_tool::ToolKind::Call,
    })];
    let native_policy: Arc<dyn ResidentToolEndpoint> = Arc::new(
        exomonad_actor::ResidentInteractivePolicy::local_with_tools(actor.clone(), tools),
    );
    assert!(native_policy.snapshot_for_request().is_err());
    let policy: Arc<dyn ResidentToolEndpoint> = Arc::new(ValidationOnlyEndpoint(native_policy));
    let installation = Arc::new(EmbeddedPolicyInstallation::new(
        actor.identity(),
        policy.clone(),
    ));
    let store = Arc::new(Store::memory().unwrap());
    let (identity, _, _) = operation(actor.identity(), "schema");
    let (wakes, incoming) = tokio::sync::mpsc::unbounded_channel();
    let host = Arc::new(CloseAfterAcknowledgement {
        inner: Arc::new(
            EmbeddedHostActor::new(
                identity,
                actor.clone(),
                installation,
                store.clone(),
                wakes,
                Arc::new(EmbeddedRoundControl::default()),
            )
            .unwrap(),
        ),
        actor: actor.clone(),
        policy,
        store: store.clone(),
        retained: Default::default(),
    });
    let conversation =
        harness::embedding::Conversation::attach(store.clone(), host.clone(), None).unwrap();
    host.inner.tool_surface().unwrap();
    let scheduler = Arc::new(harness::turn::JobScheduler::new(1).unwrap());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let engine = conversation
        .engine::<Offline, _>(
            SchemaRefusalTransport(calls.clone()),
            scheduler.clone(),
            harness::engine::EngineConfig {
                instructions: "exercise actual schema refusal then permanent native closure".into(),
                tools: Vec::new(),
                model: "offline".into(),
                effort: harness::model::Effort::Low,
                session_id: "post-retirement".into(),
                agent: host.identity().actor.clone(),
            },
            std::num::NonZeroU64::new(1000).unwrap(),
        )
        .unwrap();
    let (cancellation, receiver) = tokio::sync::watch::channel(false);
    assert!(!*cancellation.borrow());
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        engine.run_embedded(
            None,
            vec![harness::item::Item(
                json!({"type":"message","role":"user","content":"lookup"}),
            )],
            receiver,
            incoming,
        ),
    )
    .await
    .unwrap();
    let head = match result {
        Err(harness::engine::EngineError::Cancelled {
            head_request: Some(head),
        }) => head,
        result => panic!("expected typed successor cancellation, got {result:?}"),
    };
    assert!(!*cancellation.borrow());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let (operation, output, claims) = host.retained.lock().unwrap().clone().unwrap();
    assert_eq!(
        store.request(&head).unwrap().unwrap().parent,
        Some(operation.request.clone())
    );
    assert_eq!(
        store
            .replay_tool_output_operation(&operation)
            .unwrap()
            .unwrap(),
        output
    );
    assert_eq!(store.claims_for_operation(&operation).unwrap(), claims);
    assert!(store.pending_at(&operation.request).unwrap().is_empty());
    assert!(store.pending_at(&head).unwrap().is_empty());
    assert!(matches!(
        scheduler.output(&operation).await.unwrap(),
        Some(harness::turn::JobOutput::Completed(Err(_)))
    ));
    assert!(matches!(
        host.inner.tool_surface(),
        Err(harness::embedding::EmbeddedError::AdmissionClosed)
    ));
    // Engine reports the exact settled frontier; the outer driver owns its CAS.
    assert_eq!(store.embedded_agent_head(host.identity()).unwrap(), None);
    assert_eq!(
        store
            .embedded_round_frontier(host.identity())
            .unwrap()
            .pending_head,
        Some(head.clone())
    );
    assert!(store
        .settle_embedded_round(
            host.identity(),
            None,
            &head,
            harness::store::EmbeddedRoundOutcome::Cancelled
        )
        .unwrap());
    assert_eq!(
        store.embedded_agent_head(host.identity()).unwrap(),
        Some(head.clone())
    );
    assert_eq!(
        store
            .embedded_round_frontier(host.identity())
            .unwrap()
            .pending_head,
        None
    );
    assert_eq!(
        store
            .replay_tool_output_operation(&operation)
            .unwrap()
            .unwrap(),
        output
    );
    host.inner.output_committed(&operation).await.unwrap();
    task.await.unwrap();
}
