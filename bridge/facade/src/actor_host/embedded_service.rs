use std::{num::NonZeroU64, path::Path, sync::Arc};

use exomonad_actor::LocalResidentInstallation;
#[cfg(test)]
use harness::server::ClientCommand;
use harness::{
    engine::EngineConfig,
    model::{AgentPath, Effort},
    server::{QueuedCommand, ServerConfig, ServerControl, SessionSecret},
    transport::{auth::CodexFileAuth, ResponsesClient},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
    task::JoinHandle,
};

use super::{
    embedded_harness::{EmbeddedConversation, EmbeddedHarnessRuntime},
    embedded_policy::EmbeddedPolicyInstallation,
    embedded_projection::LifecyclePublisher,
};
use crate::exomonad::EmbeddedLaunchConfig;

pub(super) struct EmbeddedService {
    _owner: Arc<super::HostIncarnationLease>,
    pub(super) runtime: Arc<EmbeddedHarnessRuntime>,
    pub(super) commands: mpsc::Receiver<QueuedCommand>,
    pub(super) control: ServerControl,
    server: Option<JoinHandle<Result<(), String>>>,
    pub(super) address: std::net::SocketAddr,
    #[cfg(test)]
    test_transport: Option<TestResponsesTransport>,
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct TestResponsesTransport(Arc<dyn harness::engine::ResponsesTransport>);

#[cfg(test)]
#[async_trait::async_trait]
impl harness::engine::ResponsesTransport for TestResponsesTransport {
    async fn create(
        &self,
        request: harness::transport::ResponsesRequest,
    ) -> Result<harness::transport::ResponsesTurn, harness::transport::TransportError> {
        self.0.create(request).await
    }

