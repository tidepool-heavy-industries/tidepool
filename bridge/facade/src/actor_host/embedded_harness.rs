use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
};

use parking_lot::Mutex as ParkingMutex;

use exomonad_actor::{
    ActorAdmissionLease, ActorExitKind, ActorRef, ActorTerminal, HostedCheckpointAttachment,
    HostedCheckpointCapture, HostedCheckpointCaptureError, KernelCallFailure, LocalActorRef,
    LocalResidentInstallation, ResidentToolError, WorkbenchCancellationOutcome,
};
use exomonad_tool::{ToolArguments, ToolInvocationContext};
use harness::{
    embedding::{
        AdmissionGuard, Conversation, EmbeddedError, EmbeddedRoundId, HostActor, HostControl,
        HostControlError, HostIdentity, ToolSurface,
    },
    mailbox::DurableMailboxWake,
    model::{AgentPath, ConversationIdentity, OperationId},
    provider::{
        CallContext, CancellationAcknowledgment, CancellationOwner, InvocationCompletionSource,
        JobHandle, Provider, ProviderCompletion, ProviderError, ToolFailure,
    },
    store::Store,
    turn::{JobOutput, JobScheduler},
};
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};

use super::cell_context::{EmbeddedContextBinding, ModelResolver};
use super::embedded_policy::{EmbeddedPolicyInstallation, EmbeddedPolicySnapshot};

/// Observes real transaction boundaries without replacing admission or Store checks.
#[cfg(test)]
#[derive(Default)]
pub(super) struct EmbeddedAdmissionTestHooks {
    pub(super) before_input_wake:
        Option<Arc<dyn Fn(i64) -> futures_util::future::BoxFuture<'static, ()> + Send + Sync>>,
    pub(super) before_idle_probe: Option<Arc<dyn Fn() + Send + Sync>>,
}

struct StoreAdmission {
    _lease: ActorAdmissionLease,
}
impl AdmissionGuard for StoreAdmission {}

fn embedded_admission_error(error: KernelCallFailure) -> EmbeddedError {
    match error {
        KernelCallFailure::MailboxClosed(_) => EmbeddedError::AdmissionClosed,
        error => EmbeddedError::Host(error.to_string()),
    }
}

/// One harness owner for the existing run directory. The default
/// Codex launch never opens this Store; the embedding owner calls `open` when
/// it admits a bound conversation.
pub(super) struct EmbeddedHarnessRuntime {
    run: String,
    store: Arc<Store>,
    scheduler: Arc<JobScheduler>,
    output_observer: Arc<OnceLock<harness::server::ServerControl>>,
    recovery: OnceLock<Arc<super::embedded_recovery::EmbeddedApplicationRecovery>>,
    context_models: Arc<OnceLock<ModelResolver>>,
    #[cfg(test)]
    admission_test_hooks: OnceLock<Arc<EmbeddedAdmissionTestHooks>>,
}

impl EmbeddedHarnessRuntime {
    pub(super) fn open(run_root: &Path, concurrent_jobs: usize) -> Result<Self, EmbeddedError> {
        let store =
            super::display_output::open_run_store(run_root).map_err(EmbeddedError::Binding)?;
        Ok(Self {
            run: super::runtime_namespace(run_root),
            output_observer: Arc::new(OnceLock::new()),
            recovery: OnceLock::new(),
            context_models: Arc::new(OnceLock::new()),
            #[cfg(test)]
            admission_test_hooks: OnceLock::new(),
            store,
            scheduler: Arc::new(
                JobScheduler::new(concurrent_jobs)
                    .map_err(|error| EmbeddedError::Binding(error.to_string()))?,
            ),
        })
    }

    #[cfg(test)]
    pub(super) fn configure_admission_test_hooks(
        &self,
        hooks: Arc<EmbeddedAdmissionTestHooks>,
    ) -> Result<(), String> {
        self.admission_test_hooks
            .set(hooks)
            .map_err(|_| "embedded admission test hooks were already configured".into())
    }

    pub(super) fn configure_context_models(
        &self,
        config: &super::ActorHostConfig,
    ) -> Result<(), String> {
        let config = config.clone();
        self.context_models
            .set(Arc::new(move |selection| {
                if selection.trim().is_empty() {
                    return Err("next model must be nonempty".into());
                }
                let configured = config
                    .workspace_inputs
                    .as_ref()
                    .is_some_and(|workspace| workspace.models.contains_key(selection));
                let model = if configured {
                    exomonad_actor::Model::Alias(selection.to_owned())
                } else {
                    exomonad_actor::Model::Literal(selection.to_owned())
                };
                super::resolve_model(&config, &model)
            }))
            .map_err(|_| "context model policy already configured".into())
    }

    pub(super) fn configure_output_observer(
        &self,
        control: harness::server::ServerControl,
    ) -> Result<(), String> {
        self.output_observer
            .set(control)
            .map_err(|_| "embedded output observer was already configured".into())
    }
    pub(super) fn output_observer(&self) -> Option<Arc<dyn harness::engine::ModelOutputObserver>> {
        self.output_observer.get().map(|control| {
            Arc::new(control.clone()) as Arc<dyn harness::engine::ModelOutputObserver>
        })
    }

    pub(super) fn configure_application_recovery(
        &self,
        recovery: Arc<super::embedded_recovery::EmbeddedApplicationRecovery>,
    ) -> Result<(), String> {
        self.recovery
            .set(recovery)
            .map_err(|_| "embedded recovery owner was already configured".into())
    }

    fn prepare_application(
        &self,
        actor: ActorRef,
        identity: &HostIdentity,
    ) -> Result<(), EmbeddedError> {
        match self.recovery.get() {
            Some(recovery) => recovery
                .prepare(actor, identity)
                .map_err(EmbeddedError::Binding),
            #[cfg(test)]
            None => Ok(()),
            #[cfg(not(test))]
            None => Err(EmbeddedError::Binding(
                "embedded runtime has no retained recovery owner".into(),
            )),
        }
    }

    fn bind_application(
        &self,
        actor: ActorRef,
        conversation: &Conversation,
    ) -> Result<(), EmbeddedError> {
        match self.recovery.get() {
            Some(recovery) => recovery
                .bind(actor, conversation.identity(), &self.store)
                .map_err(EmbeddedError::Binding),
            #[cfg(test)]
            None => Ok(()),
            #[cfg(not(test))]
            None => Err(EmbeddedError::Binding(
                "embedded runtime has no retained recovery owner".into(),
            )),
        }
    }

    pub(super) fn admit_initial_input(&self, actor: ActorRef) -> bool {
        !self
            .recovery
            .get()
            .is_some_and(|recovery| recovery.root == actor && recovery.recovered_root)
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
        let actor_identity = actor.identity();
        let provider_actor = actor.clone();
        let observation = installation.observation();
        self.prepare_application(actor_identity, &identity)?;
        let (wakes, incoming) = mpsc::unbounded_channel();
        let round_control = Arc::new(EmbeddedRoundControl::default());
        let host = EmbeddedHostActor::new(
            identity,
            actor,
            installation,
            self.store.clone(),
            wakes,
            round_control.clone(),
        )?
        .with_context_models(self.context_models.clone());
        #[cfg(test)]
        let host = host.with_admission_test_hooks(self.admission_test_hooks.get().cloned());
        let host = Arc::new(host);
        let conversation = Arc::new(Conversation::attach(
            self.store.clone(),
            host.clone(),
            parent,
        )?);
        self.bind_application(actor_identity, &conversation)?;
        let provider_admission = attach_native_provider(
            &provider_actor,
            &conversation,
            self.store.clone(),
            observation.clone(),
            #[cfg(test)]
            self.admission_test_hooks.get().cloned(),
        )?;
        Ok(EmbeddedConversation {
            provider_admission,
            conversation,
            incoming,
            round_control,
            observation,
        })
    }

