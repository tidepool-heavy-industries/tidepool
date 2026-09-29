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
        let conversation = Arc::new(Conversation::attach(
            self.store.clone(),
            host.clone(),
            parent,
        )?);
        Ok(EmbeddedConversation {
            conversation,
            incoming,
        })
    }

    pub(super) fn scheduler(&self) -> Arc<JobScheduler> {
        self.scheduler.clone()
    }

    pub(super) fn store(&self) -> Arc<Store> {
        self.store.clone()
    }
}

pub(super) struct EmbeddedConversation {
    pub(super) conversation: Arc<Conversation>,
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
    use crate::actor_host::embedded_service::{
        attach_actor, drive_conversation_with_transport, submit_browser_command, EmbeddedService,
    };
    use crate::actor_host::publish_embedded_root_snapshot;
    use crate::actor_host::test_campaign::TestCampaign;
    use async_trait::async_trait;
    use futures_util::StreamExt;
    use harness::{
        embedding::InputObservation,
        engine::ResponsesTransport,
        item::Item,
        model::Effort,
        transport::{Auth, ResponsesRequest, ResponsesTurn, TransportError},
    };
    use std::{sync::Mutex, time::Duration};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    #[test]
    fn m1_only_accepts_fresh_root_and_rejects_inherited_or_child_attachments() {
        let root = exomonad_actor::ActorRef::first(exomonad_actor::ActorId(1));
        let child = exomonad_actor::ActorRef::first(exomonad_actor::ActorId(2));
        assert_eq!(
            crate::actor_host::embedded_root_attachment_error(root, true, false, None),
            None
        );
        assert!(
            crate::actor_host::embedded_root_attachment_error(root, true, true, None)
                .unwrap()
                .contains("checkpoint")
        );
        assert!(
            crate::actor_host::embedded_root_attachment_error(root, true, false, Some(child))
                .unwrap()
                .contains("inherited claims")
        );
        assert!(
            crate::actor_host::embedded_root_attachment_error(child, false, false, None)
                .unwrap()
                .contains("explicit fresh-launch")
        );
    }

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
        completed: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        requests: Arc<Mutex<Vec<ResponsesRequest>>>,
        compactions: Arc<AtomicU64>,
    }

    #[async_trait]
    impl ResponsesTransport for ParkUntilInput {
        async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
            if request.tools_allowed.as_ref().is_some_and(Vec::is_empty) {
                assert!(request.tools.is_empty());
                assert!(
                    request.input.iter().any(|item| {
                        item.0["type"] == "custom_tool_call_output"
                            && item.0.to_string().contains("42")
                    }),
                    "compaction must receive the actual settled Haskell result"
                );
                self.compactions.fetch_add(1, Ordering::Relaxed);
                return Ok(ResponsesTurn {
                    response_id: "offline-compaction".into(),
                    items: vec![Item(json!({
                        "type":"message", "role":"assistant",
                        "content":[{"type":"output_text","text":"The Haskell cell completed with result 42."}]
                    }))],
                    usage: Default::default(),
                });
            }
            let round = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request);
                requests.len()
            };
            let items = if round == 1 {
                vec![
                    Item(json!({
                        "type":"custom_tool_call", "call_id":"raw-cell-1",
                        "name":"haskell", "input":"40 + 2 :: Int"
                    })),
                    Item(json!({
                        "type":"function_call", "call_id":"wait-for-cell",
                        "name":"wait_agent", "arguments":"{}"
                    })),
                ]
            } else if round == 2 {
                self.entered.notify_one();
                self.release.notified().await;
                vec![Item(json!({
                    "type":"message", "role":"assistant", "phase":"final_answer",
                    "content":[{"type":"output_text","text":"cell finished"}]
                }))]
            } else {
                self.completed.notify_one();
                vec![Item(json!({
                    "type":"message", "role":"assistant", "phase":"final_answer",
                    "content":[{"type":"output_text","text":"done"}]
                }))]
            };
            Ok(ResponsesTurn {
                response_id: format!("park-{round}"),
                items,
                usage: harness::transport::Usage {
                    input_tokens: if round == 1 { 100_001 } else { 0 },
                    ..Default::default()
                },
            })
        }
    }

    #[tokio::test]
    async fn production_embedded_service_authenticates_browser_and_wakes_real_actor_engine() {
        let campaign = TestCampaign::start().await;
        let actor = campaign.actor.identity();
        let installation = Arc::new(EmbeddedPolicyInstallation::from_installation(
            &campaign.root_installation,
        ));
        let files = tempfile::tempdir().unwrap();
        let assets = files.path().join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
        let secret = "embedded-browser-offline-test-secret-32-bytes";
        let secret_file = files.path().join("session-secret");
        std::fs::write(&secret_file, secret).unwrap();
        let auth_file = files.path().join("codex-auth.json");
        std::fs::write(&auth_file, "{}").unwrap();
        let settings = crate::exomonad::EmbeddedLaunchConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            asset_root: assets,
            session_secret_file: secret_file,
            codex_auth_file: auth_file,
            context_capacity_tokens: 200_000,
            concurrent_jobs: 1,
        };
        let mut service = EmbeddedService::prepare(campaign.session_root.path(), &settings)
            .await
            .unwrap();
        let identity = HostIdentity {
            run: super::super::runtime_namespace(campaign.session_root.path()),
            actor: AgentPath("/root".into()),
            incarnation: actor.incarnation.0.to_string(),
        };
        let wrong = HostIdentity {
            incarnation: "wrong-incarnation".into(),
            ..identity.clone()
        };
        assert!(service
            .runtime
            .attach(wrong, campaign.actor.clone(), installation.clone(), None)
            .is_err());
        let wrong_run = HostIdentity {
            run: "another-run".into(),
            ..identity.clone()
        };
        assert!(service
            .runtime
            .attach(
                wrong_run,
                campaign.actor.clone(),
                installation.clone(),
                None
            )
            .is_err());

        let transport = ParkUntilInput {
            entered: Arc::new(tokio::sync::Notify::new()),
            completed: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
            requests: Arc::new(Mutex::new(vec![])),
            compactions: Arc::new(AtomicU64::new(0)),
        };
        let embedded = attach_actor(
            &service,
            campaign.session_root.path(),
            AgentPath("/root".into()),
            None,
            campaign.root_installation.clone(),
            Some("start".into()),
        )
        .await
        .unwrap();
        let conversation = Arc::clone(&embedded.conversation);
        let cancellation = embedded.cancellation;
        let (lifecycle, _lifecycle_rx) =
            tokio::sync::watch::channel(harness::server::HostActorLifecycle::Waiting);
        let settings_for_engine = settings.clone();
        let runtime = Arc::clone(&service.runtime);
        let transport_for_engine = transport.clone();
        let mut running = tokio::spawn(async move {
            drive_conversation_with_transport::<Offline, _>(
                embedded.driver,
                runtime,
                &settings_for_engine,
                "offline".into(),
                Effort::Medium,
                "resident test".into(),
                embedded.cancellation_rx,
                lifecycle,
                transport_for_engine,
            )
            .await
        });
        tokio::select! {
            result = &mut running => panic!("Engine stopped before completing its Haskell call: {result:?}"),
            ready = tokio::time::timeout(Duration::from_secs(30), transport.entered.notified()) => {
                ready.expect("resident Haskell call did not settle before the next model request");
            }
        }
        let origin = format!("https://{}", service.address);
        let api = format!("http://{}/api", service.address);
        let client = reqwest::Client::new();
        let login = client
            .post(format!("{api}/session"))
            .header("Origin", &origin)
            .json(&serde_json::json!({"secret": secret}))
            .send()
            .await
            .unwrap();
        assert_eq!(login.status(), reqwest::StatusCode::OK);
        let set_cookie = login.headers()[reqwest::header::SET_COOKIE]
            .to_str()
            .unwrap();
        for flag in ["HttpOnly", "SameSite=Strict", "Max-Age=28800", "Secure"] {
            assert!(set_cookie.split(';').any(|part| part.trim() == flag));
        }
        let cookie = set_cookie.split(';').next().unwrap().to_owned();
        let response = client
            .post(format!("{api}/commands"))
            .header("Origin", &origin)
            .header(reqwest::header::COOKIE, &cookie)
            .json(&harness::server::ClientCommand::Submit {
                command: "wake me".into(),
            })
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
        let command = service.commands.recv().await.unwrap();
        let receipt = submit_browser_command(command, &conversation, &service.control)
            .await
            .unwrap();
        assert!(receipt.wake_error.is_none(), "{receipt:?}");
        transport.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), transport.completed.notified())
            .await
            .unwrap();
        cancellation.send_replace(true);
        let engine_result = tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap();
        match engine_result {
            Ok(()) => {}
            Err(error) if error == "engine cancelled" => {}
            Err(error) => panic!("embedded Engine failed: {error}"),
        }
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(transport.compactions.load(Ordering::Relaxed), 1);
        assert_eq!(
            requests[2]
                .input
                .iter()
                .filter(|item| item.0["content"] == "wake me")
                .count(),
            1
        );
        assert!(
            requests[1]
                .input
                .iter()
                .any(|item| item.0.to_string().contains("42")),
            "settled Haskell result was not present in the next request: {:#?}",
            requests[1].input
        );
        assert!(
            requests[2]
                .input
                .iter()
                .any(|item| item.0.to_string().contains("42")),
            "raw-cell result was not retained: {:#?}",
            requests[2].input
        );
        assert!(matches!(
            conversation.input_observation(receipt.envelope_id).unwrap(),
            InputObservation::Included(_)
        ));
        drop(requests);
        publish_embedded_root_snapshot(
            &service.control,
            conversation.identity(),
            harness::server::HostActorLifecycle::Waiting,
        );
        for _ in 0..2 {
            let mut request = format!("ws://{}/api/ws", service.address)
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert("Origin", origin.parse().unwrap());
            request
                .headers_mut()
                .insert("Cookie", cookie.parse().unwrap());
            let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
            let frame: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            assert!(frame.get("snapshot").is_some(), "{frame}");
            assert_eq!(frame["snapshot"]["actors"].as_array().unwrap().len(), 1);
            assert_eq!(frame["snapshot"]["actors"][0]["modelConversation"], "/root");
            assert_eq!(frame["snapshot"]["conversations"][0]["path"], "/root");
            socket.close(None).await.unwrap();
        }
        service.shutdown().await.unwrap();
        drop(service);
        let rotated_secret = "rotated-embedded-browser-test-secret-32-bytes";
        std::fs::write(&settings.session_secret_file, rotated_secret).unwrap();
        let mut restarted = EmbeddedService::prepare(campaign.session_root.path(), &settings)
            .await
            .unwrap();
        let restarted_api = format!("http://{}/api", restarted.address);
        let restarted_origin = format!("https://{}", restarted.address);
        let stale = client
            .post(format!("{restarted_api}/commands"))
            .header("Origin", &restarted_origin)
            .header(reqwest::header::COOKIE, &cookie)
            .json(&harness::server::ClientCommand::Submit {
                command: "stale session".into(),
            })
            .send()
            .await
            .unwrap();
        assert_eq!(stale.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert!(restarted.commands.try_recv().is_err());
        for (secret, expected) in [
            (secret, reqwest::StatusCode::UNAUTHORIZED),
            (rotated_secret, reqwest::StatusCode::OK),
        ] {
            let login = client
                .post(format!("{restarted_api}/session"))
                .header("Origin", &restarted_origin)
                .json(&serde_json::json!({"secret": secret}))
                .send()
                .await
                .unwrap();
            assert_eq!(login.status(), expected);
        }
        restarted.shutdown().await.unwrap();
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
