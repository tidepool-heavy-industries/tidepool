use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
};

use parking_lot::Mutex as ParkingMutex;

use exomonad_actor::{
    ActorAdmissionLease, ActorExitKind, ActorRef, ActorTerminal, CheckpointLease,
    HostedCheckpointAttachment, HostedCheckpointCapture, HostedCheckpointCaptureError,
    LocalActorRef, ResidentToolError, WorkbenchCancellationOutcome,
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
        CallContext, CancellationAcknowledgment, CancellationOwner, JobHandle, Provider,
        ProviderError, ToolFailure,
    },
    store::Store,
    turn::JobScheduler,
};
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};

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
    output_observer: OnceLock<harness::server::ServerControl>,
    recovery: OnceLock<Arc<super::embedded_recovery::EmbeddedApplicationRecovery>>,
}

impl EmbeddedHarnessRuntime {
    pub(super) fn open(run_root: &Path, concurrent_jobs: usize) -> Result<Self, EmbeddedError> {
        let harness_root = run_root.join("harness");
        std::fs::create_dir_all(&harness_root)
            .map_err(|error| EmbeddedError::Binding(error.to_string()))?;
        Ok(Self {
            run: super::runtime_namespace(run_root),
            output_observer: OnceLock::new(),
            recovery: OnceLock::new(),
            store: Arc::new(Store::open(harness_root.join("store.sqlite"))?),
            scheduler: Arc::new(
                JobScheduler::new(concurrent_jobs)
                    .map_err(|error| EmbeddedError::Binding(error.to_string()))?,
            ),
        })
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
        self.prepare_application(actor_identity, &identity)?;
        let (wakes, incoming) = mpsc::unbounded_channel();
        let round_control = Arc::new(EmbeddedRoundControl::default());
        let host = Arc::new(EmbeddedHostActor::new(
            identity,
            actor,
            installation,
            self.store.clone(),
            wakes,
            round_control.clone(),
        )?);
        let conversation = Arc::new(Conversation::attach(
            self.store.clone(),
            host.clone(),
            parent,
        )?);
        self.bind_application(actor_identity, &conversation)?;
        Ok(EmbeddedConversation {
            conversation,
            incoming,
            round_control,
        })
    }

    pub(super) fn attach_checkpoint(
        &self,
        identity: HostIdentity,
        actor: LocalActorRef,
        installation: Arc<EmbeddedPolicyInstallation>,
        lease: &CheckpointLease,
        gate: &exomonad_actor::ForkGroupGate,
        captured: Arc<EmbeddedHostedCheckpoint>,
    ) -> Result<EmbeddedConversation, EmbeddedError> {
        if identity.run != self.run {
            return Err(EmbeddedError::Binding(
                "embedded checkpoint child belongs to another run".into(),
            ));
        }
        if captured.issuer != lease.issuer {
            return Err(EmbeddedError::Binding(
                "embedded checkpoint issuer mismatch".into(),
            ));
        }
        let checkpoint = match gate
            .publication()
            .map_err(|error| EmbeddedError::Binding(error.to_string()))?
        {
            exomonad_actor::ForkGroupPublication::Deferred => captured.cuts.deferred(),
            exomonad_actor::ForkGroupPublication::Captured => captured.cuts.before_call(),
        };
        let parent = checkpoint.origin().clone();
        let actor_identity = actor.identity();
        self.prepare_application(actor_identity, &identity)?;
        let (wakes, incoming) = mpsc::unbounded_channel();
        let round_control = Arc::new(EmbeddedRoundControl::default());
        let host = Arc::new(EmbeddedHostActor::new(
            identity,
            actor,
            installation,
            self.store.clone(),
            wakes,
            round_control.clone(),
        )?);
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
        Ok(EmbeddedConversation {
            conversation,
            incoming,
            round_control,
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
    pub(super) round_control: Arc<EmbeddedRoundControl>,
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
}

impl EmbeddedHostActor {
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
            .complete(tidepool_runtime::session::WorkbenchForkBoundary::Hosted(
                original,
            ))
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
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
        let manifest = snapshot.manifest();
        let dispatcher: Arc<dyn Provider> = Arc::new(EmbeddedDispatcher {
            identity: self.identity.clone(),
            issuer: self.actor.identity(),
            snapshot,
            store: self.store.clone(),
        });
        Ok(Arc::new(ToolSurface::from_manifest(
            version, manifest, dispatcher,
        )?))
    }

    async fn wake(&self, envelope_id: i64) -> Result<(), String> {
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
    pub(super) fn child_path(&self, actor: ActorRef) -> AgentPath {
        AgentPath(format!(
            "{}/a{}_i{}",
            self.cuts.deferred().origin().0,
            actor.id.0,
            actor.incarnation.0
        ))
    }
}

struct EmbeddedCheckpointCapture {
    store: Arc<Store>,
    identity: HostIdentity,
    issuer: ActorRef,
    operation: OperationId,
}

impl HostedCheckpointCapture for EmbeddedCheckpointCapture {
    fn capture(
        &self,
        name: &str,
        boundary: &tidepool_runtime::session::WorkbenchForkBoundary,
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
        let cuts = self
            .store
            .capture_checkpoint_cuts(&self.operation, &metadata, Arc::new(()))
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
        capture_checkpoints: bool,
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
        let checkpoint_capture = (capture_checkpoints && name == "haskell").then(|| {
            Arc::new(EmbeddedCheckpointCapture {
                store: self.store.clone(),
                identity: self.identity.clone(),
                issuer: self.issuer,
                operation: operation.clone(),
            }) as Arc<dyn HostedCheckpointCapture>
        });
        self.snapshot
            .dispatch(
                name.to_owned(),
                arguments,
                invocation_context,
                checkpoint_capture,
            )
            .await
            .map_err(provider_tool_error)
    }
}

fn provider_tool_error(error: ResidentToolError) -> ProviderError {
    let diagnostic = match &error {
        ResidentToolError::Invocation(exomonad_actor::KernelInvocationFailure::Workbench(
            failure,
        )) => failure.diagnostic.as_ref(),
        _ => None,
    };
    let failure = match diagnostic {
        Some(diagnostic) => ToolFailure::with_metadata(
            error.to_string(),
            serde_json::to_value(diagnostic)
                .expect("failure diagnostics contain only serializable data"),
        ),
        None => error.to_string().into(),
    };
    ProviderError::Tool(failure)
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
        self.dispatch(name, ToolArguments::Structured(arguments), context, false)
            .await
    }

    async fn call_custom_with_context(
        &self,
        name: &str,
        input: String,
        context: CallContext,
    ) -> Result<Value, ProviderError> {
        self.dispatch(name, ToolArguments::Raw(input), context, true)
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
            Ok(
                WorkbenchCancellationOutcome::Expired { reply, .. }
                | WorkbenchCancellationOutcome::PublicationSettled { reply, .. },
            ) => {
                let result = reply
                    .map_err(ResidentToolError::Invocation)
                    .and_then(|response| {
                        serde_json::to_value(response).map_err(ResidentToolError::Encoding)
                    })
                    .map_err(provider_tool_error)
                    .map_err(ProviderError::into_tool_failure);
                CancellationAcknowledgment::Completed(result)
            }
            Ok(outcome) => CancellationAcknowledgment::Unconfirmed(format!("{outcome:?}")),
            Err(error) => CancellationAcknowledgment::Unconfirmed(error.to_string()),
        }
    }
}

#[cfg(test)]
mod round_control_tests {
    use super::*;