    pub(super) fn attach_checkpoint(
        &self,
        identity: HostIdentity,
        installation: &LocalResidentInstallation,
        policy: Arc<EmbeddedPolicyInstallation>,
        captured: Arc<EmbeddedHostedCheckpoint>,
    ) -> Result<EmbeddedConversation, EmbeddedError> {
        if identity.run != self.run
            || identity.actor != captured.child_path(installation.actor.identity())
            || identity.incarnation != installation.actor.identity().incarnation.0.to_string()
        {
            return Err(EmbeddedError::Binding(
                "embedded checkpoint child belongs to another admitted run/actor".into(),
            ));
        }
        installation
            .spawn_admission
            .as_ref()
            .ok_or_else(|| {
                EmbeddedError::Binding("checkpoint child has no independent spawn authority".into())
            })?
            .validate_child(installation.actor.identity())
            .map_err(EmbeddedError::Binding)?;
        let checkpoint = if let Some(lease) = &installation.checkpoint {
            if captured.issuer != lease.issuer || installation.context_parent != Some(lease.issuer)
            {
                return Err(EmbeddedError::Binding(
                    "embedded checkpoint issuer mismatch".into(),
                ));
            }
            captured.cuts.before_call()
        } else {
            captured.validate_inherited_origin(
                &self.run,
                installation.creator,
                installation.context_parent,
                installation.checkpoint_boundary.as_ref(),
            )?;
            captured.cuts.deferred()
        };
        let actor = installation.actor.clone();
        let parent = checkpoint.origin().clone();
        let actor_identity = actor.identity();
        let provider_actor = actor.clone();
        self.prepare_application(actor_identity, &identity)?;
        let (wakes, incoming) = mpsc::unbounded_channel();
        let round_control = Arc::new(EmbeddedRoundControl::default());
        let host = EmbeddedHostActor::new(
            identity,
            actor,
            policy,
            self.store.clone(),
            wakes,
            round_control.clone(),
        )?
        .with_context_models(self.context_models.clone());
        #[cfg(test)]
        let host = host.with_admission_test_hooks(self.admission_test_hooks.get().cloned());
        let host = Arc::new(host);
        // The embedded root also records an empty contract. This installation
        // has no authoritative checkout revision to record for the child.
        let conversation = Arc::new(Conversation::from_checkpoint(
            self.store.clone(),
            host,
            &parent,
            checkpoint,
            &json!({}),
            &json!({}),
        )?);
        self.bind_application(actor_identity, &conversation)?;
        let provider_admission = attach_native_provider(
            &provider_actor,
            &conversation,
            self.store.clone(),
            installation.runtime_observation.clone(),
            #[cfg(test)]
            self.admission_test_hooks.get().cloned(),
        )?;
        Ok(EmbeddedConversation {
            provider_admission,
            conversation,
            incoming,
            round_control,
            observation: installation.runtime_observation.clone(),
        })
    }

    pub(super) fn scheduler(&self) -> Arc<JobScheduler> {
        self.scheduler.clone()
    }

    pub(super) fn output_control_handle(&self) -> Arc<OnceLock<harness::server::ServerControl>> {
        self.output_observer.clone()
    }

    pub(super) fn run_identity(&self) -> &str {
        &self.run
    }

    pub(super) fn store(&self) -> Arc<Store> {
        self.store.clone()
    }
}

fn attach_native_provider(
    actor: &LocalActorRef,
    conversation: &Conversation,
    store: Arc<Store>,
    observation: exomonad_actor::ActorRuntimeObservationHandle,
    #[cfg(test)] test_hooks: Option<Arc<EmbeddedAdmissionTestHooks>>,
) -> Result<exomonad_actor::NativeProviderAdmission, EmbeddedError> {
    let identity = conversation.identity().clone();
    actor
        .attach_native_provider(observation, move || {
            // The real private claim is held; no admission or Store lock is held.
            #[cfg(test)]
            if let Some(probe) = test_hooks
                .as_ref()
                .and_then(|hooks| hooks.before_idle_probe.as_ref())
            {
                probe();
            }
            let frontier = store
                .embedded_round_frontier(&identity)
                .map_err(|error| error.to_string())?;
            Ok(frontier.pending_head.is_none()
                && frontier.pending_interruption.is_none()
                && store
                    .unread(&identity.actor.0)
                    .map_err(|error| error.to_string())?
                    .is_empty())
        })
        .map_err(|error| EmbeddedError::Binding(error.into()))
}

pub(super) struct EmbeddedConversation {
    pub(super) provider_admission: exomonad_actor::NativeProviderAdmission,
    pub(super) conversation: Arc<Conversation>,
    pub(super) incoming: mpsc::UnboundedReceiver<DurableMailboxWake>,
    pub(super) round_control: Arc<EmbeddedRoundControl>,
    pub(super) observation: exomonad_actor::ActorRuntimeObservationHandle,
}

/// The host and its single Engine driver share one exact active-round slot.
/// A handle can signal only the watch instance created for its own round.
#[derive(Default)]
pub(super) struct EmbeddedRoundControl {
    active: ParkingMutex<Option<RoundCancellationHandle>>,
}

#[derive(Clone)]
pub(super) struct RoundCancellationHandle {
    id: EmbeddedRoundId,
    sender: watch::Sender<bool>,
}

pub(super) struct EmbeddedRoundLease {
    control: Arc<EmbeddedRoundControl>,
    handle: RoundCancellationHandle,
    receiver: watch::Receiver<bool>,
}

impl EmbeddedRoundControl {
    pub(super) fn begin(self: &Arc<Self>) -> Result<EmbeddedRoundLease, String> {
        let mut active = self.active.lock();
        if active.is_some() {
            return Err("an embedded Engine round is already active".into());
        }
        let (sender, receiver) = watch::channel(false);
        let handle = RoundCancellationHandle {
            id: EmbeddedRoundId(uuid::Uuid::new_v4()),
            sender,
        };
        *active = Some(handle.clone());
        Ok(EmbeddedRoundLease {
            control: self.clone(),
            handle,
            receiver,
        })
    }

    pub(super) fn active_round(&self) -> Option<EmbeddedRoundId> {
        self.active.lock().as_ref().map(|handle| handle.id)
    }

    pub(super) fn interrupt(&self, expected_round: EmbeddedRoundId) -> Result<(), String> {
        let active = self.active.lock();
        let handle = active
            .as_ref()
            .ok_or_else(|| "no embedded Engine round is active".to_owned())?;
        if handle.id != expected_round {
            return Err("the targeted embedded Engine round is no longer active".into());
        }
        // Match and signal under the active slot lock. A delayed request never
        // resolves its old identity to a successor round.
        handle.cancel();
        Ok(())
    }
}

impl RoundCancellationHandle {
    pub(super) fn cancel(&self) {
        self.sender.send_replace(true);
    }
}

impl EmbeddedRoundLease {
    pub(super) fn id(&self) -> EmbeddedRoundId {
        self.handle.id
    }

    pub(super) fn cancellation(&self) -> watch::Receiver<bool> {
        self.receiver.clone()
    }

    pub(super) fn cancel(&self) {
        self.handle.cancel();
    }
}

impl Drop for EmbeddedRoundLease {
    fn drop(&mut self) {
        let mut active = self.control.active.lock();
        if active
            .as_ref()
            .is_some_and(|current| current.sender.same_channel(&self.handle.sender))
        {
            *active = None;
        }
    }
}

/// Durable notification owner for one exact embedded actor incarnation. The
/// read-only observer survives retirement; `live` fences new input admission.
#[derive(Clone)]
pub(super) struct EmbeddedActorBinding {
    identity: HostIdentity,
    conversation: Arc<Mutex<Option<Arc<Conversation>>>>,
    observer: Arc<Mutex<Option<Arc<harness::embedding::InputObserver>>>>,
    pub(super) inbox: Arc<super::ActorInbox>,
    pub(super) inbox_key: String,
    pub(super) delivery: Arc<tokio::sync::Mutex<()>>,
    live: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConversationAttachError {
    Retired,
    IdentityMismatch,
}

impl EmbeddedActorBinding {
    pub(super) fn new(
        identity: HostIdentity,
        inbox: Arc<super::ActorInbox>,
        inbox_key: String,
        conversation: Option<Arc<Conversation>>,
    ) -> Self {
        let observer = conversation
            .as_ref()
            .map(|conversation| Arc::new(conversation.input_observer()));
        Self {
            identity,
            conversation: Arc::new(Mutex::new(conversation)),
            observer: Arc::new(Mutex::new(observer)),
            inbox,
            inbox_key,
            delivery: Arc::new(tokio::sync::Mutex::new(())),
            live: Arc::new(AtomicBool::new(true)),
        }
    }

    pub(super) fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

    pub(super) fn mark_retired(&self) {
        let mut conversation = self
            .conversation
            .lock()
            .expect("embedded conversation lock poisoned");
        self.live.store(false, Ordering::Release);
        conversation.take();
    }

    pub(super) fn conversation(&self) -> Option<Arc<Conversation>> {
        self.conversation
            .lock()
            .expect("embedded conversation lock poisoned")
            .clone()
    }

