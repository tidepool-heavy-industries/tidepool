//! Transport-neutral handle for one resident Haskell tool policy.
//!
//! The policy owns no machine, continuation, scheduler, or host protocol. It
//! exposes an immutable tool surface and submits typed invocations to the
//! owning local actor.

use std::{any::Any, future::Future, pin::Pin, sync::Arc};

use exomonad_tool::{HostedTool, ToolArguments, ToolInvocation, ToolInvocationContext};
use tidepool_runtime::session::{
    PublicationCancellation, PublicationDecision, PublicationPhase, ResidentHole,
    WorkbenchExecutionId, WorkbenchRequest, WorkbenchResponse,
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

pub(crate) trait WorkbenchReceiptOwner: Send + Sync {
    fn freeze(&self, reply: &mut crate::KernelWorkbenchReply);
}

mod operation_settlement;
pub use operation_settlement::{
    HostedOperationFinalization, HostedOperationSettlement, HostedOperationTerminal,
    ProviderFinalizationKind,
};
pub(crate) use operation_settlement::{HostedOperationWeak, ProviderFinalization};

pub struct WorkbenchExecutionControl {
    pub(crate) invocation: Option<WorkbenchCallKey>,
    pub(crate) provider_finalization: Arc<ProviderFinalization>,
    pub(crate) provider_owner: std::sync::OnceLock<HostedOperationWeak>,
    pub(crate) provider_replay: std::sync::OnceLock<HostedOperationSettlement>,
    publication: Arc<PublicationDecision>,
    native_cancel: Arc<std::sync::atomic::AtomicBool>,
    reservation_attempt: crate::request::WorkbenchReservationAttempt,
    execution: std::sync::OnceLock<WorkbenchExecutionId>,
    context_binding: std::sync::OnceLock<Arc<dyn crate::HostedContextBinding>>,
    receipt_owner: std::sync::OnceLock<Arc<dyn WorkbenchReceiptOwner>>,
    context_cancel_requested: std::sync::atomic::AtomicBool,
    cell_terminal: parking_lot::Mutex<Option<crate::CellExit>>,
    compiler_work: parking_lot::Mutex<Vec<crate::termination::CompilerWorkReceipt>>,
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

    pub(crate) fn from_invocation(
        invocation: Option<exomonad_tool::ToolInvocationContext>,
    ) -> Arc<Self> {
        Self::new(invocation.map(WorkbenchCallKey::from))
    }

    fn new(invocation: Option<WorkbenchCallKey>) -> Arc<Self> {
        Arc::new(Self {
            invocation,
            provider_finalization: Arc::new(ProviderFinalization::default()),
            provider_owner: std::sync::OnceLock::new(),
            provider_replay: std::sync::OnceLock::new(),
            publication: PublicationDecision::new(),
            native_cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            reservation_attempt: crate::request::WorkbenchReservationAttempt::fresh(),
            execution: std::sync::OnceLock::new(),
            context_binding: std::sync::OnceLock::new(),
            receipt_owner: std::sync::OnceLock::new(),
            context_cancel_requested: std::sync::atomic::AtomicBool::new(false),
            cell_terminal: parking_lot::Mutex::new(None),
            compiler_work: parking_lot::Mutex::new(Vec::new()),
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

    pub(crate) fn execution_id(&self, actor: crate::ActorRef) -> WorkbenchExecutionId {
        self.execution
            .get_or_init(|| {
                self.invocation.as_ref().map_or_else(
                    || WorkbenchExecutionId::from_digest(*uuid::Uuid::new_v4().as_bytes()),
                    |invocation| execution_id(actor, invocation),
                )
            })
            .clone()
    }

    pub(crate) fn reservation_owner(
        &self,
        actor: crate::ActorRef,
    ) -> Option<crate::request::RequestReservationOwner> {
        Some(crate::request::RequestReservationOwner::Workbench {
            execution: self.execution_id(actor),
            attempt: self.reservation_attempt.clone(),
        })
    }

    pub(crate) fn native_cancel(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.native_cancel)
    }

    pub(crate) fn bind_context(&self, binding: Arc<dyn crate::HostedContextBinding>) {
        if let Err(binding) = self.context_binding.set(binding) {
            assert!(Arc::ptr_eq(
                self.context_binding
                    .get()
                    .expect("original context binding"),
                &binding
            ));
        }
        if self
            .context_cancel_requested
            .load(std::sync::atomic::Ordering::Acquire)
        {
            self.context_binding.get().expect("bound context").cancel();
        }
    }

    pub(crate) fn has_context_binding(&self) -> bool {
        self.context_binding.get().is_some()
    }

    pub(crate) fn context_cancellation_requested(&self) -> bool {
        self.context_cancel_requested
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Whole-cell completion and operation cancellation share this cutoff.
    /// The ordinary reply remains delivered by LocalActor after finalization.
    pub(crate) fn finish_cell(
        &self,
        execution: WorkbenchExecutionId,
        result: &Result<crate::KernelStep<WorkbenchResponse>, crate::KernelInvocationFailure>,
        cleanup_confirmed: bool,
    ) -> crate::CellExit {
        let mut terminal = self.cell_terminal.lock();
        if let Some(exit) = terminal.as_ref() {
            assert_eq!(exit.execution, execution);
            return exit.clone();
        }
        let exit = crate::CellExit::from_reply(
            execution,
            result,
            cleanup_confirmed
                && self
                    .compiler_close_observations()
                    .iter()
                    .all(|close| close.is_confirmed()),
            // This flag proves cancellation won the arbiter and survives acknowledgement.
            self.native_cancel
                .load(std::sync::atomic::Ordering::Acquire),
        );
        *terminal = Some(exit.clone());
        exit
    }

    pub(crate) fn register_compiler_work(
        &self,
        receipt: crate::termination::CompilerWorkReceipt,
    ) -> bool {
        // This order matches finish_cell: completion fences future admissions.
        let terminal = self.cell_terminal.lock();
        if terminal.is_some() {
            return false;
        }
        self.compiler_work.lock().push(receipt.clone());
        let cancelled = self
            .native_cancel
            .load(std::sync::atomic::Ordering::Acquire);
        drop(terminal);
        if cancelled {
            receipt.request_cancellation();
        }
        true
    }

    pub(crate) fn notify_compiler_close(&self) {
        self.changed.notify_waiters();
    }

    pub(crate) fn compiler_close_observations(&self) -> Vec<crate::termination::CompilerWorkClose> {
        self.compiler_work
            .lock()
            .iter()
            .map(|receipt| receipt.observation())
            .collect()
    }

    fn cell_finished(&self) -> bool {
        self.cell_terminal.lock().is_some()
    }

    pub(crate) fn arm_sleep(&self) {
        // Admission cancellation remains sticky across every owned phase.
        if self.cancellation_requested() {
            return;
        }
        self.sleep_outcome
            .store(SLEEP_NONE, std::sync::atomic::Ordering::Release);
        let _ = self.phase.compare_exchange(
            WORKBENCH_IDLE,
            WORKBENCH_SLEEPING,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
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

    pub(crate) fn bind_receipt_owner(&self, owner: Arc<dyn WorkbenchReceiptOwner>) {
        // Admission and first terminal publication share this cutoff. An
        // already terminated control closes late custody without changing its
        // immutable reply, even before the first display boundary is captured.
        let _terminal = self.cell_terminal.lock();
        if let Err(owner) = self.receipt_owner.set(owner) {
            assert!(
                Arc::ptr_eq(
                    self.receipt_owner.get().expect("original receipt owner"),
                    &owner
                ),
                "an execution control cannot replace its admitted receipt owner"
            );
        }
        if let Some(mut reply) = self.terminal_reply() {
            self.receipt_owner
                .get()
                .expect("bound receipt owner")
                .freeze(&mut reply);
        }
    }

    /// The actor publishes the first terminal reply; transport failure may
    /// fill the slot only when no actor-owned reply arrived.
    /// Issued refusal before native admission. The same cutoff covers schema,
    /// selected-tool and mailbox denial; ordinary settlement is not evidence
    /// that an unconfirmed admitted execution never ran.
    pub(crate) fn settle_not_admitted(
        &self,
        reply: crate::KernelWorkbenchReply,
    ) -> crate::KernelWorkbenchReply {
        self.provider_finalization.reject_before_admission();
        self.settle_reply(reply)
    }

    pub(crate) fn settle(&self, reply: crate::KernelWorkbenchReply) {
        let _ = self.settle_reply(reply);
    }

    pub(crate) fn settle_reply(
        &self,
        mut reply: crate::KernelWorkbenchReply,
    ) -> crate::KernelWorkbenchReply {
        let _terminal = self.cell_terminal.lock();
        if self.settlement.send_if_modified(|current| {
            if let Some(current) = current {
                reply = current.clone();
                return false;
            }
            if let Some(owner) = self.receipt_owner.get() {
                // An exited actor carries actual display settlements through
                // the receipt-bearing failure. A displayless owner still seals
                // its admission while preserving the original failure class.
                let exited = match &reply {
                    Err(crate::KernelInvocationFailure::ActorExited(actor)) => Some(*actor),
                    _ => None,
                };
                if let Some(actor) = exited {
                    let mut projected = Err(crate::KernelInvocationFailure::Failed {
                        actor,
                        detail: "actor exited before its admitted workbench owner settled".into(),
                        receipts: Vec::new(),
                        diagnostic: None,
                    });
                    owner.freeze(&mut projected);
                    if projected
                        .as_ref()
                        .is_err_and(|failure| !failure.receipts().is_empty())
                    {
                        reply = projected;
                    }
                } else {
                    owner.freeze(&mut reply);
                }
            }
            *current = Some(reply.clone());
            true
        }) {
            self.changed.notify_waiters();
        }
        reply
    }

    fn admit_cancellation(&self) -> (bool, Option<PublicationCancellation>) {
        let terminal = self.cell_terminal.lock();
        if terminal.is_some() || self.terminal_reply().is_some() {
            return (false, None);
        }
        self.context_cancel_requested
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(binding) = self.context_binding.get() {
            binding.cancel();
        }
        let mut claimed = false;
        let publication = self.publication.request_cancellation_if(|| {
            claimed = self
                .phase
                .fetch_update(
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                    |phase| {
                        matches!(
                            phase,
                            WORKBENCH_IDLE | WORKBENCH_SLEEPING | WORKBENCH_EXPIRED
                        )
                        .then_some(WORKBENCH_CANCEL_REQUESTED)
                    },
                )
                .is_ok();
            // Keep winning evidence inside the publication decision lock.
            if claimed {
                self.native_cancel
                    .store(true, std::sync::atomic::Ordering::Release);
            }
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
        drop(terminal);
        if claimed {
            let receipts = self.compiler_work.lock().clone();
            for receipt in receipts {
                receipt.request_cancellation();
            }
        }
        (claimed, publication)
    }

    /// Human interaction publication shares the original cell cancellation
    /// cutoff. A winning durable operation is retained even if cancellation
    /// subsequently prevents its Haskell continuation from running.
    pub(crate) fn admit_interaction<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
        let terminal = self.cell_terminal.lock();
        if terminal.is_some()
            || self
                .native_cancel
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return None;
        }
        Some(operation())
    }

    pub(crate) fn request_cancellation(&self) -> bool {
        self.admit_cancellation().0
    }

    pub(crate) fn cancellation_requested(&self) -> bool {
        self.phase.load(std::sync::atomic::Ordering::Acquire) == WORKBENCH_CANCEL_REQUESTED
    }

    pub(crate) fn is_computing_hosted_cell(&self) -> bool {
        self.invocation
            .as_ref()
            .is_some_and(|invocation| invocation.0.namespace.is_none())
            && self.terminal_reply().is_none()
            && matches!(
                self.phase.load(std::sync::atomic::Ordering::Acquire),
                WORKBENCH_IDLE | WORKBENCH_EXPIRED
            )
    }

    pub(crate) fn is_waiting_hosted_cell(&self) -> bool {
        self.invocation
            .as_ref()
            .is_some_and(WorkbenchCallKey::is_original_invocation)
            && self.terminal_reply().is_none()
            && self.phase.load(std::sync::atomic::Ordering::Acquire) == WORKBENCH_SLEEPING
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
        // The first terminal owner reply wins over a later transport result.
        let reply = self.terminal_reply().unwrap_or(reply);
        if let Some(exit) = self.cell_terminal.lock().as_ref() {
            if !exit.cleanup_confirmed {
                return WorkbenchCancellationOutcome::Unconfirmed { execution };
            }
            return match exit.cause {
                crate::CellExitCause::Cancelled => {
                    WorkbenchCancellationOutcome::Cancelled { execution, reply }
                }
                _ => WorkbenchCancellationOutcome::PublicationSettled { execution, reply },
            };
        }
        if matches!(
            &reply,
            Err(crate::KernelInvocationFailure::CleanupUnconfirmed { .. })
        ) {
            return WorkbenchCancellationOutcome::Unconfirmed { execution };
        }
        if matches!(
            self.publication.phase(),
            PublicationPhase::CommitClaimed { .. }
        ) {
            return WorkbenchCancellationOutcome::Unconfirmed { execution };
        }
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
        if control
            .invocation
            .as_ref()
            .is_some_and(|key| key.invocation().model_operation().is_some())
        {
            slot.publish_provider_transport(actor.identity(), Arc::clone(control));
        } else {
            slot.publish_transport(Arc::clone(control));
        }
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
    pub(crate) result: crate::resident_workbench::ToolDispatchReply,
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentToolError {
    #[error("resident tool policy is unavailable: {0}")]
    Unavailable(String),
    #[error(transparent)]
    Declaration(#[from] exomonad_tool::ToolDeclarationError),
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
            receipts: Vec::new(),
            actor,
            detail: "actor mailbox admission is closed".into(),
            diagnostic: None,
        },
        crate::KernelCallFailure::TargetExited(_)
        | crate::KernelCallFailure::TargetUnavailable(_) => {
            crate::KernelInvocationFailure::ActorExited(actor)
        }
        failure => crate::KernelInvocationFailure::Failed {
            receipts: Vec::new(),
            actor,
            detail: failure.to_string(),
            diagnostic: None,
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

/// A completed endpoint dispatch keeps workbench execution receipts typed
/// until the host chooses how to present them to its model provider.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum ResidentToolResponse {
    Workbench(WorkbenchResponse),
    Value(serde_json::Value),
}

impl ResidentToolResponse {
    /// Project a typed dispatch result for existing structured observers.
    /// Model hosts should match the enum and present `Workbench` directly.
    pub fn into_json(self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

pub type ResidentToolDispatchFuture =
    Pin<Box<dyn Future<Output = Result<ResidentToolResponse, ResidentToolError>> + Send + 'static>>;

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
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> Result<HostedCheckpointAttachment, HostedCheckpointCaptureError>;
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
    /// Resolve only an operation already issued by this incarnation's transport.
    /// Optional workbench custody capability. Generic serial tool endpoints do
    /// not issue a native workbench finalization owner.
    fn retained_operation(
        &self,
        _invocation: ToolInvocationContext,
    ) -> Result<HostedOperationSettlement, ResidentToolError> {
        Err(ResidentToolError::Unavailable(
            "retained native operation settlement is unsupported".into(),
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
    fn instructions(&self) -> Option<&str>;
    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolDispatchFuture;
    /// Expand an actor-issued display without compiling Haskell source.
    fn expand_display_boxed(&self, _identity: (i64, i64, i64), _key: i64) -> ResidentToolFuture {
        Box::pin(async {
            Err(ResidentToolError::Unavailable(
                "display expansion is unsupported".into(),
            ))
        })
    }

    /// Dispatch with an exact hosted checkpoint capability. Endpoints that do
    /// not carry this authority fail closed; absence keeps the ordinary path.
    fn dispatch_with_checkpoint_boxed(
        &self,
        invocation: ToolInvocation,
        capture: Option<Arc<dyn HostedCheckpointCapture>>,
    ) -> ResidentToolDispatchFuture {
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

    /// Context authority is supplied by the exact synchronous host invocation.
    fn dispatch_with_context_boxed(
        &self,
        invocation: ToolInvocation,
        capture: Option<Arc<dyn HostedCheckpointCapture>>,
        context: Option<Arc<dyn crate::HostedContextBinding>>,
    ) -> ResidentToolDispatchFuture {
        if context.is_some() {
            Box::pin(async {
                Err(ResidentToolError::Unavailable(
                    "hosted context access is unsupported by this endpoint".into(),
                ))
            })
        } else {
            self.dispatch_with_checkpoint_boxed(invocation, capture)
        }
    }
    /// Pure host argument validation crosses the same dispatch boundary. An
    /// interactive owner issues its operation before accepting or refusing the
    /// validated input; legacy endpoints keep their ordinary dispatch contract.
    fn dispatch_validated_with_context_boxed(
        &self,
        mut invocation: ToolInvocation,
        arguments: Result<ToolArguments, ResidentToolError>,
        capture: Option<Arc<dyn HostedCheckpointCapture>>,
        context: Option<Arc<dyn crate::HostedContextBinding>>,
    ) -> ResidentToolDispatchFuture {
        match arguments {
            Ok(arguments) => {
                invocation.arguments = arguments;
                self.dispatch_with_context_boxed(invocation, capture, context)
            }
            Err(error) => Box::pin(async move { Err(error) }),
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
        _boundary: tidepool_runtime::session::ContextCheckpointBoundary,
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
    /// Discard an exact call's deferred publication after failed Store settlement.
    fn abort_boxed(
        &self,
        _boundary: tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> ResidentToolFuture {
        Box::pin(async { Err(ResidentToolError::CancellationUnsupported) })
    }
    /// Acknowledge the real, durable result of an enclosing model-visible call.
    fn complete_boxed(
        &self,
        _boundary: tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> ResidentToolFuture {
        Box::pin(async { Ok(serde_json::Value::Null) })
    }
}

#[derive(Clone)]
pub(crate) struct ResidentToolClient {
    actor: crate::LocalActorRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct WorkbenchCallKey(ToolInvocationContext);

impl From<ToolInvocationContext> for WorkbenchCallKey {
    fn from(context: ToolInvocationContext) -> Self {
        Self(context)
    }
}

impl WorkbenchCallKey {
    pub(crate) fn original(operation: &exomonad_tool::OriginalOperation) -> Self {
        Self(ToolInvocationContext {
            origin: exomonad_tool::ToolInvocationOrigin::Model(operation.clone()),
            call_id: operation.call_id.clone(),
            namespace: None,
        })
    }

    pub(crate) fn invocation(&self) -> &ToolInvocationContext {
        &self.0
    }

    pub(crate) fn is_original_invocation(&self) -> bool {
        self.0.namespace.is_none()
            && self
                .0
                .model_operation()
                .is_some_and(|original| original.call_id == self.0.call_id)
    }

    pub(crate) fn matches_boundary(
        &self,
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> bool {
        self.0
            .model_operation()
            .is_some_and(|operation| boundary.hosted() == Some(operation))
    }
}

pub(crate) fn execution_id(
    actor: crate::ActorRef,
    operation: &WorkbenchCallKey,
) -> WorkbenchExecutionId {
    fn field(hasher: &mut blake3::Hasher, value: &[u8]) {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value);
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tidepool.workbench-execution.v3\0");
    hasher.update(&actor.id.0.to_le_bytes());
    hasher.update(&actor.incarnation.0.to_le_bytes());
    let (origin, request_id) = match &operation.0.origin {
        exomonad_tool::ToolInvocationOrigin::Model(original) => {
            hasher.update(&[1]);
            field(&mut hasher, original.call_id.as_bytes());
            (&original.origin, &original.request_id)
        }
        exomonad_tool::ToolInvocationOrigin::Direct { origin, request_id } => {
            hasher.update(&[0]);
            (origin, request_id)
        }
    };
    match origin {
        exomonad_tool::ConversationOrigin::External { thread_id } => {
            hasher.update(&[0]);
            field(&mut hasher, thread_id.as_bytes());
        }
        exomonad_tool::ConversationOrigin::Embedded {
            run,
            actor,
            incarnation,
        } => {
            hasher.update(&[1]);
            field(&mut hasher, run.as_bytes());
            field(&mut hasher, actor.as_bytes());
            field(&mut hasher, incarnation.as_bytes());
        }
    }
    field(&mut hasher, request_id.as_bytes());
    field(&mut hasher, operation.0.call_id.as_bytes());
    match &operation.0.namespace {
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

pub(crate) async fn expand_display_response(
    actor: &crate::LocalActorRef,
    identity: (i64, i64, i64),
    key: i64,
) -> Result<WorkbenchResponse, ResidentToolError> {
    let (reply, receive) = oneshot::channel();
    actor
        .admit_mailbox(crate::KernelMessage::Workbench {
            invocation: crate::ActorWorkbenchInvocation::for_display_expansion(identity, key),
            control: Some(crate::WorkbenchExecutionControl::untracked()),
            reply: reply.into(),
        })
        .map_err(|failure| hosted_admission_failure(actor.identity(), failure))
        .map_err(ResidentToolError::Invocation)?;
    let reply = receive
        .await
        .map_err(|_| {
            ResidentToolError::Unavailable("display actor stopped before expansion settled".into())
        })?
        .map_err(ResidentToolError::Invocation)?;
    Ok(reply)
}

pub(crate) struct IssuedWorkbenchCall {
    control: Arc<WorkbenchExecutionControl>,
    publication: Option<HostedCellPublication>,
}

impl ResidentToolClient {
    pub(crate) async fn expand_display(
        &self,
        identity: (i64, i64, i64),
        key: i64,
    ) -> Result<serde_json::Value, ResidentToolError> {
        let reply = expand_display_response(&self.actor, identity, key).await?;
        serde_json::to_value(reply).map_err(ResidentToolError::Encoding)
    }

    pub(crate) async fn seal(&self) -> Result<crate::HostedWorkSeal, ResidentToolError> {
        self.actor
            .seal_hosted_work()
            .await
            .map_err(ResidentToolError::Invocation)
    }
    pub(crate) fn local(actor: crate::LocalActorRef) -> Self {
        Self { actor }
    }

    pub(crate) fn retained_operation(
        &self,
        invocation: ToolInvocationContext,
    ) -> Result<HostedOperationSettlement, ResidentToolError> {
        self.actor
            .hosted_cell()
            .retained_operation(&invocation.into())
            .map_err(ResidentToolError::Unavailable)?
            .ok_or_else(|| {
                ResidentToolError::Unavailable(
                    "operation has no issued native settlement owner".into(),
                )
            })
    }

    pub(crate) async fn cancel_workbench(
        &self,
        invocation: ToolInvocationContext,
    ) -> Result<WorkbenchCancellationOutcome, ResidentToolError> {
        let execution = execution_id(self.actor.identity(), &invocation.clone().into());
        let retained = self
            .actor
            .hosted_cell()
            .retained_operation(&invocation.clone().into())
            .map_err(ResidentToolError::Unavailable)?;
        if let Some(retained) = &retained {
            if let HostedOperationTerminal::Settled(outcome) = retained.terminal() {
                return Ok(outcome);
            }
        }
        let control = retained
            .and_then(|retained| retained.control())
            .or_else(|| {
                self.actor.hosted_cell().find(|control| {
                    control
                        .invocation
                        .as_ref()
                        .is_some_and(|key| execution_id(self.actor.identity(), key) == execution)
                })
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
            && !control.cell_finished()
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
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> Result<WorkbenchBoundaryReconciliation, ResidentToolError> {
        if boundary.hosted().is_some() {
            let owner = self
                .actor
                .hosted_cell()
                .retained_boundary(&boundary)
                .map_err(ResidentToolError::Unavailable)?;
            return match owner.terminal() {
                HostedOperationTerminal::Pending
                | HostedOperationTerminal::Settled(
                    WorkbenchCancellationOutcome::Unconfirmed { .. }
                    | WorkbenchCancellationOutcome::UnknownEvaluation { .. },
                ) => Ok(WorkbenchBoundaryReconciliation::Pending),
                HostedOperationTerminal::Settled(_) => match owner.finalization() {
                    HostedOperationFinalization::Settled(Ok(_)) => {
                        Ok(WorkbenchBoundaryReconciliation::Settled)
                    }
                    HostedOperationFinalization::Settled(Err(error)) => {
                        Err(ResidentToolError::Unavailable(error))
                    }
                    HostedOperationFinalization::Pending => owner
                        .reply()
                        .map(|reply| WorkbenchBoundaryReconciliation::Recovered { reply })
                        .ok_or_else(|| {
                            ResidentToolError::Unavailable(
                                "native terminal lost its exact original reply".into(),
                            )
                        }),
                },
            };
        }
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

    pub(crate) async fn abort(
        &self,
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> Result<serde_json::Value, ResidentToolError> {
        self.finalize_provider_operation(boundary, false).await
    }

    pub(crate) async fn complete(
        &self,
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> Result<serde_json::Value, ResidentToolError> {
        self.finalize_provider_operation(boundary, true).await
    }

    // Provider callbacks acknowledge one logical operation admitted by the
    // transport's durable claim and scheduler. Direct native physical retries
    // earn no separate acknowledgement; exact terminal replay names this same
    // owner. An arbitrary unissued operation must still refuse.
    async fn finalize_provider_operation(
        &self,
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
        completed: bool,
    ) -> Result<serde_json::Value, ResidentToolError> {
        let retained = if boundary.hosted().is_some() {
            Some(
                self.actor
                    .hosted_cell()
                    .retained_boundary(&boundary)
                    .map_err(ResidentToolError::Unavailable)?,
            )
        } else {
            None
        };
        if retained.as_ref().is_none_or(|owner| {
            matches!(owner.finalization(), HostedOperationFinalization::Pending)
        }) {
            let (reply, receive) = oneshot::channel();
            let message = if completed {
                crate::KernelMessage::ToolCompleted {
                    boundary,
                    reply: reply.into(),
                }
            } else {
                crate::KernelMessage::ToolAborted {
                    boundary,
                    reply: reply.into(),
                }
            };
            let result = match self.actor.address().send_message(message) {
                Ok(()) => {
                    if let Some(owner) = &retained {
                        // Retirement may confirm cleanup while the queued RPC
                        // can no longer run. Observe the owning proof directly;
                        // waiting for mailbox destruction can block host release.
                        tokio::select! {
                            acknowledgement = owner.acknowledge() => {
                                acknowledgement?;
                                return Ok(serde_json::Value::Null);
                            }
                            reply = receive => reply.map_err(|_| ResidentToolError::Unavailable(
                                "actor stopped before provider finalization reply".into(),
                            )).and_then(|reply| reply.map_err(ResidentToolError::Invocation)),
                        }
                    } else {
                        receive
                            .await
                            .map_err(|_| {
                                ResidentToolError::Unavailable(
                                    "actor stopped before provider finalization reply".into(),
                                )
                            })
                            .and_then(|reply| reply.map_err(ResidentToolError::Invocation))
                    }
                }
                Err(_) => Err(ResidentToolError::Unavailable(
                    "the owning actor has stopped".into(),
                )),
            };
            if retained.is_none() {
                return result;
            }
            // A lost/closed mailbox is not proof. Retirement or the ordinary
            // actor finalizer must settle the exact retained owner instead.
        }
        retained
            .expect("hosted finalizer retains its issued owner")
            .acknowledge()
            .await?;
        // Cleanup acknowledgement does not publish native context. CellExit and
        // provider ContextDisposition remain authoritative after retirement.
        Ok(serde_json::Value::Null)
    }

    #[cfg(test)]
    pub(crate) async fn dispatch_workbench(
        &self,
        request: WorkbenchRequest,
        invocation: Option<ToolInvocationContext>,
    ) -> Result<serde_json::Value, ResidentToolError> {
        self.dispatch_workbench_issued_with_context(request, invocation, None, None, None, None)
            .await
            .and_then(|response| {
                serde_json::to_value(response).map_err(ResidentToolError::Encoding)
            })
    }

    pub(crate) fn issue_workbench_call(
        &self,
        invocation: Option<ToolInvocationContext>,
    ) -> IssuedWorkbenchCall {
        let control = WorkbenchExecutionControl::from_invocation(invocation);
        let publication = control
            .invocation
            .as_ref()
            .map(|_| HostedCellPublication::publish(&self.actor, &control));
        IssuedWorkbenchCall {
            control,
            publication,
        }
    }

    pub(crate) fn reject_workbench_call(
        &self,
        issued: &IssuedWorkbenchCall,
        error: &ResidentToolError,
    ) {
        if let Some(publication) = &issued.publication {
            publication.accept();
        }
        issued
            .control
            .settle_not_admitted(Err(crate::KernelInvocationFailure::Rejected {
                actor: self.actor.identity(),
                receipts: Vec::new(),
                detail: error.to_string(),
                diagnostic: None,
            }));
        self.actor.hosted_cell().complete(&issued.control);
    }

    pub(crate) async fn dispatch_workbench_issued_with_context(
        &self,
        request: WorkbenchRequest,
        invocation: Option<ToolInvocationContext>,
        installed_tools: Option<crate::InstalledToolLease>,
        hosted_checkpoint_capture: Option<Arc<dyn HostedCheckpointCapture>>,
        context_binding: Option<Arc<dyn crate::HostedContextBinding>>,
        selected_tool: Option<HostedTool>,
    ) -> Result<WorkbenchResponse, ResidentToolError> {
        let issued = self.issue_workbench_call(invocation);
        self.dispatch_prepared_workbench(
            request,
            issued,
            installed_tools,
            hosted_checkpoint_capture,
            context_binding,
            selected_tool,
        )
        .await
    }

    pub(crate) async fn dispatch_prepared_workbench(
        &self,
        mut request: WorkbenchRequest,
        issued: IssuedWorkbenchCall,
        installed_tools: Option<crate::InstalledToolLease>,
        hosted_checkpoint_capture: Option<Arc<dyn HostedCheckpointCapture>>,
        context_binding: Option<Arc<dyn crate::HostedContextBinding>>,
        selected_tool: Option<HostedTool>,
    ) -> Result<WorkbenchResponse, ResidentToolError> {
        if let Some(operation) = issued
            .control
            .invocation
            .as_ref()
            .filter(|key| key.invocation().model_operation().is_some())
        {
            if issued.control.provider_owner.get().is_none() {
                let original = self
                    .actor
                    .hosted_cell()
                    .retained_operation(operation)
                    .map_err(ResidentToolError::Unavailable)?;
                if original
                    .as_ref()
                    .is_none_or(|owner| !owner.native_admitted())
                {
                    let error = ResidentToolError::InvalidInvocation(
                        "the original operation was not admitted; a physical retry cannot acquire its authority".into(),
                    );
                    self.reject_workbench_call(&issued, &error);
                    return Err(error);
                }
            }
        }
        if let Some(operation) = &issued.control.invocation {
            if let Some(original) = operation.invocation().model_operation() {
                request = request.with_checkpoint_boundary(
                    tidepool_runtime::session::ContextCheckpointBoundary::Hosted(original.clone()),
                );
            }
            let execution = issued.control.execution_id(self.actor.identity());
            request = request.with_execution_id(execution.clone());
            tracing::info!(actor = %self.actor.identity(), execution = %execution,
                call_id = %operation.0.call_id,
                context_call_id = operation.0.model_operation().map(|original| original.call_id.as_str()).unwrap_or(""),
                turn_id = %operation.0.request_id(), items = request.items.len(), "workbench cell dispatched to its actor");
        } else if hosted_checkpoint_capture.is_some() || context_binding.is_some() {
            let error = ResidentToolError::Unavailable(
                "hosted authority requires an exact provider invocation".into(),
            );
            self.reject_workbench_call(&issued, &error);
            return Err(error);
        }
        self.dispatch_registered_workbench(
            request,
            issued.control.clone(),
            issued.publication.as_ref(),
            installed_tools,
            hosted_checkpoint_capture,
            context_binding,
            selected_tool,
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
        context_binding: Option<Arc<dyn crate::HostedContextBinding>>,
        selected_tool: Option<HostedTool>,
    ) -> Result<WorkbenchResponse, ResidentToolError> {
        let (response, receive) = oneshot::channel();
        if let Err(error) = self
            .actor
            .admit_mailbox(crate::KernelMessage::Workbench {
                invocation: crate::ActorWorkbenchInvocation::issued(
                    request,
                    installed_tools,
                    hosted_checkpoint_capture,
                )
                .with_context(context_binding, selected_tool),
                control: Some(Arc::clone(&control)),
                reply: response.into(),
            })
            .map_err(|failure| hosted_admission_failure(self.actor.identity(), failure))
        {
            control.settle_not_admitted(Err(error.clone()));
            if let Some(publication) = publication {
                publication.accept();
            }
            self.actor.hosted_cell().complete(&control);
            return Err(ResidentToolError::Invocation(error));
        }
        if let Some(publication) = publication {
            publication.accept();
        }
        let reply = match receive.await {
            Ok(reply) => reply,
            Err(_) => {
                control.mark_unconfirmed();
                let reply = control.settle_reply(Err(crate::KernelInvocationFailure::ActorExited(
                    self.actor.identity(),
                )));
                if matches!(&reply, Err(crate::KernelInvocationFailure::ActorExited(_))) {
                    return Err(ResidentToolError::Unavailable(
                        "the actor stopped before settling the workbench invocation".into(),
                    ));
                }
                reply
            }
        };
        let response = reply.map_err(ResidentToolError::Invocation)?;
        Ok(response)
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

    fn expand_display_boxed(&self, identity: (i64, i64, i64), key: i64) -> ResidentToolFuture {
        let client = self.client.clone();
        Box::pin(async move { client.expand_display(identity, key).await })
    }

    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolDispatchFuture {
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
            client
                .dispatch(invocation)
                .await
                .map(ResidentToolResponse::Value)
        })
    }

    fn dispatch_with_checkpoint_boxed(
        &self,
        invocation: ToolInvocation,
        capture: Option<Arc<dyn HostedCheckpointCapture>>,
    ) -> ResidentToolDispatchFuture {
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
            let value = match capture {
                Some(capture) => {
                    client
                        .dispatch_with_checkpoint_capture(invocation, capture)
                        .await
                }
                None => client.dispatch(invocation).await,
            }?;
            Ok(ResidentToolResponse::Value(value))
        })
    }
}

pub(crate) fn install_local_resident_tools(
    actor: crate::LocalActorRef,
    awaiting: &ResidentToolAwait,
) -> Result<ResidentToolPolicy, exomonad_tool::ToolDeclarationError> {
    let tools = project_local_resident_tools(&awaiting.declarations)?;
    Ok(ResidentToolPolicy {
        tools: tools.into(),
        instructions: (!awaiting.synopsis.is_empty()).then(|| awaiting.synopsis.clone()),
        client: ResidentToolClient::local(actor),
    })
}

fn project_local_resident_tools(
    declarations: &[exomonad_tool::ToolDeclaration],
) -> Result<Vec<HostedTool>, exomonad_tool::ToolDeclarationError> {
    declarations
        .iter()
        .cloned()
        .map(HostedTool::try_from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_runtime::session::{WorkbenchResponse, WorkbenchRunStatus};

    fn declaration(
        name: &str,
        kind: exomonad_tool::ToolKind,
        input_schema: serde_json::Value,
    ) -> exomonad_tool::ToolDeclaration {
        exomonad_tool::ToolDeclaration {
            schedule: Default::default(),
            implementation: Default::default(),
            effect_keys: Vec::new(),
            name: name.into(),
            description: format!("{name} tool"),
            input_schema,
            output_schema: None,
            kind,
        }
    }

    #[test]
    fn local_resident_install_rejects_non_object_functions_and_keeps_valid_inputs() {
        let invalid = declaration(
            "ping",
            exomonad_tool::ToolKind::Call,
            serde_json::json!({"type": "null"}),
        );
        assert!(matches!(
            project_local_resident_tools(&[invalid]),
            Err(exomonad_tool::ToolDeclarationError::FunctionInputMustBeObject { name })
                if name == "ping"
        ));
        assert!(project_local_resident_tools(&[declaration(
            "empty",
            exomonad_tool::ToolKind::Call,
            serde_json::json!({}),
        )])
        .is_err());

        let valid = declaration(
            "lookup",
            exomonad_tool::ToolKind::Call,
            serde_json::json!({"type": "object", "properties": {}}),
        );
        let raw = declaration(
            "bash",
            exomonad_tool::ToolKind::Raw,
            serde_json::json!({"type": "string"}),
        );
        let tools = project_local_resident_tools(&[valid, raw]).unwrap();
        assert!(matches!(tools[0], HostedTool::Function(_)));
        assert!(matches!(tools[1], HostedTool::Custom(_)));
        assert!(tools[0].accepts(&exomonad_tool::ToolArguments::Structured(
            serde_json::json!({})
        )));
        assert!(tools[1].accepts(&exomonad_tool::ToolArguments::Raw("pwd".into())));
    }

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

        fn dispatch_boxed(&self, _invocation: ToolInvocation) -> ResidentToolDispatchFuture {
            self.dispatches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Ok(ResidentToolResponse::Value(serde_json::Value::Null)) })
        }
    }

    struct TestCheckpointCapture;

    impl HostedCheckpointCapture for TestCheckpointCapture {
        fn capture(
            &self,
            _name: &str,
            _boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
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
            Ok(ResidentToolResponse::Value(serde_json::Value::Null))
        ));
        assert_eq!(dispatches.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    fn call_key(call_id: &str) -> WorkbenchCallKey {
        ToolInvocationContext::external(
            "thread".into(),
            "turn".into(),
            call_id.into(),
            Some("outer-call".into()),
            Some("actor".into()),
        )
        .into()
    }

    #[test]
    fn execution_identity_is_exact_to_actor_and_hosted_call() {
        let actor = crate::ActorRef::first(crate::ActorId(7));
        let original = execution_id(actor, &call_key("call-1"));
        assert_eq!(original, execution_id(actor, &call_key("call-1")));
        assert_ne!(original, execution_id(actor, &call_key("call-2")));
        let mut different_request = call_key("call-1");
        let exomonad_tool::ToolInvocationOrigin::Model(operation) = &mut different_request.0.origin
        else {
            panic!("test context must carry the original operation");
        };
        operation.request_id = "later-turn".into();
        assert_ne!(original, execution_id(actor, &different_request));
        let mut different_context = call_key("call-1");
        let exomonad_tool::ToolInvocationOrigin::Model(operation) = &mut different_context.0.origin
        else {
            panic!("test context must carry the original operation");
        };
        operation.call_id = "different-outer-call".into();
        assert_ne!(original, execution_id(actor, &different_context));
        different_context.0.origin = exomonad_tool::ToolInvocationOrigin::Direct {
            origin: exomonad_tool::ConversationOrigin::External {
                thread_id: "thread".into(),
            },
            request_id: "turn".into(),
        };
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

    #[test]
    fn local_invocations_share_only_the_exact_original_operation_boundary() {
        use exomonad_tool::{ConversationOrigin, OriginalOperation, ToolInvocationOrigin};
        use tidepool_runtime::session::ContextCheckpointBoundary;

        let original = OriginalOperation {
            origin: ConversationOrigin::Embedded {
                run: "run".into(),
                actor: "root".into(),
                incarnation: "first".into(),
            },
            request_id: "request-1".into(),
            call_id: "provider-call".into(),
        };
        let first = WorkbenchCallKey::from(ToolInvocationContext {
            origin: ToolInvocationOrigin::Model(original.clone()),
            call_id: "nested-1".into(),
            namespace: None,
        });
        let second = WorkbenchCallKey::from(ToolInvocationContext {
            call_id: "nested-2".into(),
            ..first.0.clone()
        });
        let boundary = ContextCheckpointBoundary::Hosted(original.clone());
        let owner = WorkbenchCallKey::original(&original);
        assert!(owner.is_original_invocation());
        assert!(owner.matches_boundary(&boundary));
        assert!(!first.is_original_invocation());
        assert!(!second.is_original_invocation());
        assert_ne!(owner, first);
        assert_ne!(owner, second);
        assert!(first.matches_boundary(&boundary));
        assert!(second.matches_boundary(&boundary));
        let actor = crate::ActorRef::first(crate::ActorId(7));
        assert_ne!(execution_id(actor, &first), execution_id(actor, &second));

        let mut reused = original.clone();
        reused.request_id = "request-2".into();
        assert!(!first.matches_boundary(&ContextCheckpointBoundary::Hosted(reused)));
        let mut successor = original;
        let ConversationOrigin::Embedded { incarnation, .. } = &mut successor.origin else {
            unreachable!();
        };
        *incarnation = "second".into();
        assert!(!first.matches_boundary(&ContextCheckpointBoundary::Hosted(successor)));
    }

    fn terminal_reply() -> crate::KernelWorkbenchReply {
        Ok(WorkbenchResponse {
            status: WorkbenchRunStatus::Rejected,
            summary: None,
            items: Vec::new(),
            next_index: 0,
            total: 1,
            publication: None,
        })
    }

    #[test]
    fn direct_controls_retain_distinct_reservation_owners() {
        let actor = crate::ActorRef::first(crate::ActorId(9));
        let first = WorkbenchExecutionControl::untracked();
        let sibling = WorkbenchExecutionControl::untracked();
        let original = first
            .reservation_owner(actor)
            .expect("direct execution owner");
        assert_eq!(first.reservation_owner(actor), Some(original.clone()));
        assert_ne!(sibling.reservation_owner(actor), Some(original));
        first.request_cancellation();
        assert!(!sibling.cancellation_requested());
    }

    struct ContextCancellation(std::sync::atomic::AtomicUsize);

    impl crate::HostedContextBinding for ContextCancellation {
        fn admit(
            &self,
            _: &WorkbenchExecutionId,
            _: &ToolInvocationContext,
            _: tidepool_repr::PrincipalId,
        ) -> Result<(), tidepool_effect::error::EffectError> {
            Ok(())
        }
        fn prepare(
            &self,
            _: crate::ContextReq,
            _: tidepool_repr::PrincipalId,
            _: tidepool_repr::DataConTable,
        ) -> tidepool_effect::DeferredEffect {
            panic!("cancellation test does not evaluate context effects")
        }
        fn cancel(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        fn finish(&self, _: crate::CellExit) {}
    }

    #[test]
    fn cancellation_revokes_context_after_native_publication_and_before_binding() {
        let control = WorkbenchExecutionControl::untracked();
        assert!(control.publication_decision().claim_commit().is_some());
        assert!(!control.request_cancellation());
        assert!(!control.cancellation_requested());
        assert!(control.context_cancellation_requested());
        let binding = Arc::new(ContextCancellation(std::sync::atomic::AtomicUsize::new(0)));
        control.bind_context(binding.clone());
        assert_eq!(binding.0.load(std::sync::atomic::Ordering::Acquire), 1);
        control.request_cancellation();
        assert_eq!(binding.0.load(std::sync::atomic::Ordering::Acquire), 2);
    }

    struct FinishingContext {
        cancelled: std::sync::atomic::AtomicUsize,
        exit: parking_lot::Mutex<Option<crate::CellExit>>,
        entered: std::sync::Barrier,
        release: std::sync::Barrier,
    }

    impl crate::HostedContextBinding for FinishingContext {
        fn admit(
            &self,
            _: &WorkbenchExecutionId,
            _: &ToolInvocationContext,
            _: tidepool_repr::PrincipalId,
        ) -> Result<(), tidepool_effect::error::EffectError> {
            Ok(())
        }
        fn prepare(
            &self,
            _: crate::ContextReq,
            _: tidepool_repr::PrincipalId,
            _: tidepool_repr::DataConTable,
        ) -> tidepool_effect::DeferredEffect {
            panic!("cutoff test does not evaluate effects")
        }
        fn cancel(&self) {
            self.cancelled
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        fn finish(&self, exit: crate::CellExit) {
            *self.exit.lock() = Some(exit);
            self.entered.wait();
            self.release.wait();
        }
    }

    #[test]
    fn whole_cell_cutoff_prevents_cancellation_during_binding_finish_before_reply_delivery() {
        let control = WorkbenchExecutionControl::untracked();
        let binding = Arc::new(FinishingContext {
            cancelled: std::sync::atomic::AtomicUsize::new(0),
            exit: parking_lot::Mutex::new(None),
            entered: std::sync::Barrier::new(2),
            release: std::sync::Barrier::new(2),
        });
        control.bind_context(binding.clone());
        let execution = WorkbenchExecutionId::from_digest([24; 16]);
        let response = WorkbenchResponse {
            publication: None,
            status: WorkbenchRunStatus::Committed,
            summary: None,
            items: Vec::new(),
            next_index: 1,
            total: 1,
        };
        let finishing = {
            let control = control.clone();
            let binding = binding.clone();
            let execution = execution.clone();
            let response = response.clone();
            std::thread::spawn(move || {
                let result = Ok(crate::KernelStep::Continue(response));
                let exit = control.finish_cell(execution, &result, true);
                crate::HostedContextBinding::finish(binding.as_ref(), exit.clone());
                exit
            })
        };
        binding.entered.wait();
        assert!(
            control.terminal_reply().is_none(),
            "LocalActor has not delivered its reply"
        );
        assert!(!control.request_cancellation());
        assert!(!control.context_cancellation_requested());
        assert!(!control.cancellation_requested());
        assert_eq!(
            binding.cancelled.load(std::sync::atomic::Ordering::Acquire),
            0
        );
        let retained = binding.exit.lock().clone().unwrap();
        assert!(retained.permits_context_commit());
        binding.release.wait();
        assert_eq!(finishing.join().unwrap(), retained);
        control.settle(Ok(response.clone()));
        assert!(matches!(
            control.cancellation_outcome(execution, Ok(response.clone())),
            WorkbenchCancellationOutcome::PublicationSettled { reply, .. } if reply == Ok(response)
        ));
    }

    #[test]
    fn losing_cancellation_revokes_context_draft_without_reclassifying_published_cell() {
        let control = WorkbenchExecutionControl::untracked();
        let binding = Arc::new(ContextCancellation(std::sync::atomic::AtomicUsize::new(0)));
        control.bind_context(binding.clone());
        control
            .publication_decision()
            .claim_commit()
            .unwrap()
            .published();
        assert!(
            !control.request_cancellation(),
            "native publication is already complete"
        );
        let execution = WorkbenchExecutionId::from_digest([25; 16]);
        let response = WorkbenchResponse {
            publication: None,
            status: WorkbenchRunStatus::Committed,
            summary: None,
            items: Vec::new(),
            next_index: 1,
            total: 1,
        };
        let result = Ok(crate::KernelStep::Continue(response.clone()));
        let exit = control.finish_cell(execution.clone(), &result, true);
        assert_eq!(exit.cause, crate::CellExitCause::FullReturn);
        assert!(exit.permits_context_commit());
        control.settle(Ok(response.clone()));
        assert!(!control.request_cancellation());
        assert_eq!(binding.0.load(std::sync::atomic::Ordering::Acquire), 1);
        assert!(matches!(
            control.cancellation_outcome(execution.clone(), Ok(response.clone())),
            WorkbenchCancellationOutcome::PublicationSettled { reply, .. } if reply == Ok(response)
        ));
        assert_eq!(
            control.finish_cell(execution, &result, false),
            exit,
            "terminal evidence is immutable"
        );
    }

    #[test]
    fn pending_compiler_close_blocks_context_commit_without_rewriting_completed_action() {
        let control = WorkbenchExecutionControl::untracked();
        let receipt = crate::termination::CompilerWorkReceipt::pending();
        assert!(control.register_compiler_work(receipt.clone()));
        let ticket = crate::termination::CompilerWorkTicket::new(
            receipt,
            crate::resident_workbench::CompilerCloseOwner::Hosted(control.clone()),
        );
        let response = Ok(crate::KernelStep::Continue(WorkbenchResponse {
            publication: None,
            status: WorkbenchRunStatus::Completed,
            summary: None,
            items: Vec::new(),
            next_index: 1,
            total: 1,
        }));
        let execution = WorkbenchExecutionId::from_digest([31; 16]);
        let exit = control.finish_cell(execution.clone(), &response, true);
        assert_eq!(exit.cause, crate::CellExitCause::FullReturn);
        assert!(!exit.cleanup_confirmed);
        assert!(!exit.permits_context_commit());
        ticket.consume(tidepool_runtime::CompilerTransactionOutcome {
            action: (),
            close: tidepool_runtime::CompilerTransactionClose::Clean,
        });
        assert_eq!(
            control.finish_cell(execution, &response, true),
            exit,
            "late close cannot mutate an already published cell cutoff"
        );
        assert!(!control.register_compiler_work(crate::termination::CompilerWorkReceipt::pending()));
    }

    #[test]
    fn winning_cancellation_remains_the_cell_exit_after_native_acknowledgement() {
        let control = WorkbenchExecutionControl::untracked();
        assert!(control.request_cancellation());
        assert!(control.publication_decision().claim_commit().is_none());
        control.acknowledge_cancellation();
        let response = WorkbenchResponse {
            publication: Some(
                tidepool_runtime::session::WorkbenchPublicationOutcome::NotPublished {
                    reason: tidepool_runtime::session::WorkbenchNotPublishedReason::Cancelled,
                },
            ),
            status: WorkbenchRunStatus::Committed,
            summary: None,
            items: Vec::new(),
            next_index: 1,
            total: 1,
        };
        let exit = control.finish_cell(
            WorkbenchExecutionId::from_digest([29; 16]),
            &Ok(crate::KernelStep::Continue(response)),
            true,
        );
        assert_eq!(exit.cause, crate::CellExitCause::Cancelled);
        assert!(!exit.permits_context_commit());
    }

    #[test]
    fn computing_cancellation_preserves_sibling_native_flag() {
        let first = WorkbenchExecutionControl::untracked();
        let sibling = WorkbenchExecutionControl::untracked();
        assert!(first.request_cancellation());
        assert!(first
            .native_cancel()
            .load(std::sync::atomic::Ordering::Acquire));
        assert!(!sibling
            .native_cancel()
            .load(std::sync::atomic::Ordering::Acquire));
        first.arm_sleep();
        assert!(
            first.cancellation_requested(),
            "cancel remains sticky at next effect"
        );
        assert!(!first.request_cancellation(), "one cancellation owner wins");
        let decision = sibling.publication_decision();
        assert!(!decision.claim_commit().unwrap().published());
        assert_eq!(decision.phase(), PublicationPhase::Published);
    }

    #[test]
    fn human_interaction_commit_and_cancellation_share_original_cutoff() {
        let control = WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        let entered = Arc::new(std::sync::Barrier::new(2));
        let finish = Arc::new(std::sync::Barrier::new(2));
        let committing = {
            let control = control.clone();
            let entered = entered.clone();
            let finish = finish.clone();
            std::thread::spawn(move || {
                control.admit_interaction(|| {
                    entered.wait();
                    finish.wait();
                    "durably answered"
                })
            })
        };
        entered.wait();
        let cancelling = {
            let control = control.clone();
            std::thread::spawn(move || control.request_cancellation())
        };
        finish.wait();
        assert_eq!(committing.join().unwrap(), Some("durably answered"));
        assert!(cancelling.join().unwrap());
        assert!(control
            .admit_interaction(|| panic!("cancelled owner must not commit a later answer"))
            .is_none());
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
    async fn terminal_cleanup_evidence_preserves_the_first_reply_after_native_acknowledgement() {
        for cleanup_failed_first in [false, true] {
            let control = WorkbenchExecutionControl::untracked();
            control.arm_sleep();
            assert!(control.request_cancellation());
            control.acknowledge_cancellation();
            let failure = Err(crate::KernelInvocationFailure::CleanupUnconfirmed {
                publication: None,
                receipts: Vec::new(),
                actor: crate::ActorRef::first(crate::ActorId(1)),
                detail: "model invocation terminal receipt unavailable".into(),
            });
            if cleanup_failed_first {
                control.settle(failure.clone());
                control.settle(terminal_reply());
                assert_eq!(control.settled().await, failure);
                assert!(matches!(
                    control.cancellation_outcome(
                        WorkbenchExecutionId::from_digest([6; 16]),
                        terminal_reply(),
                    ),
                    WorkbenchCancellationOutcome::Unconfirmed { .. }
                ));
            } else {
                control.settle(terminal_reply());
                control.settle(failure.clone());
                assert_eq!(control.settled().await, terminal_reply());
                assert!(matches!(
                    control
                        .cancellation_outcome(WorkbenchExecutionId::from_digest([6; 16]), failure,),
                    WorkbenchCancellationOutcome::Cancelled { reply: Ok(_), .. }
                ));
            }
        }
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
            receipts: Vec::new(),
            actor: crate::ActorRef::first(crate::ActorId(1)),
            detail: "publication failed before visibility".into(),
            diagnostic: None,
        }));
        assert!(matches!(
            control.cancellation_outcome(execution, control.settled().await),
            WorkbenchCancellationOutcome::PublicationSettled { reply: Err(_), .. }
        ));
    }

    #[test]
    fn losing_cancellation_preserves_the_failed_publication_cell_exit() {
        let control = WorkbenchExecutionControl::untracked();
        let claim = control.publication_decision().claim_commit().unwrap();
        assert_eq!(
            control.admit_cancellation().1,
            Some(PublicationCancellation::PendingCommitOutcome)
        );
        assert!(claim.before_rename_failure());
        let publication = tidepool_runtime::session::WorkbenchPublicationOutcome::Rejected {
            detail: "rename failed".into(),
        };
        let failure = crate::KernelInvocationFailure::Workbench(crate::KernelWorkbenchFailure {
            actor: crate::ActorRef::first(crate::ActorId(1)),
            receipts: Vec::new(),
            point: tidepool_runtime::session::WorkbenchFailurePoint::Publication {
                completed_input_units: 1,
            },
            total: 1,
            publication: Some(publication),
            detail: "rename failed".into(),
            diagnostic: None,
        });
        let result = Err(failure.clone());
        let execution = WorkbenchExecutionId::from_digest([28; 16]);
        let exit = control.finish_cell(execution.clone(), &result, true);
        assert_eq!(exit.cause, crate::CellExitCause::Failed);
        assert!(control.context_cancellation_requested());
        control.settle(Err(failure.clone()));
        assert!(
            matches!(control.cancellation_outcome(execution, Err(failure.clone())),
            WorkbenchCancellationOutcome::PublicationSettled { reply: Err(retained), .. } if retained == failure)
        );
    }

    #[test]
    fn lost_publication_claim_stays_unconfirmed_after_terminal_transport_failure() {
        let control = WorkbenchExecutionControl::untracked();
        let claim = control.publication_decision().claim_commit().unwrap();
        assert_eq!(
            control.admit_cancellation().1,
            Some(PublicationCancellation::PendingCommitOutcome)
        );
        drop(claim);
        control.mark_unconfirmed();
        let actor = crate::ActorRef::first(crate::ActorId(1));
        let reply = Err(crate::KernelInvocationFailure::ActorExited(actor));
        control.settle(reply.clone());
        assert!(matches!(
            control.cancellation_outcome(WorkbenchExecutionId::from_digest([9; 16]), reply),
            WorkbenchCancellationOutcome::Unconfirmed { .. }
        ));
    }

    #[test]
    fn later_wait_does_not_reuse_an_earlier_expiry_as_abort_evidence() {
        let control = WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        assert!(control.claim_expiry());
        control.finish_sleep();
        control.arm_sleep();
        assert!(control.request_cancellation());
        assert!(matches!(
            control.cancellation_outcome(
                WorkbenchExecutionId::from_digest([10; 16]),
                terminal_reply(),
            ),
            WorkbenchCancellationOutcome::Unconfirmed { .. }
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
        control.finish_sleep();
        assert!(control.request_cancellation());
        assert_eq!(
            control.phase.load(std::sync::atomic::Ordering::Acquire),
            WORKBENCH_CANCEL_REQUESTED
        );
    }

    #[test]
    fn only_an_unsettled_unnamespaced_cell_outside_sleep_is_computing() {
        let mut key = call_key("call-1");
        key.0.namespace = None;
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
    async fn sealed_cell_cancellation_waits_for_exact_owner_reply_without_mutating_cutoff() {
        let (send, mut received) = tokio::sync::mpsc::unbounded_channel();
        let (address, actor_task) = ractor::Actor::spawn(None, WorkbenchCollector, send)
            .await
            .unwrap();
        let actor = crate::LocalActorRef::new(address.clone(), crate::RetainedActorExit::new());
        let client = ResidentToolClient::local(actor.clone());
        let invocation = ToolInvocationContext::external(
            "thread".into(),
            "turn".into(),
            "call".into(),
            Some("call".into()),
            None,
        );
        let running = {
            let client = client.clone();
            let invocation = invocation.clone();
            tokio::spawn(async move {
                client
                    .dispatch_workbench(
                        WorkbenchRequest::from_cell_input("pure True"),
                        Some(invocation),
                    )
                    .await
            })
        };
        let (control, reply) = received.recv().await.unwrap();
        let control = control.unwrap();
        let execution = control.execution_id(actor.identity());
        let response = WorkbenchResponse {
            publication: None,
            status: WorkbenchRunStatus::Committed,
            summary: None,
            items: Vec::new(),
            next_index: 1,
            total: 1,
        };
        let result = Ok(crate::KernelStep::Continue(response.clone()));
        assert!(control
            .finish_cell(execution, &result, true)
            .permits_context_commit());
        assert!(control.terminal_reply().is_none());
        let cancelled = client.cancel_workbench(invocation);
        tokio::pin!(cancelled);
        assert!(futures_util::poll!(&mut cancelled).is_pending());
        assert!(!control.context_cancellation_requested());
        control.settle(Ok(response.clone()));
        assert!(matches!(cancelled.await.unwrap(),
            WorkbenchCancellationOutcome::PublicationSettled { reply, .. } if reply == Ok(response.clone())));
        actor.hosted_cell().complete(&control);
        reply.send(Ok(response)).unwrap();
        running.await.unwrap().unwrap();
        address.stop(None);
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn a_dispatched_hosted_cell_is_visible_on_its_actor_until_it_ends() {
        let (send, mut received) = tokio::sync::mpsc::unbounded_channel();
        let (address, task) = ractor::Actor::spawn(None, WorkbenchCollector, send)
            .await
            .unwrap();
        let actor = crate::LocalActorRef::new(address.clone(), crate::RetainedActorExit::new());
        let client = ResidentToolClient::local(actor.clone());
        let invocation = ToolInvocationContext::external(
            "thread".into(),
            "turn".into(),
            "call-1".into(),
            Some("call-1".into()),
            None,
        );
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
        assert!(actor.hosted_workbench_waiting(&invocation).is_none());
        let mut second_invocation = invocation.clone();
        second_invocation.call_id = "second-call".into();
        let second = dispatch(client.clone(), second_invocation.clone());
        let (second_control, second_reply) =
            tokio::time::timeout(std::time::Duration::from_secs(2), received.recv())
                .await
                .expect("second call reaches actor while first is active")
                .unwrap();
        let second_control = second_control.unwrap();
        second.abort();
        let _ = second.await;
        assert!(
            actor.hosted_cell_computing(),
            "abandoned caller retains accepted execution"
        );
        second_control.settle(terminal_reply());
        actor.hosted_cell().complete(&second_control);
        drop(second_reply);
        assert!(
            actor.hosted_cell_computing(),
            "settling second execution preserves first"
        );
        control.arm_sleep();
        assert_eq!(
            actor.hosted_workbench_waiting(&invocation),
            Some(control.execution_id(actor.identity()))
        );
        let mut wrong_invocation = invocation.clone();
        wrong_invocation.origin = ToolInvocationContext::external(
            "other-thread".into(),
            "turn".into(),
            "call-1".into(),
            Some("call-1".into()),
            None,
        )
        .origin;
        assert!(actor.hosted_workbench_waiting(&wrong_invocation).is_none());
        assert!(actor.hosted_workbench_waiting(&second_invocation).is_none());
        let mut namespaced = invocation.clone();
        namespaced.namespace = Some("nested".into());
        assert!(actor.hosted_workbench_waiting(&namespaced).is_none());
        assert!(
            !actor.hosted_cell_computing(),
            "a sleeping cell is interruptible"
        );
        assert!(control.request_cancellation());
        assert!(actor.hosted_workbench_waiting(&invocation).is_none());
        control.settle(terminal_reply());
        assert!(actor.hosted_workbench_waiting(&invocation).is_none());
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
        let abandoned = dispatch(client.clone(), invocation.clone());
        let (control, reply) = received.recv().await.unwrap();
        let control = control.unwrap();
        abandoned.abort();
        let _ = abandoned.await;
        assert!(actor.hosted_cell_computing());
        control.arm_sleep();
        assert!(actor.hosted_workbench_waiting(&invocation).is_some());
        control.settle(terminal_reply());
        assert!(actor.hosted_workbench_waiting(&invocation).is_none());
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
        let control = WorkbenchExecutionControl::new(Some(WorkbenchCallKey::from(
            ToolInvocationContext::external(
                "thread".into(),
                "turn".into(),
                "call".into(),
                None,
                None,
            ),
        )));
        slot.publish_transport(Arc::clone(&control));
        slot.claim(&control);
        slot.complete(&control);
        slot.accept(&control);
        assert!(slot.find(|_| true).is_none());
    }
}