    #[test]
    fn embedded_tool_failure_preserves_classification_and_original_error_text() {
        let diagnostic = tidepool_toolchain::failclass::classify_compile(
            &tidepool_toolchain::CompileError::ExtractFailed("retained owner missing".into()),
        );
        let error =
            ResidentToolError::Invocation(exomonad_actor::KernelInvocationFailure::Workbench(
                exomonad_actor::KernelWorkbenchFailure {
                    actor: ActorRef::first(exomonad_actor::ActorId(7)),
                    receipts: vec![],
                    failed_index: 0,
                    total: 1,
                    detail: "retained owner missing".into(),
                    diagnostic: Some(diagnostic),
                },
            ));
        let original = error.to_string();
        let failure = provider_tool_error(error).into_tool_failure();
        assert_eq!(failure.message(), format!("tool failed: {original}"));
        assert_eq!(failure.metadata().unwrap()["class"], "version-skew");
        assert_eq!(failure.metadata().unwrap()["phase"], "compile");
        assert_eq!(
            failure.metadata().unwrap()["cause"]["kind"],
            "extractor_contract"
        );
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
    use crate::actor_host::embedded_projection::{EmbeddedProjection, LifecycleState};
    use crate::actor_host::embedded_service::{
        attach_actor, drive_conversation_with_transport, submit_browser_command, EmbeddedService,
    };
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
            public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
            public_origin: None,
            asset_root: assets,
            browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
            session_secret_file: Some(secret_file),
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
        let (lifecycle, _lifecycle_rx) = tokio::sync::watch::channel((
            Some(actor),
            harness::server::HostActorLifecycle::Waiting,
        ));
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
                actor,
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
        let command_id = command.command_id.clone();
        let input_receipt = submit_browser_command(command, &conversation, &service.control)
            .await
            .unwrap();
        assert!(input_receipt.wake_error.is_none(), "{input_receipt:?}");
        let store = service.runtime.store();
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
            Duration::from_secs(30),
            service.runtime.scheduler().wait(&claim.operation),
        )
        .await
        .expect("real Haskell output did not settle while the scripted round was held")
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
            Duration::from_secs(30),
            service
                .runtime
                .scheduler()
                .wait(&checkpoint_claim.operation),
        )
        .await
        .expect("real Haskell checkpoint effect did not settle")
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
        cancellation.send_replace(true);
        let engine_result = tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap();
        match engine_result {
            Ok(()) => {}
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
            requests[2]
                .input
                .iter()
                .any(|item| item.0.to_string().contains("42")),
            "post-compaction history lost the settled Haskell result: {:#?}",
            requests[2].input
        );
        assert!(matches!(
            conversation
                .input_observation(input_receipt.envelope_id)
                .unwrap(),
            InputObservation::Included(_)
        ));
        drop(requests);
        let mut projection = EmbeddedProjection::default();
        projection.attached(actor, conversation.identity());
        projection
            .publish(
                &service.control,
                &conversation.identity().run,
                &campaign.forest.inspect_host_graph(),
                &LifecycleState::default(),
                |_| conversation.active_round(),
                |identity| {
                    service
                        .runtime
                        .store()
                        .embedded_agent_head(identity)
                        .map(|head| head.map(|request| request.0))
                },
                |_| true,
            )
            .unwrap();
        let mut reconnect_identity = None;
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
                        && command_receipt["envelopeId"] == input_receipt.envelope_id.to_string()
                }),
                "reconnect snapshot lost the admitted browser input receipt: {command_receipts:?}"
            );
            socket.close(None).await.unwrap();
        }
        service.shutdown().await.unwrap();
        drop(service);
        let rotated_secret = "rotated-embedded-browser-test-secret-32-bytes";
        std::fs::write(
            settings.session_secret_file.as_ref().unwrap(),
            rotated_secret,
        )
        .unwrap();
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