    pub(super) fn set_conversation(
        &self,
        conversation: Arc<Conversation>,
    ) -> Result<(), ConversationAttachError> {
        let mut current = self
            .conversation
            .lock()
            .expect("embedded conversation lock poisoned");
        if !self.is_live() {
            return Err(ConversationAttachError::Retired);
        }
        if conversation.identity() != &self.identity {
            return Err(ConversationAttachError::IdentityMismatch);
        }
        *self
            .observer
            .lock()
            .expect("embedded observer lock poisoned") =
            Some(Arc::new(conversation.input_observer()));
        *current = Some(conversation);
        Ok(())
    }

    pub(super) fn input_observer(&self) -> Option<Arc<harness::embedding::InputObserver>> {
        self.observer
            .lock()
            .expect("embedded observer lock poisoned")
            .clone()
    }

    pub(super) fn identity(&self) -> &HostIdentity {
        &self.identity
    }
}

/// The exact actor and installation used by one bound harness conversation.
/// The run runtime owns Store and its scheduler; this host supplies authority
/// and an identity-only wake channel for their existing mailbox path.
pub(super) struct EmbeddedHostActor {
    identity: HostIdentity,
    actor: LocalActorRef,
    installation: Arc<EmbeddedPolicyInstallation>,
    store: Arc<Store>,
    wakes: mpsc::UnboundedSender<DurableMailboxWake>,
    round_control: Arc<EmbeddedRoundControl>,
    next_surface: AtomicU64,
    context_models: Arc<OnceLock<ModelResolver>>,
    #[cfg(test)]
    admission_test_hooks: Option<Arc<EmbeddedAdmissionTestHooks>>,
}

impl EmbeddedHostActor {
    #[cfg(test)]
    fn with_admission_test_hooks(mut self, hooks: Option<Arc<EmbeddedAdmissionTestHooks>>) -> Self {
        self.admission_test_hooks = hooks;
        self
    }

    fn with_context_models(mut self, models: Arc<OnceLock<ModelResolver>>) -> Self {
        self.context_models = models;
        self
    }

    pub(super) fn new(
        identity: HostIdentity,
        actor: LocalActorRef,
        installation: Arc<EmbeddedPolicyInstallation>,
        store: Arc<Store>,
        wakes: mpsc::UnboundedSender<DurableMailboxWake>,
        round_control: Arc<EmbeddedRoundControl>,
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
            store,
            wakes,
            round_control,
            next_surface: AtomicU64::new(1),
            context_models: Arc::new(OnceLock::new()),
            #[cfg(test)]
            admission_test_hooks: None,
        })
    }
}

#[async_trait::async_trait]
impl HostActor for EmbeddedHostActor {
    fn identity(&self) -> &HostIdentity {
        &self.identity
    }

    fn active_round(&self) -> Option<EmbeddedRoundId> {
        self.round_control.active_round()
    }

