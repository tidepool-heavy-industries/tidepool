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
        assert!(snapshot
            .dispatch(name.into(), arguments, invocation.clone(), None, None)
            .await
            .is_err());
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
    assert!(snapshot
        .dispatch(
            "status".into(),
            ToolArguments::Structured(json!({})),
            invocation,
            None,
            None
        )
        .await
        .is_err());
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
    assert!(snapshot
        .dispatch(
            "status".into(),
            ToolArguments::Structured(json!({})),
            invocation,
            None,
            None
        )
        .await
        .is_err());
    host.output_committed(&retired).await.unwrap();
    host.output_aborted(&retired).await.unwrap();
}