    async fn create_streaming(
        &self,
        request: harness::transport::ResponsesRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<harness::transport::ResponsesTurn, harness::transport::TransportError> {
        self.0.create_streaming(request, sink).await
    }

    async fn create_streaming_for_request(
        &self,
        request_id: &harness::model::RequestId,
        request: harness::transport::ResponsesRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<harness::transport::ResponsesTurn, harness::transport::TransportError> {
        self.0
            .create_streaming_for_request(request_id, request, sink)
            .await
    }
}

pub(super) struct EmbeddedActor {
    pub(super) conversation: Arc<harness::embedding::Conversation>,
    pub(super) driver: EmbeddedConversation,
    pub(super) cancellation: watch::Sender<bool>,
    pub(super) cancellation_rx: watch::Receiver<bool>,
}

impl EmbeddedService {
    /// Bind the browser and open the sole run Store before admitting an actor.
    pub(super) async fn prepare_owned(
        run_root: &Path,
        settings: &EmbeddedLaunchConfig,
        owner: Arc<super::HostIncarnationLease>,
    ) -> Result<Self, String> {
        if !owner
            .owns_run(run_root)
            .map_err(|error| error.to_string())?
        {
            return Err("embedded Store recovery requires ownership of this exact run".into());
        }
        settings.validate().map_err(|error| error.to_string())?;
        let runtime = Arc::new(
            EmbeddedHarnessRuntime::open(run_root, settings.concurrent_jobs)
                .map_err(|error| error.to_string())?,
        );
        runtime
            .store()
            .recover_embedded_command_claims(&super::runtime_namespace(run_root))
            .map_err(|error| error.to_string())?;
        let secret = std::fs::read_to_string(&settings.session_secret_file)
            .map_err(|error| error.to_string())?;
        let secret = SessionSecret::new(secret.trim_end_matches(['\r', '\n']).to_owned())
            .map_err(str::to_owned)?;
        let server_config = ServerConfig::new(settings.asset_root.clone())
            .with_history_store(runtime.store())
            .with_browser_session(secret, std::time::Duration::from_secs(8 * 60 * 60))
            .map_err(str::to_owned)?
            .with_public_origin_scheme(settings.public_origin_scheme.as_str())
            .map_err(str::to_owned)?;
        let listener = TcpListener::bind(settings.listen)
            .await
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let (router, control, commands) = harness::server::server_with_config(server_config);
        let server_owner = Arc::clone(&owner);
        let server = tokio::spawn(async move {
            let _server_owner = server_owner;
            axum::serve(listener, router)
                .await
                .map_err(|error| error.to_string())
        });
        Ok(Self {
            _owner: owner,
            runtime,
            commands,
            control,
            server: Some(server),
            address,
            #[cfg(test)]
            test_transport: None,
        })
    }

    #[cfg(test)]
    pub(super) async fn prepare(
        run_root: &Path,
        settings: &EmbeddedLaunchConfig,
    ) -> Result<Self, String> {
        let owner =
            super::HostIncarnationLease::claim(run_root).map_err(|error| error.to_string())?;
        Self::prepare_owned(run_root, settings, Arc::new(owner)).await
    }

    #[cfg(test)]
    pub(super) fn set_test_transport(
        &mut self,
        transport: Arc<dyn harness::engine::ResponsesTransport>,
    ) {
        self.test_transport = Some(TestResponsesTransport(transport));
    }

    #[cfg(test)]
    pub(super) fn test_transport(&self) -> Option<TestResponsesTransport> {
        self.test_transport.clone()
    }

    pub(super) fn server_finished(&self) -> bool {
        self.server.as_ref().is_some_and(JoinHandle::is_finished)
    }

    pub(super) async fn shutdown(&mut self) -> Result<(), String> {
        let Some(server) = self.server.take() else {
            return Ok(());
        };
        if !server.is_finished() {
            server.abort();
        }
        match server.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(error),
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

pub(super) async fn attach_actor(
    service: &EmbeddedService,
    run_root: &Path,
    path: AgentPath,
    parent: Option<AgentPath>,
    installation: LocalResidentInstallation,
    initial_input: Option<String>,
) -> Result<EmbeddedActor, String> {
    let actor = installation.actor.identity();
    let identity = harness::embedding::HostIdentity {
        run: super::runtime_namespace(run_root),
        actor: path,
        incarnation: actor.incarnation.0.to_string(),
    };
    let policy = Arc::new(EmbeddedPolicyInstallation::from_installation(&installation));
    let embedded = service
        .runtime
        .attach(
            identity,
            installation.actor.clone(),
            policy,
            parent.as_ref(),
        )
        .map_err(|error| error.to_string())?;
    if let Some(input) = initial_input.filter(|_| service.runtime.admit_initial_input(actor)) {
        embedded
            .conversation
            .input(
                &format!("launch:{}:{}", actor.id.0, actor.incarnation.0),
                "operator",
                &input,
            )
            .await
            .map_err(|error| error.to_string())?;
    }
    let conversation = Arc::clone(&embedded.conversation);
    let (cancel, cancellation) = watch::channel(false);
    Ok(EmbeddedActor {
        conversation,
        driver: embedded,
        cancellation: cancel,
        cancellation_rx: cancellation,
    })
}

pub(super) async fn attach_checkpoint_actor(
    runtime: &EmbeddedHarnessRuntime,
    run_root: &Path,
    installation: LocalResidentInstallation,
    initial_input: Option<String>,
) -> Result<EmbeddedActor, String> {
    let actor = installation.actor.identity();
    let lease = installation
        .checkpoint
        .as_ref()
        .ok_or("embedded child requires a checkpoint")?;
    if installation.context_parent != Some(lease.issuer) {
        return Err("embedded child context parent does not match checkpoint issuer".into());
    }
    let gate = installation
        .fork_gate
        .as_ref()
        .ok_or("embedded checkpoint child requires its admitted fork gate")?;
    gate.wait_committed()
        .await
        .map_err(|error| error.to_string())?;
    if gate.publication().map_err(|error| error.to_string())?
        == exomonad_actor::ForkGroupPublication::Deferred
    {
        lease
            .wait_published()
            .await
            .map_err(|refusal| format!("checkpoint publication refused: {refusal:?}"))?;
    }
    let captured = installation
        .checkpoint_attachment
        .as_ref()
        .and_then(|attachment| {
            attachment.downcast::<super::embedded_harness::EmbeddedHostedCheckpoint>()
        })
        .ok_or("embedded child requires its admitted hosted checkpoint attachment")?;
    let path = captured.child_path(actor);
    let identity = harness::embedding::HostIdentity {
        run: super::runtime_namespace(run_root),
        actor: path,
        incarnation: actor.incarnation.0.to_string(),
    };
    let policy = Arc::new(EmbeddedPolicyInstallation::from_installation(&installation));
    let embedded = runtime
        .attach_checkpoint(
            identity,
            installation.actor.clone(),
            policy,
            lease,
            gate,
            captured,
        )
        .map_err(|error| error.to_string())?;
    if let Some(input) = initial_input.filter(|_| runtime.admit_initial_input(actor)) {
        embedded
            .conversation
            .input(
                &format!("launch:{}:{}", actor.id.0, actor.incarnation.0),
                "operator",
                &input,
            )
            .await
            .map_err(|error| error.to_string())?;
    }
    let conversation = Arc::clone(&embedded.conversation);
    let (cancel, cancellation) = watch::channel(false);
    Ok(EmbeddedActor {
        conversation,
        driver: embedded,
        cancellation: cancel,
        cancellation_rx: cancellation,
    })
}

pub(super) async fn attach_selected_actor(
    runtime: &EmbeddedHarnessRuntime,
    run_root: &Path,
    parent: super::embedded_context::SelectedProviderParent,
    installation: LocalResidentInstallation,
    initial_input: Option<String>,
) -> Result<EmbeddedActor, String> {
    if installation.checkpoint.is_some() || installation.context_parent.is_some() {
        return Err("selected embedded child requires explicit fresh context".into());
    }
    let gate = installation
        .fork_gate
        .as_ref()
        .ok_or("selected embedded child requires its admitted fork gate")?;
    gate.wait_committed()
        .await
        .map_err(|error| error.to_string())?;
    let actor = installation.actor.identity();
    let run = super::runtime_namespace(run_root);
    if parent.identity().run != run {
        return Err("selected provider ancestor belongs to another run".into());
    }
    let identity = harness::embedding::HostIdentity {
        run,
        actor: parent.child_path(actor),
        incarnation: actor.incarnation.0.to_string(),
    };
    let policy = Arc::new(EmbeddedPolicyInstallation::from_installation(&installation));
    let embedded = runtime
        .attach(
            identity,
            installation.actor.clone(),
            policy,
            Some(&parent.identity().actor),
        )
        .map_err(|error| error.to_string())?;
    if let Some(input) = initial_input.filter(|_| runtime.admit_initial_input(actor)) {
        embedded
            .conversation
            .input(
                &format!("launch:{}:{}", actor.id.0, actor.incarnation.0),
                "operator",
                &input,
            )
            .await
            .map_err(|error| error.to_string())?;
    }
    let conversation = Arc::clone(&embedded.conversation);
    let (cancel, cancellation_rx) = watch::channel(false);
    Ok(EmbeddedActor {
        conversation,
        driver: embedded,
        cancellation: cancel,
        cancellation_rx,
    })
}

impl Drop for EmbeddedService {
    fn drop(&mut self) {
        if let Some(server) = &self.server {
            server.abort();
        }
    }
}

pub(super) async fn drive_conversation(
    embedded: EmbeddedConversation,
    runtime: Arc<EmbeddedHarnessRuntime>,
    settings: &EmbeddedLaunchConfig,
    model: String,
    effort: Effort,
    instructions: String,
    cancellation: watch::Receiver<bool>,
    lifecycle: impl LifecyclePublisher,
    actor_ref: exomonad_actor::ActorRef,
) -> Result<(), String> {
    drive_conversation_with_transport::<CodexFileAuth, _>(
        embedded,
        runtime,
        settings,
        model,
        effort,
        instructions,
        cancellation,
        lifecycle,
        actor_ref,
        ResponsesClient::new(CodexFileAuth::new(settings.codex_auth_file.clone())),
    )
    .await
}

pub(super) async fn drive_conversation_with_transport<A, C>(
    embedded: EmbeddedConversation,
    runtime: Arc<EmbeddedHarnessRuntime>,
    settings: &EmbeddedLaunchConfig,
    model: String,
    effort: Effort,
    instructions: String,
    mut cancellation: watch::Receiver<bool>,
    lifecycle: impl LifecyclePublisher,
    actor_ref: exomonad_actor::ActorRef,
    transport: C,
) -> Result<(), String>
where
    A: harness::transport::Auth,
    C: harness::engine::ResponsesTransport,
{
    let EmbeddedConversation {
        conversation,
        mut incoming,
        round_control,
        ..
    } = embedded;
    let actor = conversation.identity().actor.clone();
    let engine = conversation
        .engine::<A, _>(
            transport,
            runtime.scheduler(),
            EngineConfig {
                instructions,
                tools: Vec::new(),
                model,
                effort,
                session_id: format!(
                    "{}:{}:{}",
                    conversation.identity().run,
                    actor.0,
                    conversation.identity().incarnation
                ),
                agent: actor.clone(),
            },
            NonZeroU64::new(settings.context_capacity_tokens).ok_or("zero context capacity")?,
        )
        .map_err(|error| error.to_string())?;
    let store = runtime.store();
    let mut recovering = true;
    loop {
        if *cancellation.borrow() {
            return Ok(());
        }
        // Wakes are hints. A prior Engine round may have returned while a
        // forwarded hint remained in its receiver; the Store is authoritative.
        let first = match store
            .unread(&actor.0)
            .map_err(|error| error.to_string())?
            .first()
        {
            Some(envelope) => harness::mailbox::DurableMailboxWake {
                envelope_id: envelope.id,
            },
            None => tokio::select! {
                biased;
                changed = cancellation.changed() => {
                    if changed.is_err() || *cancellation.borrow() { return Ok(()); }
                    continue;
                }
                wake = incoming.recv() => match wake { Some(_) => continue, None => return Ok(()) },
            },
        };
        let head = store
            .agent(&actor)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("missing embedded agent {}", actor.0))?
            .head_request;
        let (forward, forwarded) = mpsc::unbounded_channel();
        forward
            .send(first)
            .map_err(|_| "embedded Engine wake receiver closed")?;
        let recovering_this_round = recovering;
        let mut lifetime_stopped = false;
        let result = {
            let round = round_control
                .begin()
                .map_err(|error| format!("could not begin embedded Engine round: {error}"))?;
            lifecycle.publish(actor_ref, harness::server::HostActorLifecycle::Running);
            let run = async {
                if recovering_this_round {
                    engine
                        .run_recovering_embedded(
                            head.clone(),
                            Vec::new(),
                            round.cancellation(),
                            forwarded,
                        )
                        .await
                } else {
                    engine
                        .run_embedded(head.clone(), Vec::new(), round.cancellation(), forwarded)
                        .await
                }
            };
            tokio::pin!(run);
            let mut incoming_closed = false;
            loop {
                tokio::select! {
                    biased;
                    changed = cancellation.changed() => {
                        if changed.is_err() || *cancellation.borrow() {
                            lifetime_stopped = true;
                            round.cancel();
                            break run.await;
                        }
                    }
                    result = &mut run => break result,
                    wake = incoming.recv(), if !incoming_closed => match wake {
                        Some(wake) => { forward.send(wake).ok(); }
                        None => { incoming_closed = true; }
                    },
                }
            }
        };
        let interrupted = matches!(&result, Err(error) if cancelled_head(error).is_some());
        let cleanup_failure = match &result {
            Err(error @ harness::engine::EngineError::Cleanup { primary, .. })
                if cancelled_head(primary).is_some() =>
            {
                Some(error.to_string())
            }
            _ => None,
        };
        let rejected = matches!(
            &result,
            Err(harness::engine::EngineError::RequestRejected { .. })
        );
        let durable_head = match result {
            Ok(completion) => Some(completion.head_request),
            Err(harness::engine::EngineError::RequestRejected { head_request, .. }) => {
                Some(head_request)
            }
            Err(error) => match cancelled_head(&error) {
                Some(request_head) => request_head.or_else(|| head.clone()),
                None => return Err(error.to_string()),
            },
        };
        if !rejected
            && !store
                .advance_agent_head(&actor, head.as_ref(), durable_head.as_ref())
                .map_err(|error| error.to_string())?
        {
            return Err(format!(
                "embedded conversation {} lost its durable head",
                actor.0
            ));
        }
        if lifetime_stopped || *cancellation.borrow() {
            return Err("engine cancelled".into());
        }
        if interrupted {
            if let Some(error) = cleanup_failure {
                return Err(error);
            }
            // The interrupted request and its settled claims are durable.
            // Reconcile exact claims before the next model request.
            recovering = true;
            lifecycle.publish(actor_ref, harness::server::HostActorLifecycle::Waiting);
            continue;
        }
        lifecycle.publish(actor_ref, harness::server::HostActorLifecycle::Waiting);
        recovering = false;
    }
}

fn cancelled_head(
    error: &harness::engine::EngineError,
) -> Option<Option<harness::model::RequestId>> {
    match error {
        harness::engine::EngineError::Cancelled { head_request } => Some(head_request.clone()),
        harness::engine::EngineError::Cleanup { primary, .. } => cancelled_head(primary),
        _ => None,
    }
}

#[cfg(test)]
pub(super) async fn submit_browser_command(
    command: QueuedCommand,
    root: &harness::embedding::Conversation,
    control: &ServerControl,
) -> Result<harness::embedding::InputReceipt, String> {
    match command.command {
        ClientCommand::Submit { command: text } => {
            let receipt = root
                .input(&command.command_id, "browser", &text)
                .await
                .map_err(|error| error.to_string())?;
            control.publish_command_receipt(harness::server::CommandReceipt {
                command_id: command.command_id,
                outcome: harness::server::CommandReceiptOutcome::Admitted {
                    target: None,
                    envelope_id: receipt.envelope_id.to_string(),
                    wake_error: receipt.wake_error.clone(),
                },
            });
            Ok(receipt)
        }
        ClientCommand::Host { .. } => {
            Err("targeted host commands must be routed through the embedded actor owner".into())
        }
    }
}