    async fn output_committed(&self, operation: &OperationId) -> Result<(), String> {
        let original = original_operation(&self.identity, operation)?;
        self.installation
            .complete(tidepool_runtime::session::ContextCheckpointBoundary::Hosted(original))
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn output_aborted(&self, operation: &OperationId) -> Result<(), String> {
        let original = original_operation(&self.identity, operation)?;
        self.installation
            .abort(tidepool_runtime::session::ContextCheckpointBoundary::Hosted(original))
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn admit(&self) -> Result<Box<dyn AdmissionGuard>, EmbeddedError> {
        self.actor
            .admit_transaction()
            .map(|lease| Box::new(StoreAdmission { _lease: lease }) as Box<dyn AdmissionGuard>)
            .map_err(embedded_admission_error)
    }

    fn tool_surface(&self) -> Result<Arc<ToolSurface>, EmbeddedError> {
        // A request must pin its handler before retirement closes admission.
        // The short lease also prevents cleanup from overtaking snapshot capture.
        let _admission = self
            .actor
            .admit_transaction()
            .map_err(embedded_admission_error)?;
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
        let manifest = snapshot.manifest();
        let dispatcher: Arc<dyn Provider> = Arc::new(EmbeddedDispatcher {
            identity: self.identity.clone(),
            issuer: self.actor.identity(),
            context_models: self.context_models.clone(),
            snapshot,
            store: self.store.clone(),
        });
        Ok(Arc::new(ToolSurface::from_manifest(
            version, manifest, dispatcher,
        )?))
    }

    async fn wake(&self, envelope_id: i64) -> Result<(), String> {
        // Harness has committed input and released its ActorAdmissionLease.
        #[cfg(test)]
        if let Some(wake) = self
            .admission_test_hooks
            .as_ref()
            .and_then(|hooks| hooks.before_input_wake.as_ref())
        {
            wake(envelope_id).await;
        }
        self.wakes
            .send(DurableMailboxWake { envelope_id })
            .map_err(|_| "embedded Engine wake receiver closed".into())
    }

    async fn control(&self, control: HostControl) -> Result<Value, HostControlError> {
        match control {
            HostControl::Retire => {
                self.actor
                    .shutdown(ActorTerminal {
                        kind: ActorExitKind::Cancelled,
                        summary: "embedded host requested retirement".into(),
                        diagnostic: None,
                    })
                    .await
                    .map_err(|error| HostControlError::Unconfirmed(error.to_string()))?;
                Ok(json!({"requested":true}))
            }
            HostControl::Interrupt { expected_round } => self
                .round_control
                .interrupt(expected_round)
                .map(|()| json!({"requested":true}))
                .map_err(HostControlError::Refused),
        }
    }
}

#[derive(Clone)]
struct EmbeddedDispatcher {
    context_models: Arc<OnceLock<ModelResolver>>,
    identity: HostIdentity,
    issuer: ActorRef,
    snapshot: Arc<EmbeddedPolicySnapshot>,
    store: Arc<Store>,
}

pub(super) fn original_operation(
    identity: &HostIdentity,
    operation: &OperationId,
) -> Result<exomonad_tool::OriginalOperation, String> {
    match &operation.origin {
        ConversationIdentity::Embedded {
            run,
            actor,
            incarnation,
        } if run == &identity.run
            && actor == &identity.actor
            && incarnation == &identity.incarnation => {}
        _ => return Err("foreign embedded operation".into()),
    }
    let original = exomonad_tool::OriginalOperation {
        origin: exomonad_tool::ConversationOrigin::Embedded {
            run: identity.run.clone(),
            actor: identity.actor.0.clone(),
            incarnation: identity.incarnation.clone(),
        },
        request_id: operation.request.0.clone(),
        call_id: operation.call.0.clone(),
    };
    if !original.is_complete() {
        return Err("incomplete embedded operation".into());
    }
    Ok(original)
}

/// Host-owned durable half of a Haskell checkpoint. Registry release refuses
/// new children; an admitted child keeps its captured attachment for install.
pub(super) struct EmbeddedHostedCheckpoint {
    issuer: ActorRef,
    pub(super) cuts: harness::checkpoint::CheckpointCuts<()>,
}

impl EmbeddedHostedCheckpoint {
    fn validate_inherited_origin(
        &self,
        run: &str,
        creator: Option<ActorRef>,
        context_parent: Option<ActorRef>,
        boundary: Option<&tidepool_runtime::session::ContextCheckpointBoundary>,
    ) -> Result<(), EmbeddedError> {
        let operation = self.cuts.deferred().operation().ok_or_else(|| {
            EmbeddedError::Binding("inherited hosted context has no captured operation".into())
        })?;
        validate_inherited_operation(
            self.issuer,
            self.cuts.deferred().origin(),
            operation,
            run,
            creator,
            context_parent,
            boundary,
        )
    }

    pub(super) fn child_path(&self, actor: ActorRef) -> AgentPath {
        AgentPath(format!(
            "{}/a{}_i{}",
            self.cuts.deferred().origin().0,
            actor.id.0,
            actor.incarnation.0
        ))
    }
}

fn validate_inherited_operation(
    issuer: ActorRef,
    parent: &AgentPath,
    operation: &OperationId,
    run: &str,
    creator: Option<ActorRef>,
    context_parent: Option<ActorRef>,
    boundary: Option<&tidepool_runtime::session::ContextCheckpointBoundary>,
) -> Result<(), EmbeddedError> {
    let refused = || {
        EmbeddedError::Binding(
            "inherited hosted context does not match its admitted issuer/operation".into(),
        )
    };
    if creator != Some(issuer) || context_parent != Some(issuer) {
        return Err(refused());
    }
    let original = boundary
        .and_then(tidepool_runtime::session::ContextCheckpointBoundary::hosted)
        .filter(|operation| operation.is_complete())
        .ok_or_else(refused)?;
    match (&operation.origin, &original.origin) {
        (
            ConversationIdentity::Embedded {
                run: source_run,
                actor,
                incarnation,
            },
            exomonad_tool::ConversationOrigin::Embedded {
                run: boundary_run,
                actor: boundary_actor,
                incarnation: boundary_incarnation,
            },
        ) if source_run == run
            && boundary_run == run
            && actor == parent
            && boundary_actor == &parent.0
            && incarnation == &issuer.incarnation.0.to_string()
            && boundary_incarnation == incarnation
            && original.request_id == operation.request.0
            && original.call_id == operation.call.0 =>
        {
            Ok(())
        }
        _ => Err(refused()),
    }
}

struct EmbeddedCheckpointCapture {
    store: Arc<Store>,
    identity: HostIdentity,
    issuer: ActorRef,
    operation: OperationId,
    context_head: Option<harness::model::RequestId>,
}

impl HostedCheckpointCapture for EmbeddedCheckpointCapture {
    fn capture(
        &self,
        name: &str,
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> Result<HostedCheckpointAttachment, HostedCheckpointCaptureError> {
        let original = original_operation(&self.identity, &self.operation)
            .map_err(|_| HostedCheckpointCaptureError::CaptureFailed)?;
        if boundary.hosted() != Some(&original) || name.is_empty() {
            return Err(HostedCheckpointCaptureError::CaptureFailed);
        }

        let metadata = json!({
            "name": name,
            "operation": self.operation,
        });
        let cuts = match &self.context_head {
            Some(head) => self.store.capture_checkpoint_cuts_at_head(
                &self.operation,
                head,
                &metadata,
                Arc::new(()),
            ),
            None => self
                .store
                .capture_checkpoint_cuts(&self.operation, &metadata, Arc::new(())),
        }
        .map_err(|_| HostedCheckpointCaptureError::CaptureFailed)?;
        Ok(HostedCheckpointAttachment::captured(Arc::new(
            EmbeddedHostedCheckpoint {
                issuer: self.issuer,
                cuts,
            },
        )))
    }
}

impl EmbeddedDispatcher {
    fn context(&self, operation: &OperationId) -> Result<ToolInvocationContext, ProviderError> {
        let original = original_operation(&self.identity, operation)
            .map_err(|error| ProviderError::Tool(error.into()))?;
        Ok(ToolInvocationContext {
            origin: exomonad_tool::ToolInvocationOrigin::Model(original),
            call_id: operation.call.0.clone(),
            namespace: None,
        })
    }

    async fn dispatch(
        &self,
        name: &str,
        arguments: ToolArguments,
        context: CallContext,
        context_binding: Option<Arc<dyn exomonad_actor::HostedContextBinding>>,
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
        let invocation_context = self.context(operation)?;
        let checkpoint_capture = self.snapshot.implementation(name).map(|_| {
            Arc::new(EmbeddedCheckpointCapture {
                store: self.store.clone(),
                identity: self.identity.clone(),
                issuer: self.issuer,
                operation: operation.clone(),
                context_head: context
                    .context
                    .as_ref()
                    .map(|snapshot| snapshot.head.clone()),
            }) as Arc<dyn HostedCheckpointCapture>
        });
        self.snapshot
            .dispatch(
                name.to_owned(),
                arguments,
                invocation_context,
                checkpoint_capture,
                context_binding,
            )
            .await
            .map_err(provider_tool_error)
    }
}

fn provider_tool_error(error: ResidentToolError) -> ProviderError {
    let diagnostic = match &error {
        ResidentToolError::Invocation(failure) => failure.failure_diagnostic(),
        _ => None,
    };
    let invocation = match &error {
        ResidentToolError::Invocation(failure) => Some(failure),
        _ => None,
    };
    let message = invocation
        .and_then(exomonad_actor::KernelInvocationFailure::publication)
        .map(workbench_publication_context)
        .map_or_else(
            || error.to_string(),
            |context| format!("{context} {}", error),
        );
    let mut metadata = diagnostic
        .map(|diagnostic| {
            serde_json::to_value(diagnostic)
                .expect("failure diagnostics contain only serializable data")
        })
        .unwrap_or_else(|| json!({}));
    if let Some(invocation) = invocation {
        metadata["publication"] = serde_json::to_value(invocation.publication()).unwrap();
        metadata["items"] = serde_json::to_value(invocation.receipts()).unwrap();
    }
    let failure = if diagnostic.is_some() || invocation.is_some() {
        let full = ToolFailure::with_metadata(message.clone(), metadata.clone());
        if full.metadata_omitted().is_none() {
            full
        } else {
            // Preserve committed operation identities before reducing large output payloads.
            metadata["items"] = json!(invocation
                .map(|failure| failure
                    .receipts()
                    .iter()
                    .map(|item| json!({
                        "index": item.index, "status": item.status,
                        "terminalTransfer": item.terminal_transfer,
                        "operations": item.operations.iter().map(|operation| json!({
                            "id": operation.id, "effect": operation.effect,
                            "disposition": operation.disposition,
                        })).collect::<Vec<_>>(),
                    }))
                    .collect::<Vec<_>>())
                .unwrap_or_default());
            metadata["itemsReduced"] = json!(true);
            let reduced = ToolFailure::with_metadata(message.clone(), metadata);
            if reduced.metadata_omitted().is_none() {
                reduced
            } else {
                let mut publication =
                    serde_json::to_value(invocation.and_then(|failure| failure.publication()))
                        .unwrap();
                if let Some(fields) = publication.as_object_mut() {
                    if let Some(bindings) = fields.remove("bindings") {
                        fields.insert(
                            "bindingsOmitted".into(),
                            json!(bindings.as_array().map_or(0, Vec::len)),
                        );
                    }
                    if let Some(detail) = fields.get_mut("detail") {
                        *detail = json!(exomonad_actor::bound_workbench_display(
                            detail.as_str().unwrap_or_default(),
                            512
                        ));
                    }
                }
                ToolFailure::with_metadata(
                    message,
                    json!({
                        "publication": publication,
                        "itemsOmitted": invocation.map_or(0, |failure| failure.receipts().len()),
                        "diagnosticOmitted": diagnostic.is_some(),
                    }),
                )
            }
        }
    } else {
        error.to_string().into()
    };
    ProviderError::Tool(failure)
}

fn workbench_publication_context(
    publication: &tidepool_runtime::session::WorkbenchPublicationOutcome,
) -> &'static str {
    use tidepool_runtime::session::WorkbenchPublicationOutcome;

    match publication {
        WorkbenchPublicationOutcome::NotPublished { .. }
        | WorkbenchPublicationOutcome::Rejected { .. } => {
            "This cell published no notebook bindings. Per-unit receipts describe private progress and effect outcomes. Completed external effects are not rolled back."
        }
        WorkbenchPublicationOutcome::Published { .. } => {
            "Notebook bindings were published; later failure does not undo publication."
        }
        WorkbenchPublicationOutcome::DurabilityUnconfirmed { .. } => {
            "Notebook bindings reached publication, but durability is unconfirmed."
        }
    }
}

#[async_trait::async_trait]
impl Provider for EmbeddedDispatcher {
    fn holds_job_capacity(&self) -> bool {
        // The actor owns execution admission. Parked cells may await independent
        // model invocations that need this same scheduler's provider slots.
        false
    }

    fn tools(&self) -> Vec<Value> {
        self.snapshot.tools().to_vec()
    }

    fn cancellation_owner(&self) -> Option<Arc<dyn CancellationOwner>> {
        Some(Arc::new(self.clone()))
    }

    async fn complete_call(
        &self,
        name: &str,
        input: harness::item::ToolInput,
        context: CallContext,
    ) -> ProviderCompletion {
        let snapshot = match checked_context_snapshot(&context) {
            Ok(snapshot) => snapshot,
            Err(error) => return unavailable_context_completion(JobOutput::Completed(Err(error))),
        };
        let context_read_write = match self.snapshot.context_read_write(name) {
            Ok(admitted) => admitted,
            Err(error) => {
                return unavailable_context_completion(JobOutput::Completed(Err(
                    provider_tool_error(error).into_tool_failure(),
                )));
            }
        };
        if context_read_write && snapshot.is_none() {
            return unavailable_context_completion(JobOutput::Completed(Err(
                "ContextReadWrite requires an exact synchronous context snapshot".into(),
            )));
        }
        let binding = match snapshot.filter(|_| context_read_write) {
            Some(snapshot) => {
                let operation = &snapshot.operation;
                let invocation = match self.context(operation) {
                    Ok(invocation) => invocation,
                    Err(error) => {
                        return unavailable_context_completion(JobOutput::Completed(Err(
                            error.into_tool_failure()
                        )));
                    }
                };
                let resolver = self.context_models.get().cloned().unwrap_or_else(|| {
                    Arc::new(|_| {
                        Err("next-model selection requires admitted workspace policy".into())
                    })
                });
                let binding = Arc::new(EmbeddedContextBinding::new(invocation, snapshot, resolver));
                if let Err(error) = context.completion.register(operation, binding.clone()) {
                    return unavailable_context_completion(JobOutput::Completed(Err(error
                        .to_string()
                        .into())));
                }
                Some(binding)
            }
            None => None,
        };
        if let Some(snapshot) = snapshot.filter(|_| !context_read_write) {
            // A synchronous prefix needs exact completion evidence for the
            // scheduler's native-completion race, without mutation authority.
            if let Err(error) = context
                .completion
                .register(&snapshot.operation, Arc::new(UneditedInvocationCompletion))
            {
                return unavailable_context_completion(JobOutput::Completed(Err(error
                    .to_string()
                    .into())));
            }
        }
        let arguments = match input {
            harness::item::ToolInput::Function(arguments) => ToolArguments::Structured(arguments),
            harness::item::ToolInput::Custom(source) => ToolArguments::Raw(source),
        };
        let authority = binding
            .as_ref()
            .map(|binding| binding.clone() as Arc<dyn exomonad_actor::HostedContextBinding>);
        let cancellation = context.cancel.clone();
        let operation = context.operation.clone();
        let result = self
            .dispatch(name, arguments, context, authority)
            .await
            .map_err(ProviderError::into_tool_failure);
        let invocation = operation.as_ref().map(|operation| self.context(operation));
        let retained_terminal = invocation
            .as_ref()
            .and_then(|invocation| invocation.as_ref().ok())
            .and_then(|invocation| self.snapshot.retained_operation(invocation.clone()).ok())
            .map(|owner| owner.terminal());
        // Native retirement can win before scheduler cancellation is signalled.
        // Its immutable CellExit, rather than notification timing, determines
        // cancellation, publication and unconfirmed cleanup.
        let terminal = match retained_terminal {
            Some(exomonad_actor::HostedOperationTerminal::Settled(terminal)) => Some(Ok(terminal)),
            _ if cancellation.is_cancelled() => Some(match invocation.as_ref() {
                Some(Ok(invocation)) => self.snapshot.cancel(invocation.clone()).await,
                Some(Err(error)) => Err(ResidentToolError::Unavailable(error.to_string())),
                None => Err(ResidentToolError::Unavailable(
                    "native cancellation requires an exact operation".into(),
                )),
            }),
            _ => None,
        };
        let output = if let Some(terminal) = terminal {
            if matches!(
                &terminal,
                Ok(WorkbenchCancellationOutcome::Cancelled { .. })
            ) {
                let finalized = match invocation.as_ref() {
                    Some(Ok(invocation)) => self.snapshot.abort_operation(invocation.clone()).await,
                    _ => Err(ResidentToolError::Unavailable(
                        "cancelled operation lacks original finalization authority".into(),
                    )),
                };
                if let Err(error) = finalized {
                    return unavailable_context_completion(JobOutput::CancellationUnconfirmed(
                        error.to_string(),
                    ));
                }
            }
            native_terminal_output(result, terminal)
        } else {
            JobOutput::Completed(result)
        };
        if let Some(binding) = binding {
            return binding
                .completion(output.clone())
                .unwrap_or_else(|| unavailable_context_completion(output));
        }
        UneditedInvocationCompletion::project(output)
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
        self.dispatch(name, ToolArguments::Structured(arguments), context, None)
            .await
    }

    async fn call_custom_with_context(
        &self,
        name: &str,
        input: String,
        context: CallContext,
    ) -> Result<Value, ProviderError> {
        self.dispatch(name, ToolArguments::Raw(input), context, None)
            .await
    }
}

fn checked_context_snapshot(
    context: &CallContext,
) -> Result<Option<&harness::context::ContextSnapshot>, ToolFailure> {
    let Some(snapshot) = context.context.as_ref() else {
        return Ok(None);
    };
    let operation = context
        .operation
        .as_ref()
        .ok_or_else(|| ToolFailure::from("context dispatch requires an exact operation"))?;
    if &snapshot.operation != operation {
        return Err("context snapshot belongs to another operation".into());
    }
    Ok(Some(snapshot))
}

struct UneditedInvocationCompletion;

impl UneditedInvocationCompletion {
    fn project(output: JobOutput) -> ProviderCompletion {
        ProviderCompletion {
            full_success: matches!(&output, JobOutput::Completed(Ok(_))),
            output,
            context: harness::provider::ContextDisposition::Unedited,
        }
    }
}

impl InvocationCompletionSource for UneditedInvocationCompletion {
    fn completion(&self, output: JobOutput) -> Option<ProviderCompletion> {
        Some(Self::project(output))
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
        match self.snapshot.cancel(context.clone()).await {
            Ok(WorkbenchCancellationOutcome::Cancelled { reply, .. }) => {
                match self.snapshot.abort_operation(context).await {
                    Ok(()) => CancellationAcknowledgment::StoppedWithReceipt(
                        workbench_reply_receipt(reply),
                    ),
                    Err(error) => CancellationAcknowledgment::Unconfirmed(error.to_string()),
                }
            }
            Ok(
                WorkbenchCancellationOutcome::Expired { reply, .. }
                | WorkbenchCancellationOutcome::PublicationSettled { reply, .. },
            ) => CancellationAcknowledgment::Completed(workbench_reply_receipt(reply)),
            Ok(outcome) => CancellationAcknowledgment::Unconfirmed(format!("{outcome:?}")),
            Err(error) => CancellationAcknowledgment::Unconfirmed(error.to_string()),
        }
    }
}

fn unavailable_context_completion(output: JobOutput) -> ProviderCompletion {
    ProviderCompletion {
        output,
        full_success: false,
        context: harness::provider::ContextDisposition::Unavailable,
    }
}

fn workbench_reply_receipt(
    reply: exomonad_actor::KernelWorkbenchReply,
) -> Result<Value, ToolFailure> {
    reply
        .map_err(ResidentToolError::Invocation)
        .and_then(|response| serde_json::to_value(response).map_err(ResidentToolError::Encoding))
        .map_err(provider_tool_error)
        .map_err(ProviderError::into_tool_failure)
}

fn native_terminal_output(
    result: Result<Value, ToolFailure>,
    terminal: Result<WorkbenchCancellationOutcome, ResidentToolError>,
) -> JobOutput {
    match terminal {
        Ok(WorkbenchCancellationOutcome::Cancelled { reply, .. }) => {
            JobOutput::CancelledWithReceipt(workbench_reply_receipt(reply))
        }
        Ok(
            WorkbenchCancellationOutcome::Expired { .. }
            | WorkbenchCancellationOutcome::PublicationSettled { .. }
            | WorkbenchCancellationOutcome::NotSleeping { .. },
        ) => JobOutput::Completed(result),
        Ok(outcome) => JobOutput::CancellationUnconfirmed(format!("{outcome:?}")),
        Err(error) => JobOutput::CancellationUnconfirmed(error.to_string()),
    }
}

#[cfg(test)]
#[path = "embedded_cancel_receipt_tests.rs"]
mod cancellation_receipt_tests;

#[cfg(test)]
mod round_control_tests {
    use super::*;

    #[test]
    fn admission_mapping_preserves_temporary_and_generic_refusals() {
        let actor = ActorRef {
            id: exomonad_actor::ActorId(7),
            incarnation: exomonad_actor::Incarnation(2),
        };
        assert!(matches!(
            embedded_admission_error(KernelCallFailure::MailboxClosed(actor)),
            EmbeddedError::AdmissionClosed
        ));
        for error in [
            KernelCallFailure::RetirementPending(actor),
            KernelCallFailure::TargetUnavailable(actor),
            KernelCallFailure::Handler {
                actor,
                detail: "host request admission closed".into(),
                diagnostic: None,
            },
        ] {
            assert!(matches!(
                embedded_admission_error(error),
                EmbeddedError::Host(_)
            ));
        }
    }

    fn readonly_snapshot() -> harness::context::ContextSnapshot {
        use harness::{
            item::Item,
            model::{CallId, RequestId},
            store::Usage,
        };
        let store = Store::memory().unwrap();
        let head = RequestId("root".into());
        store.write_request(&head, None, "/root", &[
            Item(json!({"type":"message", "role":"user", "content":"checkpoint prefix"})),
            Item(json!({"type":"custom_tool_call", "call_id":"call", "name":"notebook", "input":"display True"})),
        ], Usage::default()).unwrap();
        let operation = store.claim(&CallId("call".into()), &head).unwrap();
        store.begin_context(&operation, &head).unwrap()
    }

    #[test]
    fn readonly_checkpoint_snapshot_requires_its_exact_operation() {
        let snapshot = readonly_snapshot();
        let operation = snapshot.operation.clone();
        let (mut context, _) = CallContext::detached_for_test(
            JobHandle("probe".into()),
            operation.call.clone(),
            operation.origin.actor().clone(),
        );
        assert!(checked_context_snapshot(&context).unwrap().is_none());
        context.context = Some(snapshot);
        assert!(checked_context_snapshot(&context).is_err());
        context.operation = Some(operation.clone());
        assert!(std::ptr::eq(
            checked_context_snapshot(&context).unwrap().unwrap(),
            context.context.as_ref().unwrap()
        ));
        context.operation.as_mut().unwrap().request.0 = "foreign-request".into();
        assert!(checked_context_snapshot(&context).is_err());
        context.operation = Some(operation);
        context.operation.as_mut().unwrap().call.0 = "foreign-call".into();
        assert!(checked_context_snapshot(&context).is_err());
    }

    #[test]
    fn unedited_invocation_completion_preserves_terminal_categories() {
        let outputs = [
            JobOutput::Completed(Ok(json!({"value":42}))),
            JobOutput::Completed(Err("failed".into())),
            JobOutput::Cancelled,
            JobOutput::CancelledWithReceipt(Ok(json!({"cancelled":true}))),
            JobOutput::CancelledWithReceipt(Err("abort failed".into())),
            JobOutput::Interrupted,
            JobOutput::CancellationUnconfirmed("owner unavailable".into()),
        ];
        for output in outputs {
            let normal = UneditedInvocationCompletion::project(output.clone());
            let raced = UneditedInvocationCompletion
                .completion(output.clone())
                .unwrap();
            assert_eq!(normal.output, output);
            assert_eq!(raced.output, output);
            assert_eq!(
                normal.full_success,
                matches!(output, JobOutput::Completed(Ok(_)))
            );
            assert_eq!(raced.full_success, normal.full_success);
            assert_eq!(
                normal.context,
                harness::provider::ContextDisposition::Unedited
            );
            assert_eq!(raced.context, normal.context);
        }
    }

    #[tokio::test]
    async fn readonly_sync_completion_survives_native_terminal_before_provider_waiter() {
        struct Probe {
            started: tokio::sync::Notify,
            release: tokio::sync::Notify,
            operation: OperationId,
        }
        #[async_trait::async_trait]
        impl Provider for Probe {
            fn tools(&self) -> Vec<Value> {
                Vec::new()
            }
            fn cancellation_owner(&self) -> Option<Arc<dyn CancellationOwner>> {
                Some(Arc::new(NativeCompleted(self.operation.clone())))
            }
            async fn call(&self, _: &str, _: Value) -> Result<Value, ProviderError> {
                unreachable!("the scheduler supplies an exact call context")
            }
            async fn complete_call(
                &self,
                _: &str,
                _: harness::item::ToolInput,
                context: CallContext,
            ) -> ProviderCompletion {
                let snapshot = checked_context_snapshot(&context).unwrap().unwrap();
                let mut foreign = snapshot.operation.clone();
                foreign.call.0 = "foreign".into();
                assert_eq!(
                    context
                        .completion
                        .register(&foreign, Arc::new(UneditedInvocationCompletion)),
                    Err(harness::provider::CompletionRegistrationError::ForeignOperation)
                );
                context
                    .completion
                    .register(&snapshot.operation, Arc::new(UneditedInvocationCompletion))
                    .unwrap();
                assert_eq!(
                    context
                        .completion
                        .register(&snapshot.operation, Arc::new(UneditedInvocationCompletion)),
                    Err(harness::provider::CompletionRegistrationError::AlreadyRegistered)
                );
                self.started.notify_one();
                self.release.notified().await;
                UneditedInvocationCompletion::project(JobOutput::Completed(Ok(
                    json!({"native":42}),
                )))
            }
        }
        struct NativeCompleted(OperationId);
        #[async_trait::async_trait]
        impl CancellationOwner for NativeCompleted {
            async fn cancel(
                &self,
                operation: &OperationId,
                _: &JobHandle,
            ) -> CancellationAcknowledgment {
                assert_eq!(operation, &self.0);
                CancellationAcknowledgment::Completed(Ok(json!({"native":42})))
            }
        }
        let snapshot = readonly_snapshot();
        let operation = snapshot.operation.clone();
        let provider = Arc::new(Probe {
            started: Default::default(),
            release: Default::default(),
            operation: operation.clone(),
        });
        let scheduler = JobScheduler::new(1).unwrap();
        scheduler
            .queue_operation(
                provider.clone(),
                operation.clone(),
                operation.origin.actor().clone(),
                Some(operation.request.clone()),
                "notebook".into(),
                harness::item::ToolInput::Custom("display True".into()),
            )
            .await
            .unwrap();
        scheduler
            .release_operation(&operation, Some(snapshot))
            .await
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            provider.started.notified(),
        )
        .await
        .unwrap();
        let settlement = scheduler.cancel(&operation).await.unwrap().unwrap();
        assert_eq!(
            settlement.output,
            JobOutput::Completed(Ok(json!({"native":42})))
        );
        let completion = scheduler
            .invocation_completion(&operation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completion.output, settlement.output);
        assert!(completion.full_success);
        assert_eq!(
            completion.context,
            harness::provider::ContextDisposition::Unedited
        );
        provider.release.notify_one();
    }

    #[test]
    fn missing_context_terminal_metadata_refuses_publication() {
        let output = JobOutput::Completed(Ok(json!({"exact": "receipt"})));
        let completion = unavailable_context_completion(output.clone());
        assert_eq!(completion.output, output);
        assert!(!completion.full_success);
        assert_eq!(
            completion.context,
            harness::provider::ContextDisposition::Unavailable
        );
    }

    #[test]
    fn native_cancelled_owner_projects_a_cancelled_error_receipt() {
        let output = native_terminal_output(
            Err("ordinary waiter error".into()),
            Ok(WorkbenchCancellationOutcome::Cancelled {
                execution: tidepool_runtime::session::WorkbenchExecutionId::from_digest([6; 16]),
                reply: Err(exomonad_actor::KernelInvocationFailure::Failed {
                    actor: ActorRef::first(exomonad_actor::ActorId(1)),
                    detail: "native abort with retained prefix".into(),
                    receipts: Vec::new(),
                    diagnostic: None,
                }),
            }),
        );
        assert!(matches!(output, JobOutput::CancelledWithReceipt(Err(_))));
    }

    #[test]
    fn native_completed_owner_preserves_the_original_waiter_result() {
        let execution = tidepool_runtime::session::WorkbenchExecutionId::from_digest([7; 16]);
        let reply = Ok(tidepool_runtime::session::WorkbenchResponse {
            publication: None,
            status: tidepool_runtime::session::WorkbenchRunStatus::Completed,
            summary: None,
            items: Vec::new(),
            next_index: 1,
            total: 1,
        });
        for terminal in [
            WorkbenchCancellationOutcome::Expired {
                execution: execution.clone(),
                reply: reply.clone(),
            },
            WorkbenchCancellationOutcome::PublicationSettled { execution, reply },
        ] {
            let result = Ok(json!({"exact": "native return"}));
            assert_eq!(
                native_terminal_output(result.clone(), Ok(terminal)),
                JobOutput::Completed(result)
            );
        }
    }

    #[test]
    fn embedded_tool_failure_preserves_classification_and_original_error_text() {
        for (output, reduced) in [
            ("private unit output".to_owned(), false),
            ("large retained command output".repeat(1024), true),
        ] {
            let diagnostic = tidepool_toolchain::failclass::classify_compile(
                &tidepool_toolchain::CompileError::ExtractFailed("retained owner missing".into()),
            );
            let error = ResidentToolError::Invocation(
                exomonad_actor::KernelInvocationFailure::Workbench(
                    exomonad_actor::KernelWorkbenchFailure {
                        actor: ActorRef::first(exomonad_actor::ActorId(7)),
                        receipts: vec![tidepool_runtime::session::WorkbenchItemReceipt {
                            index: 0,
                            kind: None,
                            span: None,
                            source_items: Vec::new(),
                            status: tidepool_runtime::session::WorkbenchItemStatus::Committed,
                            output,
                            value: None,
                            diagnostics: Vec::new(),
                            failure_layer: None,
                            warnings: Vec::new(),
                            installed_bindings: vec!["privateValue".into()],
                            operations: vec![tidepool_runtime::session::WorkbenchOperationReceipt {
                                id: tidepool_runtime::session::WorkbenchOperationId {
                                    execution: tidepool_runtime::session::WorkbenchExecutionId::from_digest(
                                        [7; 16],
                                    ),
                                    input_unit_index: 0,
                                    effect_ordinal: 0,
                                },
                                effect: "record_send".into(),
                                disposition: tidepool_runtime::session::WorkbenchOperationDisposition::Committed,
                                display: None,
                                display_publication: None,
                            }],
                            terminal_transfer: None,
                        }],
                        point: tidepool_runtime::session::WorkbenchFailurePoint::InputUnit {
                            index: 0,
                        },
                        publication: Some(
                            tidepool_runtime::session::WorkbenchPublicationOutcome::NotPublished {
                                reason:
                                    tidepool_runtime::session::WorkbenchNotPublishedReason::Failed,
                            },
                        ),
                        total: 1,
                        detail: "retained owner missing".into(),
                        diagnostic: Some(diagnostic),
                    },
                ),
            );
            let original = error.to_string();
            let failure = provider_tool_error(error).into_tool_failure();
            assert!(failure.message().contains(&original));
            let metadata = failure.metadata().unwrap();
            assert_eq!(metadata["publication"]["status"], "notPublished");
            assert_eq!(metadata["publication"]["reason"], "failed");
            assert_eq!(metadata["items"][0]["status"], "committed");
            assert_eq!(
                metadata["items"][0]["operations"][0]["disposition"],
                "committed"
            );
            if reduced {
                assert_eq!(metadata["itemsReduced"], true);
            } else {
                assert!(metadata.get("itemsReduced").is_none());
                assert_eq!(metadata["items"][0]["installedBindings"][0], "privateValue");
            }
            assert_eq!(metadata["class"], "version-skew");
            assert_eq!(metadata["phase"], "compile");
            assert_eq!(metadata["cause"]["kind"], "extractor_contract");
        }
    }

    #[test]
    fn delayed_interrupt_never_targets_a_successor_round() {
        let control = Arc::new(EmbeddedRoundControl::default());
        let absent = EmbeddedRoundId(uuid::Uuid::new_v4());
        assert!(control.interrupt(absent).is_err());
        assert_eq!(control.active_round(), None);

        let first = control.begin().expect("first round");
        let first_id = control.active_round().expect("active first round");
        let first_receiver = first.cancellation();
        let old_handle = first.handle.clone();
        assert!(control.begin().is_err());
        control.interrupt(first_id).expect("interrupt first round");
        assert!(*first_receiver.borrow());
        drop(first);
        assert!(control.interrupt(first_id).is_err());

        let second = control.begin().expect("second round");
        let second_id = control.active_round().expect("active second round");
        assert_ne!(first_id, second_id);
        let second_receiver = second.cancellation();
        assert!(control.interrupt(first_id).is_err());
        old_handle.cancel();
        assert!(
            !*second_receiver.borrow(),
            "a delayed interrupt or old handle must not cancel its successor"
        );
        control
            .interrupt(second_id)
            .expect("interrupt second round");
        assert!(*second_receiver.borrow());
        drop(second);
        assert_eq!(control.active_round(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor_host::hosted_test_context::HostedTestRuntime;
    use crate::actor_host::test_campaign::TestCampaign;
    use async_trait::async_trait;
    use futures_util::StreamExt;
    use harness::{
        embedding::InputObservation,
        engine::ResponsesTransport,
        item::Item,
        transport::{ResponsesRequest, ResponsesTurn, TransportError},
    };
    use std::{sync::Mutex, time::Duration};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    // These semantic checks include cold whole-cell compilation in a debug
    // worker. Keep that budget bounded, while leaving provider and cleanup
    // waits short because they do not include native compilation.
    const COLD_NATIVE_CELL_SETTLEMENT_BUDGET: Duration = Duration::from_secs(300);

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

    #[test]
    fn inherited_hosted_operation_requires_exact_admitted_parent_and_call() {
        let issuer = ActorRef::first(exomonad_actor::ActorId(1));
        let other = ActorRef::first(exomonad_actor::ActorId(2));
        let parent = AgentPath("/root".into());
        let operation = OperationId {
            origin: ConversationIdentity::Embedded {
                run: "run".into(),
                actor: parent.clone(),
                incarnation: "1".into(),
            },
            request: harness::model::RequestId("request".into()),
            call: harness::model::CallId("call".into()),
        };
        let original = exomonad_tool::OriginalOperation {
            origin: exomonad_tool::ConversationOrigin::Embedded {
                run: "run".into(),
                actor: parent.0.clone(),
                incarnation: "1".into(),
            },
            request_id: "request".into(),
            call_id: "call".into(),
        };
        let boundary =
            tidepool_runtime::session::ContextCheckpointBoundary::Hosted(original.clone());
        let check =
            |run: &str,
             creator,
             context_parent,
             boundary: Option<&tidepool_runtime::session::ContextCheckpointBoundary>| {
                validate_inherited_operation(
                    issuer,
                    &parent,
                    &operation,
                    run,
                    creator,
                    context_parent,
                    boundary,
                )
            };
        assert!(check("run", Some(issuer), Some(issuer), Some(&boundary)).is_ok());
        assert!(check("other-run", Some(issuer), Some(issuer), Some(&boundary)).is_err());
        assert!(check("run", Some(other), Some(issuer), Some(&boundary)).is_err());
        assert!(check("run", Some(issuer), Some(other), Some(&boundary)).is_err());
        assert!(check("run", None, Some(issuer), Some(&boundary)).is_err());
        assert!(check("run", Some(issuer), Some(issuer), None).is_err());
        for foreign in [
            exomonad_tool::OriginalOperation {
                request_id: "other-request".into(),
                ..original.clone()
            },
            exomonad_tool::OriginalOperation {
                call_id: "other-call".into(),
                ..original.clone()
            },
            exomonad_tool::OriginalOperation {
                origin: exomonad_tool::ConversationOrigin::Embedded {
                    run: "run".into(),
                    actor: "/other".into(),
                    incarnation: "1".into(),
                },
                ..original.clone()
            },
            exomonad_tool::OriginalOperation {
                origin: exomonad_tool::ConversationOrigin::Embedded {
                    run: "run".into(),
                    actor: "/root".into(),
                    incarnation: "2".into(),
                },
                ..original.clone()
            },
            exomonad_tool::OriginalOperation {
                origin: exomonad_tool::ConversationOrigin::External {
                    thread_id: "thread".into(),
                },
                ..original
            },
        ] {
            let boundary = tidepool_runtime::session::ContextCheckpointBoundary::Hosted(foreign);
            assert!(check("run", Some(issuer), Some(issuer), Some(&boundary)).is_err());
        }
        let route = tidepool_runtime::session::ContextCheckpointBoundary::Route {
            actor_id: 1,
            incarnation: 1,
            watch_id: 1,
        };
        assert!(check("run", Some(issuer), Some(issuer), Some(&route)).is_err());
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
                vec![Item(json!({
                    "type":"custom_tool_call", "call_id":"raw-cell-1",
                    "name":"haskell", "input":"40 + 2 :: Int"
                }))]
            } else if round == 2 {
                self.entered.notify_one();
                self.release.notified().await;
                // Keep the round nonfinal after settlement so the next boundary compacts.
                vec![Item(json!({
                    "type":"custom_tool_call", "call_id":"raw-cell-2",
                    "name":"haskell",
                    "input":include_str!("embedded_checkpoint_capture.hs")
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
                    input_tokens: if round == 2 { 100_001 } else { 0 },
                    ..Default::default()
                },
            })
        }
    }

    #[tokio::test]
    async fn production_embedded_engine_compacts_and_publishes_checkpoint_effect() {
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
            public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
            public_origin: None,
            asset_root: assets,
            browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
            session_secret_file: Some(secret_file),
            provider: crate::exomonad::EmbeddedModelProvider::Codex,
            credential_file: auth_file,
            context_capacity_tokens: 200_000,
            concurrent_jobs: 1,
        };
        let transport = ParkUntilInput {
            entered: Arc::new(tokio::sync::Notify::new()),
            completed: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
            requests: Arc::new(Mutex::new(vec![])),
            compactions: Arc::new(AtomicU64::new(0)),
        };
        let provider: Arc<dyn ResponsesTransport> = Arc::new(transport.clone());
        let host = HostedTestRuntime::start(&settings, &provider)
            .await
            .unwrap();
        host.input("Run the cell before browser compaction and checkpoint capture.")
            .await
            .unwrap();
        let conversation = host
            .context
            .binding(host.context.actor.identity())
            .unwrap()
            .conversation()
            .unwrap();
        tokio::time::timeout(
            COLD_NATIVE_CELL_SETTLEMENT_BUDGET,
            transport.entered.notified(),
        )
        .await
        .expect("actual admitted Haskell cell must settle before provider round two");
        let origin = format!("https://{}", host.address);
        let api = format!("http://{}/api", host.address);
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
        let (mut receipt_socket, _) =
            crate::actor_host::m1_host_tests::browser_snapshot(host.address, &cookie).await;
        let response = client
            .post(format!("{api}/commands"))
            .header("Origin", &origin)
            .header(reqwest::header::COOKIE, &cookie)
            .json(&harness::server::ClientCommand::Host {
                operation_id: harness::embedding::ClientOperationId(uuid::Uuid::new_v4()),
                command: harness::server::HostCommand::Input {
                    target: conversation.identity().clone(),
                    text: "wake me".into(),
                },
            })
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
        let accepted: serde_json::Value = response.json().await.unwrap();
        let command_id = accepted["command_id"].as_str().unwrap().to_owned();
        let receipt = crate::actor_host::m1_host_tests::next_browser_event(
            &mut receipt_socket,
            "command.receipt",
        )
        .await;
        let receipt = &receipt["event"]["event"]["value"];
        assert_eq!(receipt["commandId"], command_id);
        assert_eq!(receipt["outcome"], "admitted");
        let envelope_id = receipt["envelopeId"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap();
        receipt_socket.close(None).await.unwrap();
        let store = host.runtime.store();
        let call = harness::model::CallId("raw-cell-1".into());
        let host_identity = conversation.identity().clone();
        let embedded_origin = ConversationIdentity::Embedded {
            run: host_identity.run.clone(),
            actor: host_identity.actor.clone(),
            incarnation: host_identity.incarnation.clone(),
        };
        let claim = store
            .claims(&call)
            .unwrap()
            .into_iter()
            .find(|claim| claim.operation.origin == embedded_origin)
            .expect("the real Engine call must retain its exact embedded operation");
        let output = tokio::time::timeout(
            COLD_NATIVE_CELL_SETTLEMENT_BUDGET,
            host.runtime.scheduler().wait(&claim.operation),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "real Haskell operation {:?} did not settle while the scripted provider round was held within {:?}; provider_rounds={}, compactions={}",
                claim.operation,
                COLD_NATIVE_CELL_SETTLEMENT_BUDGET,
                transport.requests.lock().unwrap().len(),
                transport.compactions.load(Ordering::Relaxed),
            )
        })
        .unwrap();
        assert!(
            matches!(output, harness::turn::JobOutput::Completed(Ok(_))),
            "real Haskell call did not settle successfully: {output:?}"
        );
        transport.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), transport.completed.notified())
            .await
            .unwrap();
        let checkpoint_call = harness::model::CallId("raw-cell-2".into());
        let checkpoint_claim = store
            .claims(&checkpoint_call)
            .unwrap()
            .into_iter()
            .find(|claim| claim.operation.origin == embedded_origin)
            .expect("the real Engine checkpoint call must retain its exact operation");
        let checkpoint_output = tokio::time::timeout(
            COLD_NATIVE_CELL_SETTLEMENT_BUDGET,
            host
                .runtime
                .scheduler()
                .wait(&checkpoint_claim.operation),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "real Haskell checkpoint operation {:?} did not settle within {:?}; provider_rounds={}, compactions={}",
                checkpoint_claim.operation,
                COLD_NATIVE_CELL_SETTLEMENT_BUDGET,
                transport.requests.lock().unwrap().len(),
                transport.compactions.load(Ordering::Relaxed),
            )
        })
        .unwrap();
        let checkpoint_response = match checkpoint_output {
            harness::turn::JobOutput::Completed(Ok(response)) => response,
            other => panic!("real Haskell checkpoint effect failed: {other:?}"),
        };
        let committed_run =
            serde_json::to_value(tidepool_runtime::session::WorkbenchRunStatus::Committed).unwrap();
        let committed_item =
            serde_json::to_value(tidepool_runtime::session::WorkbenchItemStatus::Committed)
                .unwrap();
        assert_eq!(
            checkpoint_response["status"], committed_run,
            "{checkpoint_response}"
        );
        let checkpoint_items = checkpoint_response["items"]
            .as_array()
            .expect("WorkbenchResponse.items must be an array");
        let checkpoint_item = checkpoint_items
            .last()
            .expect("checkpoint workbench response must contain the result item");
        assert_eq!(
            checkpoint_item["status"], committed_item,
            "{checkpoint_response}"
        );
        assert_eq!(checkpoint_item["output"], "True", "{checkpoint_response}");
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
            requests[2]
                .input
                .iter()
                .any(|item| item.0.to_string().contains("42")),
            "post-compaction history lost the settled Haskell result: {:#?}",
            requests[2].input
        );
        assert!(matches!(
            conversation.input_observation(envelope_id).unwrap(),
            InputObservation::Included(_)
        ));
        drop(requests);
        let mut reconnect_identity = None;
        for _ in 0..2 {
            let mut request = format!("ws://{}/api/ws", host.address)
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
            let actor_identity = frame["snapshot"]["actors"][0]["identity"].clone();
            if let Some(previous) = reconnect_identity.as_ref() {
                assert_eq!(
                    &actor_identity, previous,
                    "actor identity changed on reconnect"
                );
            } else {
                reconnect_identity = Some(actor_identity);
            }
            let command_receipts = frame["snapshot"]["commandReceipts"]
                .as_array()
                .expect("reconnect snapshot retains command receipts");
            assert!(
                command_receipts.iter().any(|command_receipt| {
                    command_receipt["commandId"] == command_id
                        && command_receipt["outcome"] == "admitted"
                        && command_receipt["envelopeId"] == envelope_id.to_string()
                }),
                "reconnect snapshot lost the admitted browser input receipt: {command_receipts:?}"
            );
            socket.close(None).await.unwrap();
        }
        host.stop().await.unwrap();
        let rotated_secret = "rotated-embedded-browser-test-secret-32-bytes";
        std::fs::write(
            settings.session_secret_file.as_ref().unwrap(),
            rotated_secret,
        )
        .unwrap();
        let (idle_provider, _held_requests) =
            crate::actor_host::test_campaign::hosted_script_provider();
        let restarted = HostedTestRuntime::start(&settings, &idle_provider)
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
        restarted.stop().await.unwrap();
    }

    #[tokio::test]
    async fn request_snapshot_rejects_closed_actor_before_installation_clears() {
        let campaign = TestCampaign::start().await;
        campaign
            .run_scenario(|campaign| {
                Box::pin(async move {
                    let actor = campaign.actor.identity();
                    let installation = Arc::new(EmbeddedPolicyInstallation::from_installation(
                        &campaign.root_installation,
                    ));
                    let identity = HostIdentity {
                        run: super::super::runtime_namespace(campaign.session_root.path()),
                        actor: AgentPath("/root".into()),
                        incarnation: actor.incarnation.0.to_string(),
                    };
                    let scratch_runtime =
                        EmbeddedHarnessRuntime::open(campaign.session_root.path(), 1).unwrap();
                    for foreign in [
                        HostIdentity {
                            incarnation: "wrong-incarnation".into(),
                            ..identity.clone()
                        },
                        HostIdentity {
                            run: "another-run".into(),
                            ..identity.clone()
                        },
                    ] {
                        assert!(scratch_runtime
                            .attach(foreign, campaign.actor.clone(), installation.clone(), None)
                            .is_err());
                    }
                    let (wakes, _incoming) = mpsc::unbounded_channel();
                    let scratch = tempfile::tempdir().unwrap();
                    let store = Arc::new(Store::open(scratch.path().join("store.sqlite")).unwrap());
                    let host = EmbeddedHostActor::new(
                        identity,
                        campaign.actor.clone(),
                        installation.clone(),
                        store,
                        wakes,
                        Arc::new(EmbeddedRoundControl::default()),
                    )
                    .unwrap();
                    let held_store_transaction = campaign.actor.admit_transaction().unwrap();
                    let retiring_actor = campaign.actor.clone();
                    let retiring = tokio::spawn(async move {
                        retiring_actor
                            .shutdown(ActorTerminal {
                                kind: ActorExitKind::Cancelled,
                                summary: "request snapshot retirement race".into(),
                                diagnostic: None,
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
                })
            })
            .await;
    }
}
