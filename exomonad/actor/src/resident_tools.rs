//! Transport-neutral handle for one resident Haskell tool policy.
//!
//! The policy owns no machine, continuation, scheduler, or host protocol. It
//! exposes an immutable tool surface and submits typed invocations to the
//! owning local actor.

use std::{any::Any, future::Future, pin::Pin, sync::Arc};

use exomonad_tool::{HostedTool, ToolInvocation, ToolInvocationContext};
use tidepool_runtime::session::{
    PublicationCancellation, PublicationDecision, PublicationPhase, ResidentHole,
    WorkbenchExecutionId, WorkbenchRequest,
};
use tokio::sync::oneshot;

const WORKBENCH_IDLE: u8 = 0;
const WORKBENCH_SLEEPING: u8 = 1;
const WORKBENCH_CANCEL_REQUESTED: u8 = 2;
const WORKBENCH_EXPIRED: u8 = 3;
const WORKBENCH_CANCELLED: u8 = 4;
const SLEEP_NONE: u8 = 0;
const SLEEP_EXPIRED: u8 = 1;
const SLEEP_CANCELLED: u8 = 2;
const SLEEP_UNCONFIRMED: u8 = 3;

/// Terminal evidence for an exact resident workbench interruption attempt.
#[derive(Debug, Clone)]
pub enum WorkbenchCancellationOutcome {
    Cancelled {
        execution: WorkbenchExecutionId,
        reply: crate::KernelWorkbenchReply,
    },
    Expired {
        execution: WorkbenchExecutionId,
        reply: crate::KernelWorkbenchReply,
    },
    PublicationSettled {
        execution: WorkbenchExecutionId,
        reply: crate::KernelWorkbenchReply,
    },
    Unconfirmed {
        execution: WorkbenchExecutionId,
    },
    NotSleeping {
        execution: WorkbenchExecutionId,
    },
    UnknownEvaluation {
        execution: WorkbenchExecutionId,
    },
}

/// Authoritative state of one exact hosted workbench call after transport loss.
#[derive(Debug, Clone)]
pub enum WorkbenchBoundaryReconciliation {
    Pending,
    Recovered { reply: crate::KernelWorkbenchReply },
    Settled,
}

pub struct WorkbenchExecutionControl {
    pub(crate) invocation: Option<WorkbenchCallKey>,
    publication: Arc<PublicationDecision>,
    publication_waited: std::sync::atomic::AtomicBool,
    phase: std::sync::atomic::AtomicU8,
    sleep_outcome: std::sync::atomic::AtomicU8,
    changed: tokio::sync::Notify,
    #[cfg(test)]
    cancellation_observed: parking_lot::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    settlement: tokio::sync::watch::Sender<Option<crate::KernelWorkbenchReply>>,
}

impl std::fmt::Debug for WorkbenchExecutionControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkbenchExecutionControl")
            .field("invocation", &self.invocation)
            .field(
                "phase",
                &self.phase.load(std::sync::atomic::Ordering::Acquire),
            )
            .field("settled", &self.terminal_reply().is_some())
            .finish_non_exhaustive()
    }
}

impl WorkbenchExecutionControl {
    pub(crate) fn untracked() -> Arc<Self> {
        Self::new(None)
    }

    fn new(invocation: Option<WorkbenchCallKey>) -> Arc<Self> {
        Arc::new(Self {
            invocation,
            publication: PublicationDecision::new(),
            publication_waited: std::sync::atomic::AtomicBool::new(false),
            phase: std::sync::atomic::AtomicU8::new(WORKBENCH_IDLE),
            sleep_outcome: std::sync::atomic::AtomicU8::new(SLEEP_NONE),
            changed: tokio::sync::Notify::new(),
            #[cfg(test)]
            cancellation_observed: parking_lot::Mutex::new(None),
            settlement: tokio::sync::watch::channel(None).0,
        })
    }

    pub(crate) fn publication_decision(&self) -> Arc<PublicationDecision> {
        Arc::clone(&self.publication)
    }

