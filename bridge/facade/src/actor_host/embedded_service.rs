use std::{num::NonZeroU64, path::Path, sync::Arc};

use exomonad_actor::LocalResidentInstallation;
#[cfg(test)]
use harness::transport::auth::ChatGptPlanAuth;
use harness::{
    engine::EngineConfig,
    model::{AgentPath, Effort},
    server::{QueuedCommand, ServerConfig, ServerControl, SessionSecret},
    transport::{auth::chatgpt::RunAccountAuth, ResponsesClient},
};

#[cfg(test)]
use harness::{
    server::ClientCommand,
    transport::{ResponsesProtocol, ResponsesRoute, TransportError},
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
use crate::exomonad::{EmbeddedBrowserAuth, EmbeddedLaunchConfig};

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

pub(super) fn responses_client(
    runtime: &EmbeddedHarnessRuntime,
) -> ResponsesClient<RunAccountAuth> {
    ResponsesClient::new(runtime.account_auth().clone())
}

pub(super) struct EmbeddedService {
    _owner: Arc<super::HostIncarnationLease>,
    pub(super) settings: EmbeddedLaunchConfig,
    pub(super) runtime: Arc<EmbeddedHarnessRuntime>,
    pub(super) commands: mpsc::Receiver<QueuedCommand>,
    pub(super) control: ServerControl,
    server: EmbeddedServer,
    pub(super) address: std::net::SocketAddr,
    #[cfg(test)]
    test_transport: Option<TestResponsesTransport>,
}

enum EmbeddedServer {
    Pending {
        task: JoinHandle<Result<(), String>>,
        phase: ServerPhase,
    },
    Settled(Result<(), String>),
}

#[derive(Clone, Copy)]
enum ServerPhase {
    Running,
    Aborting,
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct TestResponsesTransport(Arc<dyn harness::engine::ResponsesTransport>);

#[cfg(test)]
#[async_trait::async_trait]
impl harness::engine::ResponsesTransport for TestResponsesTransport {
    fn response_contract(&self) -> Result<(ResponsesRoute, ResponsesProtocol), TransportError> {
        self.0.response_contract()
    }

    fn prepare_submission(
        &self,
    ) -> Result<harness::transport::PreparedTransportSelection, TransportError> {
        self.0.prepare_submission()
    }

    async fn create(
        &self,
        request: harness::transport::PreparedProviderRequest,
    ) -> Result<harness::transport::ResponsesTurn, harness::transport::TransportError> {
        self.0.create(request).await
    }

    async fn create_streaming(
        &self,
        request: harness::transport::PreparedProviderRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<harness::transport::ResponsesTurn, harness::transport::TransportError> {
        self.0.create_streaming(request, sink).await
    }
}

pub(super) struct EmbeddedActor {
    pub(super) initial_context_ready: InitialContextReady,
    pub(super) conversation: Arc<harness::embedding::Conversation>,
    pub(super) driver: EmbeddedConversation,
    pub(super) cancellation: watch::Sender<bool>,
    pub(super) cancellation_rx: watch::Receiver<bool>,
}

/// Issued only after the preparation owner and publication owner have settled.
/// Conversation inputs and provider driving consume this launch boundary.
#[derive(Clone)]
pub(super) struct InitialContextReady {
    conversation: Arc<harness::embedding::Conversation>,
}

impl InitialContextReady {
    pub(super) fn into_conversation(self) -> Arc<harness::embedding::Conversation> {
        self.conversation
    }
}

async fn initialize_context(
    attachment: &super::provider_attachment::ProviderAttachment,
    conversation: &Arc<harness::embedding::Conversation>,
) -> Result<InitialContextReady, String> {
    use exomonad_actor::InitialContextState;
    let observed = attachment
        .prepare_initial_context()
        .await
        .map_err(|error| error.to_string())?;
    match observed.state {
        InitialContextState::Captured => {}
        InitialContextState::Published => {
            let identity = observed
                .identity
                .ok_or("published startup has no operation identity")?;
            let original = observed
                .publication
                .ok_or("published startup has no publication evidence")?;
            let result = observed
                .result
                .ok_or("published startup has no retained result")?;
            let issuer =
                super::embedded_recovery::identity_from_conversation(&original.conversation)?;
            let publication = conversation
                .initial_context_publication(&issuer, identity.operation.as_str())
                .map_err(|error| error.to_string())?
                .ok_or("published startup has no exact Harness receipt")?;
            if publication.request.0 != original.request
                || publication
                    .caller_reference
                    .as_ref()
                    .map(|reference| reference.as_str())
                    != original.caller_reference.as_deref()
                || publication
                    .authored_reference
                    .as_ref()
                    .map(|reference| reference.as_str())
                    != original.authored_reference.as_deref()
                || publication.caller_seed.as_deref() != observed.caller_seed.as_deref()
                || publication.authored_seed.as_deref()
                    != (!result.rendered.is_empty()).then_some(result.rendered.as_str())
            {
                return Err(
                    "published startup differs from its exact historical Harness receipt".into(),
                );
            }
        }
        InitialContextState::AbsentHook => {
            if let Some(caller) = observed.caller_seed.as_deref() {
                let actor = attachment.actor();
                conversation
                    .seed_context(
                        &format!("spawn-context:{}:{}", actor.id.0, actor.incarnation.0),
                        caller,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        InitialContextState::ResultRetained => {
            let identity = observed
                .identity
                .ok_or("initial context retained result has no operation identity")?;
            let result = observed
                .result
                .ok_or("initial context retained result is absent")?;
            let caller = observed.caller_seed.as_deref();
            let authored = (!result.rendered.is_empty()).then_some(result.rendered.as_str());
            let issuer = harness::embedding::HostIdentity {
                run: conversation.identity().run.clone(),
                actor: conversation.identity().actor.clone(),
                incarnation: identity.actor.incarnation.0.to_string(),
            };
            let publication = match conversation
                .initial_context_publication(&issuer, identity.operation.as_str())
                .map_err(|error| error.to_string())?
            {
                Some(publication) => publication,
                None => conversation
                    .seed_initial_context(identity.operation.as_str(), caller, authored)
                    .await
                    .map_err(|error| error.to_string())?,
            };
            if publication.caller_seed.as_deref() != caller
                || publication.authored_seed.as_deref() != authored
            {
                return Err(
                    "initial context publication differs from its exact retained result".into(),
                );
            }
            attachment
                .acknowledge_initial_context_publication(
                    &identity.operation,
                    exomonad_actor::InitialContextPublication {
                        conversation: super::embedded_recovery::conversation(&publication.issuer),
                        request: publication.request.0,
                        caller_reference: publication
                            .caller_reference
                            .map(|reference| reference.as_str().to_owned()),
                        authored_reference: publication
                            .authored_reference
                            .map(|reference| reference.as_str().to_owned()),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        state => {
            return Err(format!(
                "initial context is not ready for inference: {state:?}"
            ));
        }
    }
    attachment
        .validate()
        .await
        .map_err(|error| error.to_string())?;
    Ok(InitialContextReady {
        conversation: Arc::clone(conversation),
    })
}

impl EmbeddedService {
    pub(super) fn projection_target(
        &self,
        model: String,
    ) -> Result<harness::context::ContextProjectionTarget, String> {
        use harness::engine::ResponsesTransport;
        #[cfg(test)]
        let contract = match &self.test_transport {
            Some(transport) => transport.response_contract(),
            None => responses_client(&self.runtime).response_contract(),
        };
        #[cfg(not(test))]
        let contract = responses_client(&self.runtime).response_contract();
        let (route, protocol) = contract.map_err(|error| error.to_string())?;
        Ok(harness::context::ContextProjectionTarget {
            model,
            route,
            protocol,
        })
    }

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
        let browser_identity =
            harness::server::BrowserBundleIdentity::from_declared_source(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/harness-browser-source-identity.json"
            )))
            .map_err(|error| error.to_string())?;
        let browser_bundle =
            harness::server::verify_browser_bundle(&settings.asset_root, &browser_identity)
                .map_err(|error| error.to_string())?;
        let runtime = Arc::new(
            EmbeddedHarnessRuntime::open(run_root, settings.concurrent_jobs)
                .map_err(|error| error.to_string())?,
        );
        runtime.configure_accounts(settings.credential_file.clone())?;
        // Only this exclusive host startup can prove native continuations
        // from the previous process no longer exist. Ordinary Store reads
        // must preserve live mounted forms.
        runtime
            .store()
            .interrupt_actor_forms_for_restart()
            .map_err(|error| error.to_string())?;
        runtime
            .store()
            .recover_embedded_command_claims(&super::runtime_namespace(run_root))
            .map_err(|error| error.to_string())?;
        runtime
            .store()
            .recover_embedded_inference_after_host_loss(&super::runtime_namespace(run_root))
            .map_err(|error| error.to_string())?;
        let server_config = ServerConfig::new(settings.asset_root.clone())
            .with_browser_bundle(browser_bundle)
            .with_history_store(runtime.store())
            .with_account_auth(runtime.account_auth().clone())
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
        control.publish(
            harness::server::browser_contract::StateEvent::AccountSelectionUpsert(
                runtime
                    .account_auth()
                    .selection()
                    .map_err(|e| e.to_string())?,
            ),
        );
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
            server: EmbeddedServer::Pending {
                task: server,
                phase: ServerPhase::Running,
            },
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
        match &self.server {
            EmbeddedServer::Pending { task, .. } => task.is_finished(),
            EmbeddedServer::Settled(_) => true,
        }
    }

    pub(super) async fn shutdown(&mut self) -> Result<(), String> {
        let (task, phase) = match &mut self.server {
            EmbeddedServer::Pending { task, phase } => (task, phase),
            EmbeddedServer::Settled(result) => return result.clone(),
        };
        if matches!(phase, ServerPhase::Running) && !task.is_finished() {
            task.abort();
            *phase = ServerPhase::Aborting;
        }
        // Borrow the retained handle so cancelling an observer cannot detach
        // the task. Publication follows the join without another await.
        let result = match task.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(error.to_string()),
        };
        self.server = EmbeddedServer::Settled(result.clone());
        result
    }
}

pub(super) async fn attach_actor(
    service: &EmbeddedService,
    attachment: super::provider_attachment::ProviderAttachment,
    run_root: &Path,
    path: AgentPath,
    parent: Option<harness::embedding::HostIdentity>,
    installation: LocalResidentInstallation,
    initial_input: Option<String>,
) -> Result<EmbeddedActor, String> {
    attach_root_actor(
        service.runtime.as_ref(),
        attachment,
        run_root,
        path,
        parent,
        installation,
        initial_input,
        None,
    )
    .await
}

pub(super) async fn attach_root_actor(
    runtime: &EmbeddedHarnessRuntime,
    attachment: super::provider_attachment::ProviderAttachment,
    run_root: &Path,
    path: AgentPath,
    parent: Option<harness::embedding::HostIdentity>,
    installation: LocalResidentInstallation,
    initial_input: Option<String>,
    binding: Option<&super::embedded_harness::EmbeddedActorBinding>,
) -> Result<EmbeddedActor, String> {
    let actor = installation.actor.identity();
    let identity = harness::embedding::HostIdentity {
        run: super::runtime_namespace(run_root),
        actor: path,
        incarnation: actor.incarnation.0.to_string(),
    };
    let policy = Arc::new(EmbeddedPolicyInstallation::from_installation(&installation));
    let embedded = runtime
        .attach(
            attachment.clone(),
            identity,
            installation.actor.clone(),
            policy,
            parent.as_ref(),
        )
        .await
        .map_err(|error| error.to_string())?;
    if let Some(binding) = binding {
        binding
            .stage_conversation(Arc::clone(&embedded.conversation))
            .map_err(|error| format!("startup conversation staging refused: {error:?}"))?;
    }
    let initial_context_ready = initialize_context(&attachment, &embedded.conversation).await?;
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
        initial_context_ready,
        conversation,
        driver: embedded,
        cancellation: cancel,
        cancellation_rx: cancellation,
    })
}

pub(super) async fn attach_checkpoint_actor(
    runtime: &EmbeddedHarnessRuntime,
    projection_target: harness::context::ContextProjectionTarget,
    attachment: super::provider_attachment::ProviderAttachment,
    run_root: &Path,
    installation: LocalResidentInstallation,
    initial_input: Option<String>,
    binding: &super::embedded_harness::EmbeddedActorBinding,
) -> Result<EmbeddedActor, String> {
    let actor = installation.actor.identity();
    installation
        .spawn_admission
        .as_ref()
        .ok_or("embedded child requires independent spawn admission")?
        .validate_child(actor)?;

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
            attachment.clone(),
            identity,
            &installation,
            policy,
            captured,
            &projection_target,
        )
        .await
        .map_err(|error| error.to_string())?;
    binding
        .stage_conversation(Arc::clone(&embedded.conversation))
        .map_err(|error| format!("startup conversation staging refused: {error:?}"))?;
    let initial_context_ready = initialize_context(&attachment, &embedded.conversation).await?;
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
        initial_context_ready,
        conversation,
        driver: embedded,
        cancellation: cancel,
        cancellation_rx: cancellation,
    })
}

