use std::{num::NonZeroU64, path::Path, sync::Arc};

use exomonad_actor::LocalResidentInstallation;
#[cfg(test)]
use harness::server::ClientCommand;
use harness::{
    engine::EngineConfig,
    model::{AgentPath, Effort},
    server::{QueuedCommand, ServerConfig, ServerControl, SessionSecret},
    transport::{
        auth::{ChatGptPlanAuth, CodexFileAuth},
        Auth, AuthCredentials, ResponsesClient, ResponsesProtocol, ResponsesRoute, TransportError,
    },
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
use crate::exomonad::{EmbeddedBrowserAuth, EmbeddedLaunchConfig, EmbeddedModelProvider};

struct TailscaleBrowserAuth(exomonad_node::network::TailscalePeerVerifier);

#[async_trait::async_trait]
impl harness::server::BrowserPeerAuthenticator for TailscaleBrowserAuth {
    async fn authenticate(
        &self,
        peer: std::net::SocketAddr,
    ) -> Result<(), harness::server::PeerAuthError> {
        self.0
            .authorize_peer(peer)
            .await
            .map_err(|error| match error {
                exomonad_node::network::TailscalePeerError::Denied => {
                    harness::server::PeerAuthError::Denied
                }
                exomonad_node::network::TailscalePeerError::Unavailable => {
                    harness::server::PeerAuthError::Unavailable
                }
            })
    }
}

#[derive(Clone)]
pub(super) enum EmbeddedAuth {
    Codex(CodexFileAuth),
    ChatGptPlan(ChatGptPlanAuth),
}

impl Auth for EmbeddedAuth {
    fn access(&self) -> Result<(String, String), TransportError> {
        match self {
            Self::Codex(auth) => auth.access(),
            Self::ChatGptPlan(auth) => auth.access(),
        }
    }
    fn credentials(&self) -> Result<AuthCredentials, TransportError> {
        match self {
            Self::Codex(auth) => auth.credentials(),
            Self::ChatGptPlan(auth) => auth.credentials(),
        }
    }
    fn route(&self) -> ResponsesRoute {
        match self {
            Self::Codex(auth) => auth.route(),
            Self::ChatGptPlan(auth) => auth.route(),
        }
    }
}

pub(super) fn responses_client(settings: &EmbeddedLaunchConfig) -> ResponsesClient<EmbeddedAuth> {
    let auth_file = settings.credential_file.clone();
    match settings.provider {
        EmbeddedModelProvider::Codex => {
            // Native async calls must remain pending across model requests.
            // Responses Lite rejects async declarations and cannot carry them.
            ResponsesClient::new(EmbeddedAuth::Codex(CodexFileAuth::new(auth_file)))
                .with_protocol(ResponsesProtocol::Standard)
        }
        EmbeddedModelProvider::ChatGptPlan => {
            ResponsesClient::new(EmbeddedAuth::ChatGptPlan(ChatGptPlanAuth::new(auth_file)))
        }
    }
}

pub(super) struct EmbeddedService {
    _owner: Arc<super::HostIncarnationLease>,
    pub(super) settings: EmbeddedLaunchConfig,
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
        let server_config = ServerConfig::new(settings.asset_root.clone())
            .with_history_store(runtime.store())
            .with_public_origin_scheme(settings.public_origin_scheme.as_str())
            .map_err(str::to_owned)?;
        let server_config = match &settings.public_origin {
            Some(origin) => server_config
                .with_public_origin(origin.clone())
                .map_err(str::to_owned)?,
            None => server_config,
        };
        let server_config = match &settings.browser_auth {
            EmbeddedBrowserAuth::Secret => {
                let path = settings.session_secret_file.as_deref().ok_or_else(|| {
                    "embedded secret authentication requires session_secret_file".to_owned()
                })?;
                let secret = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
                let secret = SessionSecret::new(secret.trim_end_matches(['\r', '\n']).to_owned())
                    .map_err(str::to_owned)?;
                server_config
                    .with_browser_session(secret, std::time::Duration::from_secs(8 * 60 * 60))
                    .map_err(str::to_owned)?
            }
            EmbeddedBrowserAuth::Tailscale {
                allowed_user_ids,
                localapi_socket,
            } => {
                let verifier = exomonad_node::network::TailscalePeerVerifier::new(
                    localapi_socket.clone(),
                    allowed_user_ids.clone(),
                )
                .map_err(|error| error.to_string())?;
                server_config
                    .with_browser_peer_auth(Arc::new(TailscaleBrowserAuth(verifier)))
                    .map_err(str::to_owned)?
            }
        };
        let listener = TcpListener::bind(settings.listen)
            .await
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let (router, control, commands) = harness::server::server_with_config(server_config);
        runtime.configure_output_observer(control.clone())?;
        let server_owner = Arc::clone(&owner);
        let server = tokio::spawn(async move {
            let _server_owner = server_owner;
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .map_err(|error| error.to_string())
        });
        Ok(Self {
            _owner: owner,
            settings: settings.clone(),
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
        let owner = super::HostIncarnationLease::claim(
            &tidepool_atomic_write::DirectoryAnchor::open_existing(run_root)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
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
    if installation.spawn_admission.is_none() {
        let gate = installation.fork_gate.as_ref()
            .ok_or("embedded checkpoint child requires admitted authority")?;
        gate.wait_committed().await.map_err(|error| error.to_string())?;
        if gate.publication().map_err(|error| error.to_string())?
            == exomonad_actor::ForkGroupPublication::Deferred {
            if let Some(lease) = &installation.checkpoint {
                lease.wait_published().await
                    .map_err(|refusal| format!("checkpoint publication refused: {refusal:?}"))?;
            }
        }
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
        .attach_checkpoint(identity, &installation, policy, captured)
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
    if installation.spawn_admission.is_none() {
        let gate = installation.fork_gate.as_ref()
            .ok_or("selected embedded child requires admitted authority")?;
        gate.wait_committed().await.map_err(|error| error.to_string())?;
    }
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
    if let Some(prompt) = installation.fresh_context_seed.as_deref() {
        embedded.conversation.seed_context(
            &format!("spawn-context:{}:{}", actor.id.0, actor.incarnation.0), prompt,
        ).map_err(|error| error.to_string())?;
    }
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

#[derive(Debug, thiserror::Error)]
pub(super) enum EmbeddedDriverError {
    #[error(transparent)]
    Engine(#[from] harness::engine::EngineError),
    #[error("{0}")]
    Host(String),
}

impl From<String> for EmbeddedDriverError {
    fn from(detail: String) -> Self {
        Self::Host(detail)
    }
}

impl From<&str> for EmbeddedDriverError {
    fn from(detail: &str) -> Self {
        Self::Host(detail.into())
    }
}

impl EmbeddedDriverError {
    pub(super) fn cleanup_failed(&self) -> bool {
        matches!(
            self,
            Self::Engine(harness::engine::EngineError::Cleanup { .. })
        )
    }
}

fn cancelled_round(
    error: harness::engine::EngineError,
    previous_head: Option<harness::model::RequestId>,
) -> Result<
    (
        Option<harness::model::RequestId>,
        Option<EmbeddedDriverError>,
    ),
    EmbeddedDriverError,
> {
    let Some(head) = cancelled_head(&error) else {
        return Err(error.into());
    };
    let cleanup = matches!(&error, harness::engine::EngineError::Cleanup { .. })
        .then(|| EmbeddedDriverError::Engine(error));
    Ok((head.or(previous_head), cleanup))
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
) -> Result<(), EmbeddedDriverError> {
    drive_conversation_with_transport::<EmbeddedAuth, _>(
        embedded,
        runtime,
        settings,
        model,
        effort,
        instructions,
        cancellation,
        lifecycle,
        actor_ref,
        responses_client(settings),
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
) -> Result<(), EmbeddedDriverError>
where
    A: harness::transport::Auth,
    C: harness::engine::ResponsesTransport,
{
    let EmbeddedConversation {
        provider_admission,
        conversation,
        mut incoming,
        round_control,
        observation,
    } = embedded;
    let actor = conversation.identity().actor.clone();
    let provider_thread = format!(
        "{}:{}:{}",
        conversation.identity().run,
        actor.0,
        conversation.identity().incarnation
    );
    // The supervisor starts the driver only after accepting its attachment.
    observation.publish_provider_binding(None, provider_thread.clone());
    let engine = conversation
        .engine::<A, _>(
            transport,
            runtime.scheduler(),
            EngineConfig {
                instructions,
                tools: Vec::new(),
                model,
                effort,
                session_id: provider_thread.clone(),
                agent: actor.clone(),
            },
            NonZeroU64::new(settings.context_capacity_tokens).ok_or("zero context capacity")?,
        )
        .map_err(|error| error.to_string())?;
    let engine = match runtime.output_observer() {
        Some(observer) => engine.with_output_observer(observer),
        None => engine,
    };
    let store = runtime.store();
    let mut recovering = true;
    loop {
        if *cancellation.borrow() {
            return Ok(());
        }
        // Wakes are hints. A prior Engine round may have returned while a
        // forwarded hint remained in its receiver; the Store is authoritative.
        let frontier = store
            .embedded_round_frontier(conversation.identity())
            .map_err(|error| error.to_string())?;
        let first = store
            .unread(&actor.0)
            .map_err(|error| error.to_string())?
            .first()
            .map(|envelope| harness::mailbox::DurableMailboxWake {
                envelope_id: envelope.id,
            });
        if first.is_none()
            && (frontier.pending_interruption.is_some() || frontier.pending_head.is_none())
        {
            tokio::select! {
                biased;
                changed = cancellation.changed() => {
                    if changed.is_err() || *cancellation.borrow() { return Ok(()); }
                }
                wake = incoming.recv() => if wake.is_none() { return Ok(()); },
            }
            continue;
        }
        let head = frontier.settled_head;
        let (forward, forwarded) = mpsc::unbounded_channel();
        if let Some(first) = first {
            forward
                .send(first)
                .map_err(|_| "embedded Engine wake receiver closed")?;
        }
        let recovering_this_round = recovering || frontier.pending_head.is_some();
        let mut lifetime_stopped = false;
        let round = round_control
            .begin()
            .map_err(|error| format!("could not begin embedded Engine round: {error}"))?;
        let observed_round = match provider_admission
            .begin_provider_turn(provider_thread.clone(), round.id().0.to_string())
        {
            Ok(lease) => lease,
            Err(exomonad_actor::NativeProviderStartError::RetirementPending) => {
                drop(round);
                tokio::select! {
                    biased;
                    changed = cancellation.changed() => {
                        if changed.is_err() || *cancellation.borrow() { return Ok(()); }
                    }
                    () = provider_admission.wait_for_retirement_decision() => {}
                }
                continue;
            }
            Err(exomonad_actor::NativeProviderStartError::Closed) => return Ok(()),
            Err(error) => return Err(error.to_string().into()),
        };
        let result = {
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
        let (durable_head, interrupted, rejected, cleanup_failure) = match result {
            Ok(completion) => (Some(completion.head_request), false, false, None),
            Err(harness::engine::EngineError::RequestRejected { head_request, .. }) => {
                (Some(head_request), false, true, None)
            }
            Err(harness::engine::EngineError::InterruptedModelRound {
                head_request,
                cause,
            }) => {
                // Engine has confirmed cleanup and retained this pending head.
                // A stream failure alone never authorizes another provider
                // request. Durable explicit input triggers existing recovery,
                // which reconciles retained claims without dispatching them.
                tracing::warn!(?actor, ?head_request, %cause, "embedded provider round interrupted; awaiting explicit input");
                observed_round.fail(exomonad_model::ProviderFailure::TransportFailed);
                recovering = true;
                lifecycle.publish(actor_ref, harness::server::HostActorLifecycle::Waiting);
                continue;
            }
            Err(error) => {
                let (head, cleanup) = match cancelled_round(error, head.clone()) {
                    Ok(cancelled) => cancelled,
                    Err(error) => {
                        observed_round
                            .fail(exomonad_model::ProviderFailure::Other(error.to_string()));
                        return Err(error);
                    }
                };
                (head, true, false, cleanup)
            }
        };
        // A clean rejection already committed its exact failed head in Engine.
        let advanced = if rejected || durable_head == head {
            Ok(true)
        } else if let Some(durable_head) = &durable_head {
            store.settle_embedded_round(
                conversation.identity(),
                head.as_ref(),
                durable_head,
                if interrupted {
                    harness::store::EmbeddedRoundOutcome::Cancelled
                } else {
                    harness::store::EmbeddedRoundOutcome::Completed
                },
            )
        } else {
            Ok(false)
        };
        if let Some(error) = cleanup_failure {
            if !matches!(&advanced, Ok(true)) {
                tracing::warn!(
                    ?actor,
                    ?advanced,
                    "embedded head advance failed alongside Engine cleanup"
                );
            }
            observed_round.fail(exomonad_model::ProviderFailure::Other(error.to_string()));
            return Err(error);
        }
        if !advanced.map_err(|error| error.to_string())? {
            return Err(format!("embedded conversation {} lost its durable head", actor.0).into());
        }
        drop(round);
        // Provider idleness authorizes typed cleanup. Publish it only after
        // Engine settlement and the matching durable head transition.
        if interrupted {
            observed_round.interrupt();
        } else if rejected {
            observed_round.fail(exomonad_model::ProviderFailure::RequestRejected);
        } else {
            observed_round.succeed();
        }
        if lifetime_stopped || *cancellation.borrow() {
            return Ok(());
        }
        if interrupted {
            // The interrupted request and its settled claims are durable.
            // Reconcile exact claims before the next model request.
            recovering = true;
            lifecycle.publish(actor_ref, harness::server::HostActorLifecycle::Waiting);
            continue;
        }
        lifecycle.publish(actor_ref, harness::server::HostActorLifecycle::Waiting);
        // Cleanup may retain a settled tool output on an ancestor claim.
        // Reconcile the branch lineage before the next explicit-input request.
        recovering = rejected;
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

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    use harness::{engine::EngineError, model::RequestId};

    #[tokio::test]
    async fn embedded_provider_choice_routes_actor_and_cell_clients() {
        let source = "listen = '127.0.0.1:0'\nasset_root = '/tmp/assets'\nprovider = 'chatgpt_plan'\ncredential_file = '/tmp/exomonad-auth'\ncontext_capacity_tokens = 4096\n";
        let mut settings: EmbeddedLaunchConfig = toml::from_str(source).unwrap();
        let credentials = tempfile::tempdir().unwrap();
        settings.credential_file = credentials.path().join("absent.json");
        for (provider, route) in [
            (
                EmbeddedModelProvider::ChatGptPlan,
                ResponsesRoute::ChatGptPlan,
            ),
            (EmbeddedModelProvider::Codex, ResponsesRoute::Codex),
        ] {
            settings.provider = provider;
            let client = responses_client(&settings);
            assert_eq!(client.auth.route(), route);
            // Normalization happens before credential access. Lite rejects the
            // plan route, and rejects this native tool on the Codex route.
            // Authentication proves the selected Standard path normalized; the
            // deliberately missing file prevents network traffic.
            let tools = if route == ResponsesRoute::Codex {
                vec![serde_json::json!({"type": "tool_search"})]
            } else {
                Vec::new()
            };
            let result = client
                .create(harness::transport::ResponsesRequest {
                    input: Vec::new(),
                    instructions: String::new(),
                    tools: tools.into(),
                    tools_allowed: None,
                    model: "gpt-6.1-sol".into(),
                    pinned_effort: Effort::Medium,
                    session_id: "provider-constructor-test".into(),
                })
                .await;
            assert!(matches!(result, Err(TransportError::Authentication)));
        }
    }

    #[test]
    fn cancellation_retains_durable_head_and_distinguishes_cleanup_failure() {
        let previous = RequestId("previous".into());
        let (head, cleanup) = cancelled_round(
            EngineError::Cancelled { head_request: None },
            Some(previous.clone()),
        )
        .unwrap();
        assert_eq!(head, Some(previous.clone()));
        assert!(
            cleanup.is_none(),
            "confirmed cancellation is a successful stop"
        );

        let exact = RequestId("cancelled-request".into());
        let (head, cleanup) = cancelled_round(
            EngineError::Cleanup {
                primary: Box::new(EngineError::Cancelled {
                    head_request: Some(exact.clone()),
                }),
                cleanup: "outstanding call claim could not be settled".into(),
            },
            Some(previous),
        )
        .unwrap();
        assert_eq!(head, Some(exact));
        let error = cleanup.expect("cancellation must retain cleanup failure");
        assert!(error.cleanup_failed());
        assert!(
            matches!(error, EmbeddedDriverError::Engine(EngineError::Cleanup { cleanup, .. })
            if cleanup == "outstanding call claim could not be settled")
        );
    }

    #[test]
    fn generic_engine_error_remains_failure_during_cancellation() {
        let error = cancelled_round(EngineError::InvalidFunctionCall, None).unwrap_err();
        assert!(matches!(
            error,
            EmbeddedDriverError::Engine(EngineError::InvalidFunctionCall)
        ));
        assert!(!error.cleanup_failed());
    }
}

#[cfg(test)]
#[path = "embedded_restart_driver_tests.rs"]
mod restart_tests;