    pub(crate) fn arm_sleep(&self) {
        #[allow(
            clippy::expect_used,
            reason = "arm_sleep is called once per workbench execution's own \
                      sleep boundary and this control is not shared across \
                      concurrent executions; a failed exchange means the \
                      caller's own single-execution invariant broke, which \
                      should panic rather than be silently ignored"
        )]
        self.phase
            .compare_exchange(
                WORKBENCH_IDLE,
                WORKBENCH_SLEEPING,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .expect("one workbench execution cannot overlap sleep boundaries");
    }

    pub(crate) async fn wait_for_cancellation(&self) {
        loop {
            // Capture the notification generation before observing the phase: a
            // cancellation can notify between that observation and the await.
            let notified = self.changed.notified();
            if self.phase.load(std::sync::atomic::Ordering::Acquire) == WORKBENCH_CANCEL_REQUESTED {
                return;
            }
            #[cfg(test)]
            if let Some(observed) = self.cancellation_observed.lock().take() {
                observed();
            }
            notified.await;
        }
    }

    pub(crate) fn claim_expiry(&self) -> bool {
        let claimed = self
            .phase
            .compare_exchange(
                WORKBENCH_SLEEPING,
                WORKBENCH_EXPIRED,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok();
        if claimed {
            self.sleep_outcome
                .store(SLEEP_EXPIRED, std::sync::atomic::Ordering::Release);
        }
        claimed
    }

    pub(crate) fn acknowledge_cancellation(&self) {
        let previous = self
            .phase
            .swap(WORKBENCH_CANCELLED, std::sync::atomic::Ordering::AcqRel);
        debug_assert_eq!(previous, WORKBENCH_CANCEL_REQUESTED);
        self.sleep_outcome
            .store(SLEEP_CANCELLED, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn finish_sleep(&self) {
        // best-effort CAS: only advance EXPIRED -> IDLE; if the phase moved
        // elsewhere in the meantime (e.g. a concurrent cancellation) that
        // transition owns the state instead, and this one is a no-op.
        self.phase
            .compare_exchange(
                WORKBENCH_EXPIRED,
                WORKBENCH_IDLE,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .ok();
    }

    /// The actor publishes the first terminal reply; transport failure may
    /// fill the slot only when no actor-owned reply arrived.
    pub(crate) fn settle(&self, reply: crate::KernelWorkbenchReply) {
        if self.settlement.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(reply);
            true
        }) {
            self.changed.notify_waiters();
        }
    }

    fn admit_cancellation(&self) -> (bool, Option<PublicationCancellation>) {
        let mut claimed = false;
        let publication = self.publication.request_cancellation_if(|| {
            claimed = self
                .phase
                .compare_exchange(
                    WORKBENCH_SLEEPING,
                    WORKBENCH_CANCEL_REQUESTED,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .is_ok();
            claimed
        });
        if claimed {
            self.changed.notify_waiters();
        }
        if matches!(
            publication,
            Some(
                PublicationCancellation::PendingCommitOutcome
                    | PublicationCancellation::AlreadyPublished
                    | PublicationCancellation::AlreadyTerminated
            )
        ) {
            self.publication_waited
                .store(true, std::sync::atomic::Ordering::Release);
        }
        (claimed, publication)
    }

    pub(crate) fn request_cancellation(&self) -> bool {
        self.admit_cancellation().0
    }

    pub(crate) fn cancellation_requested(&self) -> bool {
        self.phase.load(std::sync::atomic::Ordering::Acquire) == WORKBENCH_CANCEL_REQUESTED
    }

    /// Whether this is an unsettled model-visible `haskell` call (no tool
    /// namespace) that is computing rather than parked in a cancellable
    /// sleep. Codex cancels exactly such a call before admitting new input,
    /// and `ResidentToolClient::cancel_workbench` answers `NotSleeping` for
    /// it, so an input submitted now cannot be admitted until the cell ends.
    pub(crate) fn is_computing_hosted_cell(&self) -> bool {
        self.invocation
            .as_ref()
            .is_some_and(|invocation| invocation.namespace.is_none())
            && self.terminal_reply().is_none()
            && matches!(
                self.phase.load(std::sync::atomic::Ordering::Acquire),
                WORKBENCH_IDLE | WORKBENCH_EXPIRED
            )
    }

    async fn settled(&self) -> crate::KernelWorkbenchReply {
        let mut settlement = self.settlement.subscribe();
        loop {
            if let Some(reply) = settlement.borrow_and_update().clone() {
                return reply;
            }
            if settlement.changed().await.is_err() {
                unreachable!("workbench execution control retains its settlement sender");
            }
        }
    }

    pub(crate) fn terminal_reply(&self) -> Option<crate::KernelWorkbenchReply> {
        self.settlement.borrow().clone()
    }

    pub(crate) fn cancellation_outcome(
        &self,
        execution: WorkbenchExecutionId,
        reply: crate::KernelWorkbenchReply,
    ) -> WorkbenchCancellationOutcome {
        let phase = self.phase.load(std::sync::atomic::Ordering::Acquire);
        match self
            .sleep_outcome
            .load(std::sync::atomic::Ordering::Acquire)
        {
            SLEEP_CANCELLED => WorkbenchCancellationOutcome::Cancelled { execution, reply },
            _ if self.publication.phase() == PublicationPhase::Published
                || self
                    .publication_waited
                    .load(std::sync::atomic::Ordering::Acquire) =>
            {
                WorkbenchCancellationOutcome::PublicationSettled { execution, reply }
            }
            SLEEP_EXPIRED => WorkbenchCancellationOutcome::Expired { execution, reply },
            SLEEP_NONE if phase == WORKBENCH_CANCEL_REQUESTED => {
                WorkbenchCancellationOutcome::Unconfirmed { execution }
            }
            SLEEP_NONE => WorkbenchCancellationOutcome::NotSleeping { execution },
            _ => WorkbenchCancellationOutcome::Unconfirmed { execution },
        }
    }

    pub(crate) fn mark_unconfirmed(&self) {
        self.sleep_outcome
            .compare_exchange(
                SLEEP_NONE,
                SLEEP_UNCONFIRMED,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .ok();
    }
}

/// Keeps a queued call visible until mailbox admission transfers its control
/// to the actor. A dropped caller withdraws only work never sent to the actor.
struct HostedCellPublication {
    slot: crate::kernel::HostedCellSlot,
    control: Arc<WorkbenchExecutionControl>,
}

impl HostedCellPublication {
    fn publish(actor: &crate::LocalActorRef, control: &Arc<WorkbenchExecutionControl>) -> Self {
        let slot = Arc::clone(actor.hosted_cell());
        slot.publish_transport(Arc::clone(control));
        Self {
            slot,
            control: Arc::clone(control),
        }
    }

    fn accept(&self) {
        self.slot.accept(&self.control);
    }
}

impl Drop for HostedCellPublication {
    fn drop(&mut self) {
        self.slot.withdraw_transport(&self.control);
    }
}

/// A policy waiting for its next invocation.
pub(crate) struct ResidentToolAwait {
    pub(crate) continuation: ResidentHole,
    pub(crate) declarations: Vec<exomonad_tool::ToolDeclaration>,
    pub(crate) synopsis: String,
    pub(crate) initial_user_message: Option<String>,
}

/// A completed invocation waiting for Rust to acknowledge its result.
pub(crate) struct ResidentToolReply {
    pub(crate) continuation: ResidentHole,
    pub(crate) result: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentToolError {
    #[error("resident tool policy is unavailable: {0}")]
    Unavailable(String),
    #[error("invalid resident tool invocation: {0}")]
    InvalidInvocation(String),
    #[error(transparent)]
    Invocation(#[from] crate::KernelInvocationFailure),
    #[error("could not encode resident tool response: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("exact workbench cancellation is unsupported")]
    CancellationUnsupported,
}

fn hosted_admission_failure(
    actor: crate::ActorRef,
    failure: crate::KernelCallFailure,
) -> crate::KernelInvocationFailure {
    match failure {
        crate::KernelCallFailure::MailboxClosed(_) => crate::KernelInvocationFailure::Rejected {
            actor,
            detail: "actor mailbox admission is closed".into(),
        },
        crate::KernelCallFailure::TargetExited(_)
        | crate::KernelCallFailure::TargetUnavailable(_) => {
            crate::KernelInvocationFailure::ActorExited(actor)
        }
        failure => crate::KernelInvocationFailure::Failed {
            actor,
            detail: failure.to_string(),
        },
    }
}

/// A cloneable request handle for one exact actor incarnation.
///
/// Calls are serialized because a resident actor has one turn at a time. The
/// owning actor retains and resumes the Haskell continuation through every
/// boundary reached by the tool handler.
pub struct ResidentToolPolicy {
    tools: Arc<[HostedTool]>,
    instructions: Option<String>,
    client: ResidentToolClient,
}

pub type ResidentToolFuture =
    Pin<Box<dyn Future<Output = Result<serde_json::Value, ResidentToolError>> + Send + 'static>>;

/// Opaque host-owned half of a resident context checkpoint. The actor keeps
/// this share with its checkpoint lease; only the host that created the value
/// can interpret it.
#[derive(Clone)]
pub struct HostedCheckpointAttachment {
    value: Arc<dyn Any + Send + Sync>,
    context: HostedCheckpointContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedCheckpointContext {
    DeferredOnly,
    Captured,
}

impl HostedCheckpointAttachment {
    #[must_use]
    pub fn new<T: Any + Send + Sync>(value: Arc<T>) -> Self {
        Self {
            value,
            context: HostedCheckpointContext::DeferredOnly,
        }
    }

    /// Issued by a trusted host capture owner only after it retains both
    /// the deferred and independently usable immutable context cuts.
    #[must_use]
    pub fn captured<T: Any + Send + Sync>(value: Arc<T>) -> Self {
        Self {
            value,
            context: HostedCheckpointContext::Captured,
        }
    }

    #[must_use]
    pub fn context(&self) -> HostedCheckpointContext {
        self.context
    }

    #[must_use]
    pub fn downcast<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        Arc::downcast(Arc::clone(&self.value)).ok()
    }
}

impl std::fmt::Debug for HostedCheckpointAttachment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HostedCheckpointAttachment(..)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedCheckpointCaptureError {
    Unavailable,
    CaptureFailed,
}

/// Trusted per-call host capability. Concrete hosts close over their exact
/// invocation identity; that identity is never parsed back out of display IDs.
pub trait HostedCheckpointCapture: Send + Sync {
    fn capture(
        &self,
        name: &str,
        boundary: &tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> Result<HostedCheckpointAttachment, HostedCheckpointCaptureError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub enum ResidentToolOutput {
    #[default]
    Value,
    Workbench,
}

/// Transport-neutral interface projected by an actor-local tool host.
/// Implementations retain actor admission and execution ownership behind
/// their dispatcher; a concrete host sees only declarations and typed
/// invocations.
pub trait ResidentToolEndpoint: Send + Sync {
    /// Pin the actor-issued handler/source installation for one model request.
    /// An unsupported endpoint must fail rather than silently dispatch live state.
    fn snapshot_for_request(&self) -> Result<Arc<dyn ResidentToolEndpoint>, ResidentToolError> {
        Err(ResidentToolError::Unavailable(
            "request-scoped installed tools are unavailable".into(),
        ))
    }
    /// Unsupported implementations cannot fabricate an admission barrier.
    fn seal_hosted_work_boxed(
        &self,
    ) -> Pin<
        Box<dyn Future<Output = Result<crate::HostedWorkSeal, ResidentToolError>> + Send + 'static>,
    > {
        Box::pin(async {
            Err(ResidentToolError::Unavailable(
                "hosted-work seal is unsupported".into(),
            ))
        })
    }

    fn tools(&self) -> &[HostedTool];
    /// The owning endpoint chooses interpretation; tool input syntax does not.
    fn output_format(&self) -> ResidentToolOutput {
        ResidentToolOutput::Value
    }
    fn instructions(&self) -> Option<&str>;
    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture;
    /// Dispatch with an exact hosted checkpoint capability. Endpoints that do
    /// not carry this authority fail closed; absence keeps the ordinary path.
    fn dispatch_with_checkpoint_boxed(
        &self,
        invocation: ToolInvocation,
        capture: Option<Arc<dyn HostedCheckpointCapture>>,
    ) -> ResidentToolFuture {
        if capture.is_some() {
            Box::pin(async {
                Err(ResidentToolError::Unavailable(
                    "hosted checkpoint capture is unsupported by this endpoint".into(),
                ))
            })
        } else {
            self.dispatch_boxed(invocation)
        }
    }
    fn cancel_workbench_boxed(
        &self,
        _invocation: ToolInvocationContext,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<WorkbenchCancellationOutcome, ResidentToolError>>
                + Send
                + 'static,
        >,
    > {
        Box::pin(async { Err(ResidentToolError::CancellationUnsupported) })
    }
    fn reconcile_workbench_boxed(
        &self,
        _boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<WorkbenchBoundaryReconciliation, ResidentToolError>>
                + Send
                + 'static,
        >,
    > {
        Box::pin(async { Err(ResidentToolError::CancellationUnsupported) })
    }
    /// Session attachment is deliberately side-effect free. Exact lost-call
    /// recovery is performed through `reconcile_workbench_boxed`.
    fn reattach_boxed(&self) -> ResidentToolFuture {
        Box::pin(async { Ok(serde_json::Value::Null) })
    }
    /// Acknowledge the real, durable result of an enclosing model-visible call.
    fn complete_boxed(
        &self,
        _boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> ResidentToolFuture {
        Box::pin(async { Ok(serde_json::Value::Null) })
    }
}

#[derive(Clone)]
pub(crate) struct ResidentToolClient {
    actor: crate::LocalActorRef,
    dispatch_gate: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct WorkbenchCallKey {
    context_call_id: Option<String>,
    thread_id: String,
    turn_id: String,
    call_id: String,
    namespace: Option<String>,
}

impl From<ToolInvocationContext> for WorkbenchCallKey {
    fn from(context: ToolInvocationContext) -> Self {
        Self {
            context_call_id: context.context_call_id,
            thread_id: context.thread_id,
            turn_id: context.turn_id,
            call_id: context.call_id,
            namespace: context.namespace,
        }
    }
}

impl WorkbenchCallKey {
    pub(crate) fn matches_boundary(
        &self,
        boundary: &tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> bool {
        self.thread_id == boundary.thread_id
            && self.context_call_id.as_deref() == Some(boundary.call_id.as_str())
    }
}

fn execution_id(actor: crate::ActorRef, operation: &WorkbenchCallKey) -> WorkbenchExecutionId {
    fn field(hasher: &mut blake3::Hasher, value: &[u8]) {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value);
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tidepool.workbench-execution.v2\0");
    hasher.update(&actor.id.0.to_le_bytes());
    hasher.update(&actor.incarnation.0.to_le_bytes());
    match &operation.context_call_id {
        Some(context_call_id) => {
            hasher.update(&[1]);
            field(&mut hasher, context_call_id.as_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    field(&mut hasher, operation.thread_id.as_bytes());
    field(&mut hasher, operation.turn_id.as_bytes());
    field(&mut hasher, operation.call_id.as_bytes());
    match &operation.namespace {
        Some(namespace) => {
            hasher.update(&[1]);
            field(&mut hasher, namespace.as_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    let mut digest = [0_u8; 16];
    digest.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    WorkbenchExecutionId::from_digest(digest)
}

impl ResidentToolClient {
    pub(crate) async fn seal(&self) -> Result<crate::HostedWorkSeal, ResidentToolError> {
        self.actor
            .seal_hosted_work()
            .await
            .map_err(ResidentToolError::Invocation)
    }
    pub(crate) fn local(actor: crate::LocalActorRef) -> Self {
        Self {
            actor,
            dispatch_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) async fn cancel_workbench(
        &self,
        invocation: ToolInvocationContext,
    ) -> Result<WorkbenchCancellationOutcome, ResidentToolError> {
        let execution = execution_id(self.actor.identity(), &invocation.clone().into());
        let control = self.actor.hosted_cell().find(|control| {
            control
                .invocation
                .as_ref()
                .is_some_and(|key| execution_id(self.actor.identity(), key) == execution)
        });
        let Some(control) = control else {
            let (reply, receive) = oneshot::channel();
            self.actor
                .address()
                .send_message(crate::KernelMessage::ReconcileWorkbenchCancellation {
                    execution,
                    invocation: Some(invocation),
                    reply: reply.into(),
                })
                .map_err(|_| {
                    ResidentToolError::Unavailable("the owning actor has stopped".into())
                })?;
            return receive.await.map_err(|_| {
                ResidentToolError::Unavailable(
                    "the actor stopped before reconciling the exact evaluation".into(),
                )
            });
        };
        if let Some(reply) = control.terminal_reply() {
            return Ok(control.cancellation_outcome(execution, reply));
        }
        let (claimed, publication) = control.admit_cancellation();
        let phase = control.phase.load(std::sync::atomic::Ordering::Acquire);
        if !claimed
            && !matches!(
                publication,
                Some(
                    PublicationCancellation::PendingCommitOutcome
                        | PublicationCancellation::AlreadyPublished
                        | PublicationCancellation::AlreadyTerminated
                )
            )
            && !matches!(
                phase,
                WORKBENCH_CANCEL_REQUESTED | WORKBENCH_EXPIRED | WORKBENCH_CANCELLED
            )
        {
            return Ok(WorkbenchCancellationOutcome::NotSleeping { execution });
        }
        let reply = if let Some(reply) = control.terminal_reply() {
            Some(reply)
        } else {
            tokio::select! {
                reply = control.settled() => Some(reply),
                _ = self.actor.terminal().wait() => control.terminal_reply(),
            }
        };
        let Some(reply) = reply else {
            control.mark_unconfirmed();
            return Ok(WorkbenchCancellationOutcome::Unconfirmed { execution });
        };
        Ok(control.cancellation_outcome(execution, reply))
    }

    pub(crate) async fn dispatch(
        &self,
        invocation: ToolInvocation,
    ) -> Result<serde_json::Value, ResidentToolError> {
        let _turn = self.dispatch_gate.lock().await;
        let (response, receive) = oneshot::channel();
        self.actor
            .admit_mailbox(crate::KernelMessage::Tool {
                invocation,
                reply: response.into(),
            })
            .map_err(|failure| {
                ResidentToolError::Invocation(hosted_admission_failure(
                    self.actor.identity(),
                    failure,
                ))
            })?;
        receive
            .await
            .map_err(|_| {
                ResidentToolError::Unavailable(
                    "the actor stopped before settling the invocation".into(),
                )
            })?
            .map_err(ResidentToolError::Invocation)
    }

    pub(crate) async fn dispatch_with_checkpoint_capture(
        &self,
        invocation: ToolInvocation,
        capture: Arc<dyn HostedCheckpointCapture>,
    ) -> Result<serde_json::Value, ResidentToolError> {
        let _turn = self.dispatch_gate.lock().await;
        let (response, receive) = oneshot::channel();
        self.actor
            .admit_mailbox(crate::KernelMessage::ToolWithHostedCheckpoint {
                invocation,
                capture,
                reply: response.into(),
            })
            .map_err(|failure| {
                ResidentToolError::Invocation(hosted_admission_failure(
                    self.actor.identity(),
                    failure,
                ))
            })?;
        receive
            .await
            .map_err(|_| {
                ResidentToolError::Unavailable(
                    "the actor stopped before settling the invocation".into(),
                )
            })?
            .map_err(ResidentToolError::Invocation)
    }

    pub(crate) async fn reconcile_workbench(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> Result<WorkbenchBoundaryReconciliation, ResidentToolError> {
        if self
            .actor
            .hosted_cell()
            .find(|control| {
                control.invocation.as_ref().is_some_and(|invocation| {
                    invocation.matches_boundary(&boundary) && control.terminal_reply().is_none()
                })
            })
            .is_some()
        {
            return Ok(WorkbenchBoundaryReconciliation::Pending);
        }
        let (reply, receive) = oneshot::channel();
        self.actor
            .address()
            .send_message(crate::KernelMessage::ReconcileWorkbenchBoundary {
                boundary,
                reply: reply.into(),
            })
            .map_err(|_| ResidentToolError::Unavailable("the owning actor has stopped".into()))?;
        receive.await.map_err(|_| {
            ResidentToolError::Unavailable(
                "actor stopped during exact workbench reconciliation".into(),
            )
        })
    }

    pub(crate) async fn complete(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> Result<serde_json::Value, ResidentToolError> {
        let (reply, receive) = oneshot::channel();
        self.actor
            .address()
            .send_message(crate::KernelMessage::ToolCompleted {
                boundary: boundary.clone(),
                reply: reply.into(),
            })
            .map_err(|_| ResidentToolError::Unavailable("the owning actor has stopped".into()))?;
        let result = receive
            .await
            .map_err(|_| {
                ResidentToolError::Unavailable("actor stopped before tool completion".into())
            })?
            .map_err(ResidentToolError::Invocation)?;
        Ok(result)
    }

    #[cfg(test)]
    pub(crate) async fn dispatch_workbench(
        &self,
        request: WorkbenchRequest,
        invocation: Option<ToolInvocationContext>,
    ) -> Result<serde_json::Value, ResidentToolError> {
        self.dispatch_workbench_issued(request, invocation, None)
            .await
    }

    pub(crate) async fn dispatch_workbench_issued(
        &self,
        request: WorkbenchRequest,
        invocation: Option<ToolInvocationContext>,
        installed_tools: Option<crate::InstalledToolLease>,
    ) -> Result<serde_json::Value, ResidentToolError> {
        self.dispatch_workbench_issued_with_capture(request, invocation, installed_tools, None)
            .await
    }

    pub(crate) async fn dispatch_workbench_issued_with_capture(
        &self,
        mut request: WorkbenchRequest,
        invocation: Option<ToolInvocationContext>,
        installed_tools: Option<crate::InstalledToolLease>,
        hosted_checkpoint_capture: Option<Arc<dyn HostedCheckpointCapture>>,
    ) -> Result<serde_json::Value, ResidentToolError> {
        let Some(invocation) = invocation else {
            if hosted_checkpoint_capture.is_some() {
                return Err(ResidentToolError::Unavailable(
                    "hosted checkpoint capture requires an exact provider invocation".into(),
                ));
            }
            let _turn = self.dispatch_gate.lock().await;
            let control = WorkbenchExecutionControl::untracked();
            return self
                .dispatch_registered_workbench(request, control, None, installed_tools, None)
                .await;
        };
        if let Some(context_call_id) = &invocation.context_call_id {
            request =
                request.with_fork_boundary(tidepool_runtime::session::WorkbenchForkBoundary {
                    thread_id: invocation.thread_id.clone(),
                    call_id: context_call_id.clone(),
                });
        }
        let operation = WorkbenchCallKey::from(invocation);
        let execution = execution_id(self.actor.identity(), &operation);
        request = request.with_execution_id(execution.clone());
        let control = WorkbenchExecutionControl::new(Some(operation.clone()));
        // Visible on the actor from before the dispatch gate until this call
        // returns or is dropped: while queued on the gate or computing,
        // `cancel_workbench` cannot interrupt it, and the host's delivery
        // pump must not start an input exchange that would wait on it.
        let published = HostedCellPublication::publish(&self.actor, &control);
        let _turn = self.dispatch_gate.lock().await;
        {
            // The cell runs under the actor span, so its span cannot be a
            // child of the tool call. This event is the join: the provider's
            // call id and the execution id the cell span carries, recorded
            // while both are in one scope.
            tracing::info!(
                actor = %self.actor.identity(),
                execution = %execution,
                call_id = %operation.call_id,
                context_call_id = operation.context_call_id.as_deref().unwrap_or(""),
                turn_id = %operation.turn_id,
                items = request.items.len(),
                "workbench cell dispatched to its actor"
            );
        }
        self.dispatch_registered_workbench(
            request,
            control,
            Some(&published),
            installed_tools,
            hosted_checkpoint_capture,
        )
        .await
    }

    async fn dispatch_registered_workbench(
        &self,
        request: WorkbenchRequest,
        control: Arc<WorkbenchExecutionControl>,
        publication: Option<&HostedCellPublication>,
        installed_tools: Option<crate::InstalledToolLease>,
        hosted_checkpoint_capture: Option<Arc<dyn HostedCheckpointCapture>>,
    ) -> Result<serde_json::Value, ResidentToolError> {
        let (response, receive) = oneshot::channel();
        if let Err(error) = self
            .actor
            .admit_mailbox(crate::KernelMessage::Workbench {
                invocation: crate::ActorWorkbenchInvocation::issued(
                    request,
                    installed_tools,
                    hosted_checkpoint_capture,
                ),
                control: Some(Arc::clone(&control)),
                reply: response.into(),
            })
            .map_err(|failure| hosted_admission_failure(self.actor.identity(), failure))
        {
            control.settle(Err(error.clone()));
            return Err(ResidentToolError::Invocation(error));
        }
        if let Some(publication) = publication {
            publication.accept();
        }
        let reply = match receive.await {
            Ok(reply) => reply,
            Err(_) => {
                control.mark_unconfirmed();
                control.settle(Err(crate::KernelInvocationFailure::ActorExited(
                    self.actor.identity(),
                )));
                return Err(ResidentToolError::Unavailable(
                    "the actor stopped before settling the workbench invocation".into(),
                ));
            }
        };
        let response = reply.map_err(ResidentToolError::Invocation)?;
        serde_json::to_value(response).map_err(ResidentToolError::Encoding)
    }
}

impl ResidentToolPolicy {
    #[must_use]
    pub fn tools(&self) -> &[HostedTool] {
        &self.tools
    }

    #[must_use]
    pub fn instructions(&self) -> Option<&str> {
        self.instructions.as_deref()
    }

    pub async fn dispatch(
        &self,
        invocation: ToolInvocation,
    ) -> Result<serde_json::Value, ResidentToolError> {
        if !self
            .tools
            .iter()
            .any(|tool| tool.name() == invocation.name && tool.accepts(&invocation.arguments))
        {
            return Err(ResidentToolError::InvalidInvocation(
                "unknown tool or invalid argument kind".into(),
            ));
        }
        self.client.dispatch(invocation).await
    }
}

impl ResidentToolEndpoint for ResidentToolPolicy {
    fn seal_hosted_work_boxed(
        &self,
    ) -> Pin<
        Box<dyn Future<Output = Result<crate::HostedWorkSeal, ResidentToolError>> + Send + 'static>,
    > {
        let actor = self.client.actor.clone();
        Box::pin(async move {
            actor
                .seal_hosted_work()
                .await
                .map_err(ResidentToolError::Invocation)
        })
    }

    fn tools(&self) -> &[HostedTool] {
        self.tools()
    }

    fn instructions(&self) -> Option<&str> {
        self.instructions()
    }

    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
        let client = self.client.clone();
        let tools = self.tools.clone();
        Box::pin(async move {
            if !tools
                .iter()
                .any(|tool| tool.name() == invocation.name && tool.accepts(&invocation.arguments))
            {
                return Err(ResidentToolError::InvalidInvocation(
                    "unknown tool or invalid argument kind".into(),
                ));
            }
            client.dispatch(invocation).await
        })
    }

    fn dispatch_with_checkpoint_boxed(
        &self,
        invocation: ToolInvocation,
        capture: Option<Arc<dyn HostedCheckpointCapture>>,
    ) -> ResidentToolFuture {
        let client = self.client.clone();
        let tools = self.tools.clone();
        Box::pin(async move {
            if !tools
                .iter()
                .any(|tool| tool.name() == invocation.name && tool.accepts(&invocation.arguments))
            {
                return Err(ResidentToolError::InvalidInvocation(
                    "unknown tool or invalid argument kind".into(),
                ));
            }
            match capture {
                Some(capture) => {
                    client
                        .dispatch_with_checkpoint_capture(invocation, capture)
                        .await
                }
                None => client.dispatch(invocation).await,
            }
        })
    }
}

pub(crate) fn install_local_resident_tools(
    actor: crate::LocalActorRef,
    awaiting: &ResidentToolAwait,
) -> ResidentToolPolicy {
    ResidentToolPolicy {
        tools: awaiting
            .declarations
            .iter()
            .cloned()
            .map(HostedTool::from)
            .collect::<Vec<_>>()
            .into(),
        instructions: (!awaiting.synopsis.is_empty()).then(|| awaiting.synopsis.clone()),
        client: ResidentToolClient::local(actor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_runtime::session::{WorkbenchResponse, WorkbenchRunStatus};

    struct LegacyEndpoint {
        dispatches: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ResidentToolEndpoint for LegacyEndpoint {
        fn tools(&self) -> &[HostedTool] {
            &[]
        }

        fn instructions(&self) -> Option<&str> {
            None
        }

        fn dispatch_boxed(&self, _invocation: ToolInvocation) -> ResidentToolFuture {
            self.dispatches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Ok(serde_json::Value::Null) })
        }
    }

    struct TestCheckpointCapture;

    impl HostedCheckpointCapture for TestCheckpointCapture {
        fn capture(
            &self,
            _name: &str,
            _boundary: &tidepool_runtime::session::WorkbenchForkBoundary,
        ) -> Result<HostedCheckpointAttachment, HostedCheckpointCaptureError> {
            Ok(HostedCheckpointAttachment::new(Arc::new(())))
        }
    }

    #[tokio::test]
    async fn endpoints_without_checkpoint_support_fail_closed_but_none_uses_legacy_dispatch() {
        let dispatches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let endpoint = LegacyEndpoint {
            dispatches: Arc::clone(&dispatches),
        };
        let invocation = || ToolInvocation {
            context: None,
            name: "legacy".into(),
            arguments: exomonad_tool::ToolArguments::Structured(serde_json::Value::Null),
        };
        assert!(matches!(
            endpoint
                .dispatch_with_checkpoint_boxed(
                    invocation(),
                    Some(Arc::new(TestCheckpointCapture)),
                )
                .await,
            Err(ResidentToolError::Unavailable(_))
        ));
        assert_eq!(dispatches.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(matches!(
            endpoint
                .dispatch_with_checkpoint_boxed(invocation(), None)
                .await,
            Ok(serde_json::Value::Null)
        ));
        assert_eq!(dispatches.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    fn call_key(call_id: &str) -> WorkbenchCallKey {
        ToolInvocationContext {
            context_call_id: Some("outer-call".into()),
            thread_id: "thread".into(),
            turn_id: "turn".into(),
            call_id: call_id.into(),
            namespace: Some("actor".into()),
        }
        .into()
    }

    #[test]
    fn execution_identity_is_exact_to_actor_and_hosted_call() {
        let actor = crate::ActorRef::first(crate::ActorId(7));
        let original = execution_id(actor, &call_key("call-1"));
        assert_eq!(original, execution_id(actor, &call_key("call-1")));
        assert_ne!(original, execution_id(actor, &call_key("call-2")));
        let mut different_context = call_key("call-1");
        different_context.context_call_id = Some("different-outer-call".into());
        assert_ne!(original, execution_id(actor, &different_context));
        different_context.context_call_id = None;
        assert_ne!(original, execution_id(actor, &different_context));
        assert_ne!(
            original,
            execution_id(
                crate::ActorRef {
                    id: actor.id,
                    incarnation: crate::Incarnation(2),
                },
                &call_key("call-1")
            )
        );
    }

    fn terminal_reply() -> crate::KernelWorkbenchReply {
        Ok(WorkbenchResponse {
            status: WorkbenchRunStatus::Rejected,
            summary: None,
            items: Vec::new(),
            next_index: 0,
            total: 1,
        })
    }

    #[tokio::test]
    async fn exact_workbench_control_linearizes_cancellation_and_settlement() {
        let control = WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        assert!(control.request_cancellation());
        assert!(control.publication_decision().claim_commit().is_none());
        assert!(!control.request_cancellation());
        control.acknowledge_cancellation();
        control.settle(terminal_reply());
        assert_eq!(
            control.phase.load(std::sync::atomic::Ordering::Acquire),
            WORKBENCH_CANCELLED
        );
        assert!(control.settled().await.is_ok());
    }

    #[tokio::test]
    async fn publication_claim_keeps_cancellation_pending_until_owner_settles() {
        let control = WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        let decision = control.publication_decision();
        let claim = decision.claim_commit().unwrap();
        let (claimed, admitted) = control.admit_cancellation();
        assert!(!claimed);
        assert_eq!(
            admitted,
            Some(PublicationCancellation::PendingCommitOutcome)
        );
        assert!(!control.cancellation_requested());
        assert_eq!(
            control.phase.load(std::sync::atomic::Ordering::Acquire),
            WORKBENCH_SLEEPING,
            "a commit claim must prevent the native abort transition"
        );
        assert!(control.terminal_reply().is_none());
        assert!(claim.published());
        let execution = WorkbenchExecutionId::from_digest([7; 16]);
        control.settle(terminal_reply());
        assert!(matches!(
            control.cancellation_outcome(execution, control.settled().await),
            WorkbenchCancellationOutcome::PublicationSettled { reply: Ok(_), .. }
        ));
    }

    #[tokio::test]
    async fn cancellation_after_claim_preserves_before_rename_failure_reply() {
        let control = WorkbenchExecutionControl::untracked();
        let claim = control.publication_decision().claim_commit().unwrap();
        assert_eq!(
            control.admit_cancellation().1,
            Some(PublicationCancellation::PendingCommitOutcome)
        );
        assert!(claim.before_rename_failure());
        let execution = WorkbenchExecutionId::from_digest([8; 16]);
        control.settle(Err(crate::KernelInvocationFailure::Failed {
            actor: crate::ActorRef::first(crate::ActorId(1)),
            detail: "publication failed before visibility".into(),
        }));
        assert!(matches!(
            control.cancellation_outcome(execution, control.settled().await),
            WorkbenchCancellationOutcome::PublicationSettled { reply: Err(_), .. }
        ));
    }

    #[tokio::test]
    async fn cancellation_between_phase_observation_and_wait_is_not_lost() {
        let control = WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        let cancelling = Arc::clone(&control);
        *control.cancellation_observed.lock() = Some(Box::new(move || {
            assert!(cancelling.request_cancellation());
        }));

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            control.wait_for_cancellation(),
        )
        .await
        .expect("cancellation before polling the notification must wake the sleeper");
    }

    #[test]
    fn exact_workbench_control_gives_expiry_one_winner() {
        let control = WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        assert!(control.claim_expiry());
        assert!(!control.request_cancellation());
        control.finish_sleep();
        assert_eq!(
            control.phase.load(std::sync::atomic::Ordering::Acquire),
            WORKBENCH_IDLE
        );
    }

    #[test]
    fn only_an_unsettled_unnamespaced_cell_outside_sleep_is_computing() {
        let mut key = call_key("call-1");
        key.namespace = None;
        let control = WorkbenchExecutionControl::new(Some(key));
        assert!(control.is_computing_hosted_cell());
        control.arm_sleep();
        assert!(
            !control.is_computing_hosted_cell(),
            "a sleep is cancellable"
        );
        assert!(control.claim_expiry());
        assert!(control.is_computing_hosted_cell());
        control.finish_sleep();
        assert!(control.is_computing_hosted_cell());
        control.settle(terminal_reply());
        assert!(!control.is_computing_hosted_cell());

        let namespaced = WorkbenchExecutionControl::new(Some(call_key("call-2")));
        assert!(!namespaced.is_computing_hosted_cell());
        assert!(!WorkbenchExecutionControl::untracked().is_computing_hosted_cell());
    }

    type ReceivedWorkbench = (
        Option<Arc<WorkbenchExecutionControl>>,
        ractor::RpcReplyPort<crate::KernelWorkbenchReply>,
    );

    /// Stands in for the owning actor: hands each workbench dispatch to the
    /// test without running it.
    struct WorkbenchCollector;

    impl ractor::Actor for WorkbenchCollector {
        type Msg = crate::KernelMessage;
        type State = tokio::sync::mpsc::UnboundedSender<ReceivedWorkbench>;
        type Arguments = Self::State;

        async fn pre_start(
            &self,
            _: ractor::ActorRef<Self::Msg>,
            sender: Self::Arguments,
        ) -> Result<Self::State, ractor::ActorProcessingErr> {
            Ok(sender)
        }

        async fn handle(
            &self,
            _: ractor::ActorRef<Self::Msg>,
            message: Self::Msg,
            sender: &mut Self::State,
        ) -> Result<(), ractor::ActorProcessingErr> {
            if let crate::KernelMessage::Workbench { control, reply, .. } = message {
                sender.send((control, reply))?;
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_dispatched_hosted_cell_is_visible_on_its_actor_until_it_ends() {
        let (send, mut received) = tokio::sync::mpsc::unbounded_channel();
        let (address, task) = ractor::Actor::spawn(None, WorkbenchCollector, send)
            .await
            .unwrap();
        let actor = crate::LocalActorRef::new(address.clone(), crate::RetainedActorExit::new());
        let client = ResidentToolClient::local(actor.clone());
        let invocation = ToolInvocationContext {
            context_call_id: Some("outer-call".into()),
            thread_id: "thread".into(),
            turn_id: "turn".into(),
            call_id: "call-1".into(),
            namespace: None,
        };
        let dispatch = |client: ResidentToolClient, invocation: ToolInvocationContext| {
            tokio::spawn(async move {
                client
                    .dispatch_workbench(WorkbenchRequest::from_cell_input("cell"), Some(invocation))
                    .await
            })
        };
        assert!(!actor.hosted_cell_computing());

        let running = dispatch(client.clone(), invocation.clone());
        let (control, reply) = received.recv().await.unwrap();
        let control = control.unwrap();
        assert!(actor.hosted_cell_computing());
        let mut queued_invocation = invocation.clone();
        queued_invocation.call_id = "queued-call".into();
        let queued = dispatch(client.clone(), queued_invocation);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if actor
                    .hosted_cell()
                    .find(|candidate| {
                        candidate
                            .invocation
                            .as_ref()
                            .is_some_and(|key| key.call_id == "queued-call")
                    })
                    .is_some()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("queued call published before the gate");
        queued.abort();
        let _ = queued.await;
        assert!(
            actor.hosted_cell_computing(),
            "abandoned queued call cannot hide active cell"
        );
        control.arm_sleep();
        assert!(
            !actor.hosted_cell_computing(),
            "a sleeping cell is interruptible"
        );
        control.settle(terminal_reply());
        actor.hosted_cell().complete(&control);
        reply.send(terminal_reply()).unwrap();
        running.await.unwrap().unwrap();
        assert!(!actor.hosted_cell_computing());

        // An execution whose reply is lost is cleared too.
        let torn_down = dispatch(client.clone(), invocation.clone());
        let (control, reply) = received.recv().await.unwrap();
        let control = control.unwrap();
        assert!(actor.hosted_cell_computing());
        actor.hosted_cell().complete(&control);
        drop(reply);
        assert!(torn_down.await.unwrap().is_err());
        assert!(!actor.hosted_cell_computing());

        // Dropping the caller leaves the actor's accepted work visible.
        let abandoned = dispatch(client.clone(), invocation);
        let (control, reply) = received.recv().await.unwrap();
        let control = control.unwrap();
        abandoned.abort();
        let _ = abandoned.await;
        assert!(actor.hosted_cell_computing());
        control.settle(terminal_reply());
        actor.hosted_cell().complete(&control);
        drop(reply);
        assert!(!actor.hosted_cell_computing());

        address.stop(None);
        task.await.unwrap();
    }

    #[test]
    fn exact_workbench_control_never_turns_an_unproved_abort_into_cancellation() {
        let execution = WorkbenchExecutionId::from_digest([9; 16]);
        let control = WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        assert!(control.request_cancellation());
        assert!(matches!(
            control.cancellation_outcome(execution, terminal_reply()),
            WorkbenchCancellationOutcome::Unconfirmed { .. }
        ));
    }

    #[test]
    fn rapid_actor_completion_cannot_be_republished_by_transport_handoff() {
        let slot = crate::kernel::HostedCellPublications::default();
        let control =
            WorkbenchExecutionControl::new(Some(WorkbenchCallKey::from(ToolInvocationContext {
                context_call_id: None,
                thread_id: "thread".into(),
                turn_id: "turn".into(),
                call_id: "call".into(),
                namespace: None,
            })));
        slot.publish_transport(Arc::clone(&control));
        slot.claim(&control);
        slot.complete(&control);
        slot.accept(&control);
        assert!(slot.find(|_| true).is_none());
    }
}