pub(super) async fn attach_selected_actor(
    runtime: &EmbeddedHarnessRuntime,
    attachment: super::provider_attachment::ProviderAttachment,
    run_root: &Path,
    parent: super::embedded_context::SelectedProviderParent,
    installation: LocalResidentInstallation,
    initial_input: Option<String>,
    binding: &super::embedded_harness::EmbeddedActorBinding,
) -> Result<EmbeddedActor, String> {
    if installation.checkpoint.is_some() || installation.context_parent.is_some() {
        return Err("selected embedded child requires explicit fresh context".into());
    }

    let actor = installation.actor.identity();
    installation
        .spawn_admission
        .as_ref()
        .ok_or("embedded child requires independent spawn admission")?
        .validate_child(actor)?;

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
            attachment.clone(),
            identity,
            installation.actor.clone(),
            policy,
            Some(parent.identity()),
        )
        .await
        .map_err(|error| error.to_string())?;
    binding
        .stage_conversation(Arc::clone(&embedded.conversation))
        .map_err(|error| format!("startup conversation staging refused: {error:?}"))?;
    let initial_context_ready = initialize_context(&attachment, &embedded.conversation).await?;
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
        initial_context_ready,
        conversation,
        driver: embedded,
        cancellation: cancel,
        cancellation_rx,
    })
}

