use std::{
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use exomonad_actor::{
    ActorAdmissionLease, ActorExitKind, ActorTerminal, LocalActorRef, ResidentToolError,
    WorkbenchCancellationOutcome,
};
use exomonad_tool::{ToolArguments, ToolInvocationContext};
use harness::{
    embedding::{
        AdmissionGuard, Conversation, EmbeddedError, HostActor, HostControl, HostIdentity,
        ToolSurface,
    },
    mailbox::DurableMailboxWake,
    model::{AgentPath, ConversationIdentity, OperationId},
    provider::{
        CallContext, CancellationAcknowledgment, CancellationOwner, JobHandle, Provider,
        ProviderError,
    },
    store::Store,
    turn::JobScheduler,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::embedded_policy::{EmbeddedPolicyInstallation, EmbeddedPolicySnapshot};

struct StoreAdmission {
    _lease: ActorAdmissionLease,
}
impl AdmissionGuard for StoreAdmission {}

/// One harness owner for the existing run directory. The default
/// Codex launch never opens this Store; the embedding owner calls `open` when
/// it admits a bound conversation.
pub(super) struct EmbeddedHarnessRuntime {
    run: String,
    store: Arc<Store>,
    scheduler: Arc<JobScheduler>,
}

impl EmbeddedHarnessRuntime {
    pub(super) fn open(run_root: &Path, concurrent_jobs: usize) -> Result<Self, EmbeddedError> {
        let harness_root = run_root.join("harness");
        std::fs::create_dir_all(&harness_root)
            .map_err(|error| EmbeddedError::Binding(error.to_string()))?;
        Ok(Self {
            run: super::runtime_namespace(run_root),
            store: Arc::new(Store::open(harness_root.join("store.sqlite"))?),
            scheduler: Arc::new(
                JobScheduler::new(concurrent_jobs)
                    .map_err(|error| EmbeddedError::Binding(error.to_string()))?,
            ),
        })
    }

    pub(super) fn attach(
        &self,
        identity: HostIdentity,
        actor: LocalActorRef,
        installation: Arc<EmbeddedPolicyInstallation>,
        parent: Option<&AgentPath>,
    ) -> Result<EmbeddedConversation, EmbeddedError> {
        if identity.run != self.run {
            return Err(EmbeddedError::Binding(
                "embedded host run does not match the owning run directory".into(),
            ));
        }
        let (wakes, incoming) = mpsc::unbounded_channel();
        let host = Arc::new(EmbeddedHostActor::new(
            identity,
            actor,
            installation,
            wakes,
        )?);
        let conversation = Conversation::attach(self.store.clone(), host.clone(), parent)?;
        Ok(EmbeddedConversation {
            host,
            conversation,
            incoming,
        })
    }

    pub(super) fn scheduler(&self) -> Arc<JobScheduler> {
        self.scheduler.clone()
    }
}

pub(super) struct EmbeddedConversation {
    pub(super) host: Arc<EmbeddedHostActor>,
    pub(super) conversation: Conversation,
    pub(super) incoming: mpsc::UnboundedReceiver<DurableMailboxWake>,
}

/// The exact actor and installation used by one bound harness conversation.
/// The run runtime owns Store and its scheduler; this host supplies authority
/// and an identity-only wake channel for their existing mailbox path.
pub(super) struct EmbeddedHostActor {
    identity: HostIdentity,
    actor: LocalActorRef,
    installation: Arc<EmbeddedPolicyInstallation>,
    wakes: mpsc::UnboundedSender<DurableMailboxWake>,
    next_surface: AtomicU64,
}

impl EmbeddedHostActor {
    pub(super) fn new(
        identity: HostIdentity,
        actor: LocalActorRef,
        installation: Arc<EmbeddedPolicyInstallation>,
        wakes: mpsc::UnboundedSender<DurableMailboxWake>,
    ) -> Result<Self, EmbeddedError> {
        let exact_actor = actor.identity();
        if installation.actor() != exact_actor
            || identity.incarnation != exact_actor.incarnation.0.to_string()
        {
            return Err(EmbeddedError::Binding(
                "embedded host identity and installed policy must name the exact actor incarnation"
                    .into(),
            ));
        }
        Ok(Self {
            identity,
            actor,
            installation,
            wakes,
            next_surface: AtomicU64::new(1),
        })
    }
}

#[async_trait::async_trait]
impl HostActor for EmbeddedHostActor {
    fn identity(&self) -> &HostIdentity {
        &self.identity
    }

    fn admit(&self) -> Result<Box<dyn AdmissionGuard>, EmbeddedError> {
        self.actor
            .admit_transaction()
            .map(|lease| Box::new(StoreAdmission { _lease: lease }) as Box<dyn AdmissionGuard>)
            .map_err(|error| EmbeddedError::Host(error.to_string()))
    }

    fn tool_surface(&self) -> Result<Arc<ToolSurface>, EmbeddedError> {
        // A request must pin its handler before retirement closes admission.
        // The short lease also prevents cleanup from overtaking snapshot capture.
        let _admission = self
            .actor
            .admit_transaction()
            .map_err(|error| EmbeddedError::Host(error.to_string()))?;
        let snapshot = Arc::new(
            self.installation
                .request_snapshot()
                .map_err(|error| EmbeddedError::Surface(error.to_string()))?,
        );
        let sequence = self.next_surface.fetch_add(1, Ordering::Relaxed);
        let version = format!(
            "{}:{}:{}:{sequence}",
            self.identity.run, self.identity.actor.0, self.identity.incarnation
        );
        let tools = snapshot.tools().to_vec();
        let dispatcher: Arc<dyn Provider> = Arc::new(EmbeddedDispatcher {
            identity: self.identity.clone(),
            snapshot,
        });
        Ok(Arc::new(ToolSurface::new(version, tools, dispatcher)?))
    }

    async fn wake(&self, envelope_id: i64) -> Result<(), String> {
        self.wakes
            .send(DurableMailboxWake { envelope_id })
            .map_err(|_| "embedded Engine wake receiver closed".into())
    }

    async fn control(&self, control: HostControl) -> Result<Value, String> {
        match control {
            HostControl::Retire => {
                self.actor
                    .shutdown(ActorTerminal {
                        kind: ActorExitKind::Cancelled,
                        summary: "embedded host requested retirement".into(),
                    })
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(json!({"requested":true}))
            }
            HostControl::Interrupt => Err(
                "interrupt requires an exact operation; use the bound cancellation owner".into(),
            ),
        }
    }
}

#[derive(Clone)]
struct EmbeddedDispatcher {
    identity: HostIdentity,
    snapshot: Arc<EmbeddedPolicySnapshot>,
}

impl EmbeddedDispatcher {
    fn context(&self, operation: &OperationId) -> Result<ToolInvocationContext, ProviderError> {
        match &operation.origin {
            ConversationIdentity::Embedded {
                run,
                actor,
                incarnation,
            } if run == &self.identity.run
                && actor == &self.identity.actor
                && incarnation == &self.identity.incarnation => {}
            _ => return Err(ProviderError::Tool("foreign embedded operation".into())),
        }
        Ok(ToolInvocationContext {
            context_call_id: None,
            thread_id: format!("{}:{}", self.identity.run, self.identity.actor.0),
            turn_id: operation.request.0.clone(),
            call_id: operation.call.0.clone(),
            namespace: Some(format!("embedded:{}", self.identity.incarnation)),
        })
    }

    async fn dispatch(
        &self,
        name: &str,
        arguments: ToolArguments,
        context: CallContext,
    ) -> Result<Value, ProviderError> {
        let operation = context.operation.as_ref().ok_or_else(|| {
            ProviderError::Tool("embedded dispatch requires an exact operation".into())
        })?;
        if context.request.as_ref() != Some(&operation.request)
            || context.call_id != operation.call
            || context.agent != self.identity.actor
        {
            return Err(ProviderError::Tool("foreign embedded call context".into()));
        }
        self.snapshot
            .dispatch(name.to_owned(), arguments, self.context(operation)?)
            .await
            .map_err(|error| ProviderError::Tool(error.to_string()))
    }
}

#[async_trait::async_trait]
impl Provider for EmbeddedDispatcher {
    fn tools(&self) -> Vec<Value> {
        self.snapshot.tools().to_vec()
    }

    fn cancellation_owner(&self) -> Option<Arc<dyn CancellationOwner>> {
        Some(Arc::new(self.clone()))
    }

    async fn call(&self, _: &str, _: Value) -> Result<Value, ProviderError> {
        Err(ProviderError::Tool(
            "embedded dispatch requires an exact operation".into(),
        ))
    }

    async fn call_with_context(
        &self,
        name: &str,
        arguments: Value,
        context: CallContext,
    ) -> Result<Value, ProviderError> {
        self.dispatch(name, ToolArguments::Structured(arguments), context)
            .await
    }

    async fn call_custom_with_context(
        &self,
        name: &str,
        input: String,
        context: CallContext,
    ) -> Result<Value, ProviderError> {
        self.dispatch(name, ToolArguments::Raw(input), context)
            .await
    }
}

#[async_trait::async_trait]
impl CancellationOwner for EmbeddedDispatcher {
    async fn cancel(
        &self,
        operation: &OperationId,
        _handle: &JobHandle,
    ) -> CancellationAcknowledgment {
        let context = match self.context(operation) {
            Ok(context) => context,
            Err(error) => return CancellationAcknowledgment::Unconfirmed(error.to_string()),
        };
        match self.snapshot.cancel(context).await {
            Ok(WorkbenchCancellationOutcome::Cancelled { .. }) => {
                CancellationAcknowledgment::Stopped
            }
            Ok(WorkbenchCancellationOutcome::Expired { reply, .. }) => {
                let result = reply
                    .map_err(ResidentToolError::Invocation)
                    .and_then(|response| {
                        serde_json::to_value(response).map_err(ResidentToolError::Encoding)
                    })
                    .map_err(|error| ProviderError::Tool(error.to_string()).to_string());
                CancellationAcknowledgment::Completed(result)
            }
            Ok(outcome) => CancellationAcknowledgment::Unconfirmed(format!("{outcome:?}")),
            Err(error) => CancellationAcknowledgment::Unconfirmed(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor_host::test_campaign::TestCampaign;
    use async_trait::async_trait;
    use harness::{
        embedding::InputObservation,
        engine::{EngineConfig, ResponsesTransport},
        item::Item,
        model::Effort,
        transport::{Auth, ResponsesRequest, ResponsesTurn, TransportError},
    };
    use std::{num::NonZeroU64, sync::Mutex, time::Duration};

    #[derive(Clone)]
    struct Offline;
    impl Auth for Offline {
        fn access(&self) -> Result<(String, String), TransportError> {
            panic!("offline transport must not request credentials")
        }
    }

    #[derive(Clone)]
    struct ParkUntilInput {
        entered: Arc<tokio::sync::Notify>,
        requests: Arc<Mutex<Vec<ResponsesRequest>>>,
    }

    #[async_trait]
    impl ResponsesTransport for ParkUntilInput {
        async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request);
            let round = requests.len();
            drop(requests);
            let items = if round == 1 {
                self.entered.notify_one();
                vec![Item(json!({
                    "type":"function_call", "call_id":"wait-for-input",
                    "name":"wait_agent", "arguments":"{}"
                }))]
            } else {
                vec![Item(json!({
                    "type":"message", "role":"assistant", "phase":"final_answer",
                    "content":[{"type":"output_text","text":"done"}]
                }))]
            };
            Ok(ResponsesTurn {
                response_id: format!("park-{round}"),
                items,
                usage: Default::default(),
            })
        }
    }

    #[tokio::test]
    async fn real_actor_bound_engine_wakes_from_durable_input() {
        let campaign = TestCampaign::start().await;
        let actor = campaign.actor.identity();
        let installation = Arc::new(EmbeddedPolicyInstallation::from_installation(
            &campaign.root_installation,
        ));
        let runtime = EmbeddedHarnessRuntime::open(campaign.session_root.path(), 1).unwrap();
        let identity = HostIdentity {
            run: super::super::runtime_namespace(campaign.session_root.path()),
            actor: AgentPath("/root".into()),
            incarnation: actor.incarnation.0.to_string(),
        };
        let wrong = HostIdentity {
            incarnation: "wrong-incarnation".into(),
            ..identity.clone()
        };
        assert!(runtime
            .attach(wrong, campaign.actor.clone(), installation.clone(), None)
            .is_err());
        let wrong_run = HostIdentity {
            run: "another-run".into(),
            ..identity.clone()
        };
        assert!(runtime
            .attach(
                wrong_run,
                campaign.actor.clone(),
                installation.clone(),
                None
            )
            .is_err());
        let binding = runtime
            .attach(identity.clone(), campaign.actor.clone(), installation, None)
            .unwrap();
        assert!(!binding.host.tool_surface().unwrap().tools().is_empty());

        let transport = ParkUntilInput {
            entered: Arc::new(tokio::sync::Notify::new()),
            requests: Arc::new(Mutex::new(vec![])),
        };
        let engine = binding
            .conversation
            .engine::<Offline, _>(
                transport.clone(),
                runtime.scheduler(),
                EngineConfig {
                    instructions: "resident test".into(),
                    tools: vec![],
                    model: "offline".into(),
                    effort: Effort::Medium,
                    session_id: "resident-test".into(),
                    agent: identity.actor,
                },
                NonZeroU64::new(200_000).unwrap(),
            )
            .unwrap();
        let (_cancel, cancellation) = tokio::sync::watch::channel(false);
        let running = tokio::spawn(async move {
            engine
                .run_embedded(
                    None,
                    vec![Item(json!({
                        "type":"message", "role":"user", "content":"start"
                    }))],
                    cancellation,
                    binding.incoming,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), transport.entered.notified())
            .await
            .unwrap();
        let receipt = binding
            .conversation
            .input("operator-1", "operator", "wake me")
            .await
            .unwrap();
        assert!(receipt.wake_error.is_none(), "{receipt:?}");
        tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1]
                .input
                .iter()
                .filter(|item| item.0["content"] == "wake me")
                .count(),
            1
        );
        assert!(matches!(
            binding
                .conversation
                .input_observation(receipt.envelope_id)
                .unwrap(),
            InputObservation::Included(_)
        ));
        drop(requests);
        campaign.forest.shutdown().await;
        campaign.hosted.await.unwrap();
    }

    #[tokio::test]
    async fn request_snapshot_rejects_closed_actor_before_installation_clears() {
        let campaign = TestCampaign::start().await;
        let actor = campaign.actor.identity();
        let installation = Arc::new(EmbeddedPolicyInstallation::from_installation(
            &campaign.root_installation,
        ));
        let identity = HostIdentity {
            run: super::super::runtime_namespace(campaign.session_root.path()),
            actor: AgentPath("/root".into()),
            incarnation: actor.incarnation.0.to_string(),
        };
        let (wakes, _incoming) = mpsc::unbounded_channel();
        let host = EmbeddedHostActor::new(
            identity,
            campaign.actor.clone(),
            installation.clone(),
            wakes,
        )
        .unwrap();
        let held_store_transaction = campaign.actor.admit_transaction().unwrap();
        let retiring_actor = campaign.actor.clone();
        let retiring = tokio::spawn(async move {
            retiring_actor
                .shutdown(ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "request snapshot retirement race".into(),
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if campaign.actor.admit_transaction().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            installation.request_snapshot().is_ok(),
            "the old installation remains published"
        );
        assert!(
            host.tool_surface().is_err(),
            "closed admission must reject a new request"
        );
        drop(held_store_transaction);
        retiring.await.unwrap().unwrap();
        campaign.forest.shutdown().await;
        campaign.hosted.await.unwrap();
    }
}