impl Drop for EmbeddedService {
    fn drop(&mut self) {
        if let EmbeddedServer::Pending { task, .. } = &self.server {
            task.abort();
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum EmbeddedDriverError {
    #[error(transparent)]
    Round(Arc<harness::engine::EmbeddedRoundFailure>),
    #[error(transparent)]
    NativeAdmission(#[from] exomonad_actor::NativeProviderStartError),
    #[error("provider settlement failed after settled Engine action: {source}")]
    ProviderAndRoundSettlement {
        #[source]
        source: exomonad_actor::NativeProviderSettlementError,
        report: Box<harness::engine::EmbeddedRoundSettlement>,
    },
    #[error("provider settlement failed: {source}; primary outcome: {primary}")]
    ProviderAndRoundFailure {
        #[source]
        source: exomonad_actor::NativeProviderSettlementError,
        primary: Arc<harness::engine::EmbeddedRoundFailure>,
    },
    #[error("{0}")]
    Host(String),
}

impl From<harness::engine::EmbeddedRoundFailure> for EmbeddedDriverError {
    fn from(error: harness::engine::EmbeddedRoundFailure) -> Self {
        Self::Round(Arc::new(error))
    }
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
    pub(super) fn requires_recovery(&self) -> bool {
        match self {
            Self::Round(error) => {
                error.assessment().store_continuation()
                    == harness::engine::StoreContinuation::ReopenRequired
            }
            Self::NativeAdmission(error) => {
                matches!(error, exomonad_actor::NativeProviderStartError::Native(_))
            }
            Self::ProviderAndRoundSettlement { .. } | Self::ProviderAndRoundFailure { .. } => true,
            Self::Host(_) => false,
        }
    }
    /// Includes unobserved release; this does not imply cleanup was attempted.
    pub(super) fn cleanup_unconfirmed(&self) -> bool {
        match self {
            Self::Round(error) => {
                error.assessment().cleanup() == harness::engine::RoundCleanup::Unconfirmed
            }
            Self::ProviderAndRoundSettlement { report, .. } => {
                report.assessment().cleanup() == harness::engine::RoundCleanup::Unconfirmed
            }
            Self::ProviderAndRoundFailure { primary, .. } => {
                primary.assessment().cleanup() == harness::engine::RoundCleanup::Unconfirmed
            }
            Self::NativeAdmission(_) | Self::Host(_) => false,
        }
    }
}

async fn fail_provider_round(
    observed: exomonad_actor::NativeProviderTurnLease,
    primary: harness::engine::EmbeddedRoundFailure,
) -> EmbeddedDriverError {
    match observed
        .fail(exomonad_model::ProviderFailure::Other(primary.to_string()))
        .await
    {
        Ok(()) => primary.into(),
        Err(source) => EmbeddedDriverError::ProviderAndRoundFailure {
            source,
            primary: Arc::new(primary),
        },
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
) -> Result<(), EmbeddedDriverError> {
    let transport = responses_client(&runtime);
    drive_conversation_with_transport::<RunAccountAuth, _>(
        embedded,
        runtime,
        settings,
        model,
        effort,
        instructions,
        cancellation,
        lifecycle,
        actor_ref,
        transport,
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
        session_identity,
        provider_admission,
        conversation,
        mut incoming,
        round_control,
        observation,
    } = embedded;
    let actor = conversation.identity().actor.clone();
    let provider_thread = session_identity.thread().to_owned();
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
                session_id: session_identity,
                agent: actor.clone(),
            },
            NonZeroU64::new(settings.context_capacity_tokens).ok_or("zero context capacity")?,
        )
        .map_err(|error| error.to_string())?;
    let engine = match runtime.output_observer() {
        Some(observer) => engine.with_output_observer(observer),
        None => engine,
    };
    let mut driver = engine.into_embedded_rounds()?;
    loop {
        if *cancellation.borrow() {
            return Ok(());
        }
        let recovery_changed = round_control.recovery_changed();
        tokio::pin!(recovery_changed);
        recovery_changed.as_mut().enable();
        let selection = match driver.next_round()? {
            harness::engine::EmbeddedRoundReadiness::Selected(selection) => selection,
            harness::engine::EmbeddedRoundReadiness::Waiting(_) => {
                lifecycle.publish(actor_ref, harness::server::HostActorLifecycle::Waiting);
                tokio::select! {
                    biased;
                    changed = cancellation.changed() => {
                        if changed.is_err() || *cancellation.borrow() { return Ok(()); }
                    }
                    () = &mut recovery_changed => {}
                    wake = incoming.recv() => if wake.is_none() { return Ok(()); },
                }
                continue;
            }
        };
        let (forward, forwarded) = mpsc::unbounded_channel();
        let mut lifetime_stopped = false;
        let round = round_control
            .begin()
            .map_err(|error| format!("could not begin embedded Engine round: {error}"))?;
        let observed_round = match provider_admission
            .begin_provider_turn(provider_thread.clone(), round.id().0.to_string())
            .await
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
            Err(error) => return Err(error.into()),
        };
        let result = {
            lifecycle.publish(actor_ref, harness::server::HostActorLifecycle::Running);
            let run = driver.run_round(selection, round.cancellation(), forwarded);
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
        let report = match result {
            Ok(report) => report,
            Err(error) => return Err(fail_provider_round(observed_round, error).await),
        };
        let assessment = report.assessment();
        if let Some(issue) = report.issue() {
            tracing::warn!(?actor, execution = ?assessment.execution(), %issue, "embedded provider action retains its round obligations");
        }
        drop(round);
        // Native idleness follows the driver's issued head and release evidence.
        // Inference permission remains independently owned by Harness.
        let settled = match assessment.execution() {
            harness::engine::RoundExecution::Completed => observed_round.succeed().await,
            harness::engine::RoundExecution::Cancelled => observed_round.interrupt().await,
            harness::engine::RoundExecution::Rejected => {
                observed_round
                    .fail(exomonad_model::ProviderFailure::RequestRejected)
                    .await
            }
            harness::engine::RoundExecution::Interrupted
            | harness::engine::RoundExecution::Failed => {
                observed_round
                    .fail(exomonad_model::ProviderFailure::TransportFailed)
                    .await
            }
        };
        if let Err(source) = settled {
            return Err(EmbeddedDriverError::ProviderAndRoundSettlement {
                source,
                report: Box::new(report),
            });
        }
        if lifetime_stopped || *cancellation.borrow() {
            return Ok(());
        }
        lifecycle.publish(actor_ref, harness::server::HostActorLifecycle::Waiting);
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
                    envelope_id: receipt.envelope_id.into(),
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
pub(super) mod round_fixture {
    use super::*;
    use harness::provider::Provider;

    #[derive(Clone, Copy)]
    pub(in crate::actor_host) enum Outcome {
        Completed,
        Rejected,
        CompletedHeadRefusal { reopen: bool },
        UnstartedClaimRefusal { reopen: bool },
        UnobservedRefusal,
    }
    struct Offline;
    impl harness::transport::Auth for Offline {
        fn access(&self) -> Result<String, TransportError> {
            panic!("component fixture has no network authentication")
        }
    }
    struct NoDispatch;
    #[async_trait::async_trait]
    impl Provider for NoDispatch {
        async fn call(
            &self,
            _: &str,
            _: serde_json::Value,
        ) -> Result<serde_json::Value, harness::provider::ProviderError> {
            panic!("unstarted policy refusal must precede provider dispatch")
        }
        fn tools(&self) -> Vec<serde_json::Value> {
            vec![
                serde_json::json!({"type":"function","name":"work","description":"fixture","strict":true,"parameters":{"type":"object","properties":{},"required":[],"additionalProperties":false}}),
            ]
        }
    }
    struct Script(Outcome);
    #[async_trait::async_trait]
    impl harness::engine::ResponsesTransport for Script {
        fn response_contract(&self) -> Result<(ResponsesRoute, ResponsesProtocol), TransportError> {
            Ok((ResponsesRoute::ChatGptPlan, ResponsesProtocol::Standard))
        }
        async fn create(
            &self,
            _: harness::transport::PreparedProviderRequest,
        ) -> Result<harness::transport::ResponsesTurn, TransportError> {
            if matches!(self.0, Outcome::Rejected) {
                return Err(TransportError::Authentication);
            }
            let items = if matches!(self.0, Outcome::UnstartedClaimRefusal { .. }) {
                vec![harness::item::Item(
                    serde_json::json!({"type":"function_call","call_id":"fixture-work","name":"work","arguments":"{}","async":true}),
                )]
            } else {
                vec![harness::item::Item(
                    serde_json::json!({"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"done"}]}),
                )]
            };
            Ok(harness::transport::ResponsesTurn {
                response_id: "fixture".into(),
                items,
                usage: Default::default(),
            })
        }
    }
    /// Real component admission, Store transactions and owner driver produce
    /// these facts. This does not exercise the resident compiler or native lease.
    pub(in crate::actor_host) async fn run(
        outcome: Outcome,
    ) -> Result<harness::engine::EmbeddedRoundSettlement, harness::engine::EmbeddedRoundFailure>
    {
        let files = tempfile::tempdir().unwrap();
        let path = files.path().join("store.sqlite");
        let store = Arc::new(harness::store::Store::open(&path).unwrap());
        let mut admitted =
            crate::actor_host::embedded_policy::operation_settlement_tests::admitted_round_fixture(
                store.clone(),
                Arc::new(
                    harness::embedding::ToolSurface::new(
                        "round-fixture".into(),
                        NoDispatch.tools(),
                        Arc::new(NoDispatch),
                    )
                    .unwrap(),
                ),
            )
            .await;
        let input = admitted
            .conversation
            .input("first", "human", "run fixture")
            .await
            .unwrap();
        assert!(input.wake_error.is_none());
        let identity = admitted.conversation.identity().clone();
        let config = EngineConfig {
            instructions: "fixture".into(),
            tools: Vec::new(),
            model: "fixture".into(),
            effort: Effort::Low,
            session_id: "owner-fixture".into(),
            agent: identity.actor,
        };
        let scheduler = Arc::new(harness::turn::JobScheduler::new(1).unwrap());
        let result = if matches!(outcome, Outcome::UnobservedRefusal) {
            // Standalone entry has no embedded binding or affirmative cleanup receipt.
            harness::engine::Engine::<Offline, NoDispatch, Script>::with_transport(
                Script(outcome),
                store,
                scheduler,
                Arc::new(NoDispatch),
                config,
            )
            .into_embedded_rounds()
            .err()
            .map_or_else(|| panic!("unbound engine must refuse"), Err)
        } else {
            let engine = admitted
                .conversation
                .engine::<Offline, _>(
                    Script(outcome),
                    scheduler,
                    config,
                    std::num::NonZeroU64::new(100_000).unwrap(),
                )
                .unwrap();
            let sql = rusqlite::Connection::open(&path).unwrap();
            match outcome {
                Outcome::CompletedHeadRefusal { reopen: false } => sql.execute_batch("CREATE TRIGGER head_fault BEFORE UPDATE OF head_request ON agents WHEN NEW.head_request IS NOT OLD.head_request BEGIN SELECT RAISE(ABORT,'head settlement refused'); END;").unwrap(),
                Outcome::CompletedHeadRefusal { reopen: true } => sql.execute_batch("CREATE TABLE fault_parent(id INTEGER PRIMARY KEY); CREATE TABLE fault_child(id INTEGER REFERENCES fault_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER head_fault AFTER UPDATE OF head_request ON agents WHEN NEW.head_request IS NOT OLD.head_request BEGIN INSERT INTO fault_child VALUES(1); END;").unwrap(),
                Outcome::UnstartedClaimRefusal { reopen } => {
                    sql.execute_batch("CREATE TRIGGER policy_fault BEFORE INSERT ON events WHEN NEW.kind='tool_scheduling' BEGIN SELECT RAISE(ABORT,'policy recording refused'); END;").unwrap();
                    if reopen { sql.execute_batch("CREATE TABLE fault_parent(id INTEGER PRIMARY KEY); CREATE TABLE fault_child(id INTEGER REFERENCES fault_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER claim_fault AFTER UPDATE ON claims BEGIN INSERT INTO fault_child VALUES(1); END;").unwrap(); }
                    else { sql.execute_batch("CREATE TRIGGER claim_fault BEFORE UPDATE ON claims BEGIN SELECT RAISE(IGNORE); END;").unwrap(); }
                }
                Outcome::Completed | Outcome::Rejected => {}
                Outcome::UnobservedRefusal => unreachable!(),
            }
            let mut driver = engine.into_embedded_rounds().unwrap();
            let harness::engine::EmbeddedRoundReadiness::Selected(selection) =
                driver.next_round().unwrap()
            else {
                panic!("durable fixture input must select");
            };
            let (_cancel, cancellation) = watch::channel(false);
            driver
                .run_round(selection, cancellation, mpsc::unbounded_channel().1)
                .await
        };
        admitted.finish().await;
        result
    }
    pub(in crate::actor_host) async fn failure(outcome: Outcome) -> EmbeddedDriverError {
        run(outcome).await.unwrap_err().into()
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    use harness::engine::EngineError;
    use harness::model::RequestId;

    use std::{
        future::Future,
        task::{Context, Poll, Waker},
        time::Duration,
    };
    use tokio::sync::oneshot;

    async fn service_with_server(
        files: &tempfile::TempDir,
        server: JoinHandle<Result<(), String>>,
    ) -> EmbeddedService {
        let settings = crate::actor_host::test_campaign::hosted_test_settings(files, 1);
        let mut service = EmbeddedService::prepare(files.path(), &settings)
            .await
            .unwrap();
        let previous = std::mem::replace(
            &mut service.server,
            EmbeddedServer::Pending {
                task: server,
                phase: ServerPhase::Running,
            },
        );
        let EmbeddedServer::Pending { task, .. } = previous else {
            unreachable!()
        };
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        service
    }

    #[derive(Clone, Copy)]
    enum ServerOutcome {
        Success,
        Error,
        Panic,
    }

    impl ServerOutcome {
        fn finish(self) -> Result<(), String> {
            match self {
                Self::Success => Ok(()),
                Self::Error => Err("retained server failure".into()),
                Self::Panic => panic!("retained server panic"),
            }
        }
        fn assert_result(self, result: &Result<(), String>) {
            match self {
                Self::Success => assert_eq!(result, &Ok(())),
                Self::Error => assert_eq!(result, &Err("retained server failure".into())),
                Self::Panic => assert!(result
                    .as_ref()
                    .unwrap_err()
                    .contains("retained server panic")),
            }
        }
    }

    async fn cancelled_shutdown_history(outcome: ServerOutcome) {
        let files = tempfile::tempdir().unwrap();
        let (started, ready) = oneshot::channel();
        let (release, held) = std::sync::mpsc::channel();
        // Hold one poll of the real task until the observer has been dropped.
        // Abort requests cannot settle a task while its current poll is running.
        let server = tokio::spawn(async move {
            started.send(()).unwrap();
            held.recv_timeout(Duration::from_secs(10)).unwrap();
            outcome.finish()
        });
        ready.await.unwrap();
        let mut service = service_with_server(&files, server).await;
        assert!(!service.server_finished());
        {
            let mut shutdown = Box::pin(service.shutdown());
            assert!(shutdown
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending());
        }
        assert!(!service.server_finished());
        let retry_pending = {
            let mut retry = Box::pin(service.shutdown());
            retry
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        };
        release.send(()).unwrap();
        assert!(
            retry_pending,
            "retry must still observe the owned server task"
        );
        let result = tokio::time::timeout(Duration::from_secs(10), service.shutdown())
            .await
            .unwrap();
        outcome.assert_result(&result);
        assert!(service.server_finished());
        for _ in 0..3 {
            assert_eq!(service.shutdown().await, result);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_shutdown_retains_late_error() {
        cancelled_shutdown_history(ServerOutcome::Error).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_shutdown_retains_late_panic() {
        cancelled_shutdown_history(ServerOutcome::Panic).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_shutdown_retains_late_success() {
        cancelled_shutdown_history(ServerOutcome::Success).await;
    }

    async fn finished_shutdown_history(outcome: ServerOutcome) {
        let files = tempfile::tempdir().unwrap();
        let server = tokio::spawn(async move { outcome.finish() });
        let completion = server.abort_handle();
        let mut service = service_with_server(&files, server).await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while !completion.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(service.server_finished());
        let result = service.shutdown().await;
        outcome.assert_result(&result);
        for _ in 0..3 {
            assert_eq!(service.shutdown().await, result);
        }
        assert!(service.server_finished());
    }

    #[tokio::test]
    async fn finished_shutdown_repeats_error() {
        finished_shutdown_history(ServerOutcome::Error).await;
    }

    #[tokio::test]
    async fn finished_shutdown_repeats_panic() {
        finished_shutdown_history(ServerOutcome::Panic).await;
    }

    #[tokio::test]
    async fn finished_shutdown_repeats_success() {
        finished_shutdown_history(ServerOutcome::Success).await;
    }

    #[tokio::test]
    async fn invalid_browser_assets_refuse_before_opening_or_recovering_the_run_store() {
        for retained in [false, true] {
            let files = tempfile::tempdir().unwrap();
            let mut settings = crate::actor_host::test_campaign::hosted_test_settings(&files, 1);
            let assets = files.path().join("invalid-assets");
            std::fs::create_dir(&assets).unwrap();
            std::fs::write(assets.join("index.html"), "unsealed assets").unwrap();
            settings.asset_root = assets;
            settings.validate().unwrap();
            let identity =
                harness::server::BrowserBundleIdentity::from_declared_source(include_bytes!(
                    concat!(env!("OUT_DIR"), "/harness-browser-source-identity.json")
                ))
                .unwrap();
            let expected = harness::server::verify_browser_bundle(&settings.asset_root, &identity)
                .unwrap_err();
            assert!(matches!(
                expected,
                harness::server::BrowserBundleError::Io(_)
            ));
            let store_path = files.path().join("harness/store.sqlite");
            let before = if retained {
                let store = super::super::display_output::open_run_store(files.path()).unwrap();
                store
                    .write_request(
                        &RequestId("retained-before-browser-refusal".into()),
                        None,
                        "/root",
                        &[harness::item::Item(serde_json::json!({
                            "type":"message", "role":"user", "content":"retained input",
                        }))],
                        Default::default(),
                    )
                    .unwrap();
                drop(store);
                Some(std::fs::read(&store_path).unwrap())
            } else {
                None
            };
            let actual = match EmbeddedService::prepare(files.path(), &settings).await {
                Err(error) => error,
                Ok(_) => panic!("unsealed assets must refuse startup"),
            };
            assert_eq!(actual, expected.to_string());
            match before {
                Some(before) => assert_eq!(std::fs::read(&store_path).unwrap(), before),
                None => assert!(!files.path().join("harness").exists()),
            }
        }
    }

    #[tokio::test]
    async fn requested_abort_repeats_success() {
        let files = tempfile::tempdir().unwrap();
        let (started, ready) = oneshot::channel();
        let server = tokio::spawn(async move {
            started.send(()).unwrap();
            std::future::pending::<Result<(), String>>().await
        });
        let mut service = service_with_server(&files, server).await;
        ready.await.unwrap();
        assert_eq!(service.shutdown().await, Ok(()));
        assert!(service.server_finished());
        for _ in 0..3 {
            assert_eq!(service.shutdown().await, Ok(()));
        }
    }

    struct TerminationNotice(Option<oneshot::Sender<()>>);
    impl Drop for TerminationNotice {
        fn drop(&mut self) {
            self.0.take().unwrap().send(()).ok();
        }
    }

    #[tokio::test]
    async fn service_drop_terminates_server_before_and_after_abort() {
        for begin_shutdown in [false, true] {
            let files = tempfile::tempdir().unwrap();
            let (started, ready) = oneshot::channel();
            let (stopped, terminated) = oneshot::channel();
            let server = tokio::spawn(async move {
                let _notice = TerminationNotice(Some(stopped));
                started.send(()).unwrap();
                std::future::pending::<Result<(), String>>().await
            });
            let mut service = service_with_server(&files, server).await;
            ready.await.unwrap();
            if begin_shutdown {
                let mut shutdown = Box::pin(service.shutdown());
                assert!(matches!(
                    shutdown
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop())),
                    Poll::Pending
                ));
            }
            drop(service);
            tokio::time::timeout(Duration::from_secs(10), terminated)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn embedded_actor_and_cell_clients_use_public_standard_responses() {
        struct NoCalls;
        #[async_trait::async_trait]
        impl harness::provider::Provider for NoCalls {
            async fn call(
                &self,
                _: &str,
                _: serde_json::Value,
            ) -> Result<serde_json::Value, harness::provider::ProviderError> {
                panic!("authentication must fail before tool dispatch")
            }
            fn tools(&self) -> Vec<serde_json::Value> {
                Vec::new()
            }
        }
        let source = "listen = '127.0.0.1:0'\nasset_root = '/tmp/assets'\nprovider = 'chatgpt_plan'\ncredential_file = '/tmp/exomonad-auth'\ncontext_capacity_tokens = 4096\n";
        let mut settings: EmbeddedLaunchConfig = toml::from_str(source).unwrap();
        let credentials = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let legacy = credentials.path().join("legacy.json");
        std::fs::write(
            &legacy,
            r#"{"tokens":{"access_token":"retired-private-token","account_id":"retired-account"}}"#,
        )
        .unwrap();
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o600)).unwrap();
        for path in [credentials.path().join("absent.json"), legacy] {
            settings.credential_file = path;
            let client =
                ResponsesClient::new(ChatGptPlanAuth::new(settings.credential_file.clone()));
            assert_eq!(
                harness::engine::ResponsesTransport::response_contract(&client).unwrap(),
                (ResponsesRoute::ChatGptPlan, ResponsesProtocol::Standard),
            );
            // Credential refusal prevents model traffic while the real Engine
            // path proves async host tool normalization and durable rejection.
            let tools = vec![serde_json::json!({
                "type": "custom", "name": "haskell", "format": {"type": "text"}, "async": true,
            })];
            let store = Arc::new(harness::store::Store::memory().unwrap());
            let engine = harness::engine::Engine::<ChatGptPlanAuth, _>::with_transport(
                client,
                store.clone(),
                Arc::new(harness::turn::JobScheduler::new(1).unwrap()),
                Arc::new(NoCalls),
                EngineConfig {
                    instructions: String::new(),
                    tools,
                    model: "gpt-6.1-sol".into(),
                    effort: Effort::Medium,
                    session_id: "provider-constructor-test".into(),
                    agent: AgentPath("/provider-constructor-test".into()),
                },
            );
            let (_cancellation, cancellation) = watch::channel(false);
            let (_input, incoming) = mpsc::unbounded_channel();
            let result = engine
                .run(
                    None,
                    vec![harness::item::Item(serde_json::json!({
                        "type":"message", "role":"user", "content":"check selected provider route",
                    }))],
                    cancellation,
                    incoming,
                )
                .await;
            let Err(EngineError::RequestRejected {
                head_request,
                error: TransportError::Authentication,
            }) = result
            else {
                panic!(
                    "selected transport must normalize then reject missing credentials: {result:?}"
                );
            };
            assert!(store.request(&head_request).unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn native_settlement_error_retains_completed_and_rejected_reports() {
        for rejected in [false, true] {
            let report = Box::new(
                round_fixture::run(if rejected {
                    round_fixture::Outcome::Rejected
                } else {
                    round_fixture::Outcome::Completed
                })
                .await
                .unwrap(),
            );
            let expected = report.assessment();
            let completion = report
                .original_completion()
                .map(|completion| completion as *const _);
            let issue = report.issue().map(|issue| issue as *const _);
            let error = EmbeddedDriverError::ProviderAndRoundSettlement {
                source: if rejected {
                    exomonad_actor::NativeProviderSettlementError::StaleTurn
                } else {
                    exomonad_actor::NativeProviderSettlementError::Unavailable
                },
                report,
            };
            assert!(error.requires_recovery());
            assert!(!error.cleanup_unconfirmed());
            let EmbeddedDriverError::ProviderAndRoundSettlement { report, source } = error else {
                unreachable!()
            };
            assert_eq!(report.assessment().execution(), expected.execution());
            assert_eq!(
                report.assessment().inference_obligations(),
                expected.inference_obligations()
            );
            assert_eq!(
                report
                    .original_completion()
                    .map(|completion| completion as *const _),
                completion
            );
            assert_eq!(report.issue().map(|issue| issue as *const _), issue);
            assert_eq!(
                report.assessment().cleanup(),
                harness::engine::RoundCleanup::Confirmed
            );
            assert_eq!(
                report.assessment().store_continuation(),
                harness::engine::StoreContinuation::NoReopenRequired
            );
            if rejected {
                assert_eq!(
                    source,
                    exomonad_actor::NativeProviderSettlementError::StaleTurn
                );
                assert!(matches!(
                    report.issue(),
                    Some(EngineError::RequestRejected {
                        error: TransportError::Authentication,
                        ..
                    })
                ));
                assert!(matches!(
                    report.assessment().inference_obligations(),
                    [harness::engine::InferenceObligation::RecoverBlock { .. }]
                ));
            } else {
                assert_eq!(
                    source,
                    exomonad_actor::NativeProviderSettlementError::Unavailable
                );
                assert!(!report.original_completion().unwrap().transcript.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn native_settlement_error_accumulates_owner_reopen_and_cleanup_obligations() {
        let primary = Arc::new(
            round_fixture::run(round_fixture::Outcome::UnstartedClaimRefusal { reopen: true })
                .await
                .unwrap_err(),
        );
        let original = primary.primary_error().unwrap() as *const _;
        let error = EmbeddedDriverError::ProviderAndRoundFailure {
            source: exomonad_actor::NativeProviderSettlementError::Unavailable,
            primary,
        };
        assert!(error.requires_recovery());
        assert!(error.cleanup_unconfirmed());
        let EmbeddedDriverError::ProviderAndRoundFailure { primary, source } = error else {
            unreachable!()
        };
        assert_eq!(
            source,
            exomonad_actor::NativeProviderSettlementError::Unavailable
        );
        assert_eq!(primary.primary_error().unwrap() as *const _, original);
        assert_eq!(
            primary.assessment().store_continuation(),
            harness::engine::StoreContinuation::ReopenRequired
        );
        assert_eq!(
            primary.assessment().cleanup(),
            harness::engine::RoundCleanup::Unconfirmed
        );
        assert!(primary
            .assessment()
            .secondary_failure_stages()
            .contains(&harness::engine::RoundFailureStage::NativeCleanup));
    }
}

#[cfg(test)]
#[path = "embedded_restart_driver_tests.rs"]
mod restart_tests;
