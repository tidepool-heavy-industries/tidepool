//! Local message protocol and exact handles for the Ractor-owned kernel.
//!
//! This protocol is deliberately process-local and non-serializable. JSON is
//! reserved for real external boundaries; live Haskell roots move directly
//! through these messages under Rust ownership.

use ractor::{ActorRef as RactorRef, RpcReplyPort};

pub mod lifecycle;
pub use lifecycle::{
    ActorAdmissionLease, AdmittedKernelEnvelope, LifecycleHandle, NativeProviderAdmission,
    NativeProviderSettlementError, NativeProviderStartError, NativeProviderTurnLease,
    ProviderIdleProbe,
};
use tidepool_runtime::session::{WorkbenchRequest, WorkbenchResponse};

/// Exact identity of one actor-owned execution step. The generation
/// changes for every task in the actor incarnation. The admission generation
/// identifies the execution even for direct notebook submissions; the optional
/// request identity is transport correlation metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbenchStepKey {
    actor: ActorRef,
    generation: u64,
    execution_generation: u64,
    request_execution: Option<tidepool_runtime::session::WorkbenchExecutionId>,
}

impl WorkbenchStepKey {
    pub(crate) fn new(
        actor: ActorRef,
        generation: u64,
        request_execution: Option<tidepool_runtime::session::WorkbenchExecutionId>,
    ) -> Self {
        Self {
            actor,
            generation,
            execution_generation: generation,
            request_execution,
        }
    }

    /// Keep this execution's admission identity while fencing its next task.
    pub(crate) fn next_step(&self, generation: u64) -> Self {
        Self {
            generation,
            ..self.clone()
        }
    }

    /// The first task generation, retained across all steps of this execution.
    #[must_use]
    pub fn execution_generation(&self) -> u64 {
        self.execution_generation
    }

    #[must_use]
    pub fn actor(&self) -> ActorRef {
        self.actor
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn request_execution(&self) -> Option<&tidepool_runtime::session::WorkbenchExecutionId> {
        self.request_execution.as_ref()
    }
}

use crate::{
    ActorRef, ActorTerminal, ExternalApplicationFailure, ExternalFailureDisposition,
    KernelBehaviorError, MailboxValue, RetainedActorExit,
};

/// The exact synchronous-call path currently occupying a chain of actors.
///
/// A callee extends the path before running its handler. Re-entering any actor
/// already in the path is rejected before request ownership changes hands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallAncestry {
    actors: Vec<ActorRef>,
}

impl CallAncestry {
    #[must_use]
    pub fn begin(caller: ActorRef) -> Self {
        Self {
            actors: vec![caller],
        }
    }

    pub fn enter(&self, target: ActorRef) -> Result<Self, KernelCallFailure> {
        if self.actors.contains(&target) {
            return Err(KernelCallFailure::Cycle {
                target,
                ancestry: self.actors.clone(),
            });
        }
        let mut actors = self.actors.clone();
        actors.push(target);
        Ok(Self { actors })
    }

    #[must_use]
    pub fn actors(&self) -> &[ActorRef] {
        &self.actors
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KernelCallFailure {
    #[error("synchronous call to actor {target} would re-enter its own ancestry {ancestry:?}")]
    Cycle {
        target: ActorRef,
        ancestry: Vec<ActorRef>,
    },
    #[error("target actor {0} has exited")]
    TargetExited(ActorRef),
    #[error("target actor {0} is unavailable")]
    TargetUnavailable(ActorRef),
    #[error("public owner installation for target actor {0} is pending")]
    InstallationPending(ActorRef),
    #[error("idle retirement for target actor {0} is being checked")]
    RetirementPending(ActorRef),
    #[error("target actor {0} has closed mailbox admission")]
    MailboxClosed(ActorRef),
    #[error("target actor {actor:?} failed while handling the call: {detail}")]
    Handler {
        actor: ActorRef,
        detail: String,
        diagnostic: Option<tidepool_toolchain::failclass::FailureEnvelope>,
    },
}

pub type KernelCallReply = Result<MailboxValue, KernelCallFailure>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KernelInvocationFailure {
    #[error("actor {0} has exited")]
    ActorExited(ActorRef),
    #[error("actor {actor} invocation was cancelled")]
    Cancelled { actor: ActorRef },
    #[error("actor {actor} rejected the invocation: {detail}")]
    Rejected {
        actor: ActorRef,
        detail: String,
        receipts: Vec<tidepool_runtime::session::WorkbenchItemReceipt>,
        diagnostic: Option<tidepool_toolchain::failclass::FailureEnvelope>,
    },
    #[error("actor {actor} invocation failed: {detail}")]
    Failed {
        actor: ActorRef,
        detail: String,
        receipts: Vec<tidepool_runtime::session::WorkbenchItemReceipt>,
        diagnostic: Option<tidepool_toolchain::failclass::FailureEnvelope>,
    },
    #[error("actor {actor} invocation cleanup remains unconfirmed: {detail}")]
    CleanupUnconfirmed {
        publication: Option<tidepool_runtime::session::WorkbenchPublicationOutcome>,
        actor: ActorRef,
        detail: String,
        receipts: Vec<tidepool_runtime::session::WorkbenchItemReceipt>,
    },
    #[error(
        "actor {actor} accepted request {request:?}, but its terminal transfer failed: {source}"
    )]
    TerminalTransferFailed {
        actor: ActorRef,
        request: crate::RequestId,
        source: Box<KernelInvocationFailure>,
    },
    #[error(transparent)]
    Workbench(#[from] KernelWorkbenchFailure),
}

impl KernelInvocationFailure {
    pub fn failure_diagnostic(&self) -> Option<&tidepool_toolchain::failclass::FailureEnvelope> {
        match self {
            Self::Failed { diagnostic, .. } | Self::Rejected { diagnostic, .. } => {
                diagnostic.as_ref()
            }
            Self::Workbench(failure) => failure.diagnostic.as_ref(),
            Self::TerminalTransferFailed { source, .. } => source.failure_diagnostic(),
            _ => None,
        }
    }

    pub(crate) fn into_behavior_error(self) -> KernelBehaviorError {
        KernelBehaviorError::with_diagnostic(self.to_string(), self.failure_diagnostic().cloned())
    }
    /// Receipts retained by the admitted execution, including terminal cleanup
    /// and transfer failures. The failure class remains independently typed.
    pub fn receipts(&self) -> &[tidepool_runtime::session::WorkbenchItemReceipt] {
        match self {
            Self::Workbench(failure) => &failure.receipts,
            Self::Rejected { receipts, .. }
            | Self::Failed { receipts, .. }
            | Self::CleanupUnconfirmed { receipts, .. } => receipts,
            Self::TerminalTransferFailed { source, .. } => source.receipts(),
            Self::ActorExited(_) | Self::Cancelled { .. } => &[],
        }
    }

    /// Public visibility survives terminal transfer and cleanup failures.
    pub fn publication(&self) -> Option<&tidepool_runtime::session::WorkbenchPublicationOutcome> {
        match self {
            Self::Workbench(failure) => failure.publication.as_ref(),
            Self::CleanupUnconfirmed { publication, .. } => publication.as_ref(),
            Self::TerminalTransferFailed { source, .. } => source.publication(),
            _ => None,
        }
    }

    pub(crate) fn receipts_mut(
        &mut self,
    ) -> Option<&mut Vec<tidepool_runtime::session::WorkbenchItemReceipt>> {
        match self {
            Self::Workbench(failure) => Some(&mut failure.receipts),
            Self::Rejected { receipts, .. }
            | Self::Failed { receipts, .. }
            | Self::CleanupUnconfirmed { receipts, .. } => Some(receipts),
            Self::TerminalTransferFailed { source, .. } => source.receipts_mut(),
            Self::ActorExited(_) | Self::Cancelled { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelWorkbenchFailure {
    pub actor: ActorRef,
    pub receipts: Vec<tidepool_runtime::session::WorkbenchItemReceipt>,
    pub point: tidepool_runtime::session::WorkbenchFailurePoint,
    pub total: usize,
    pub publication: Option<tidepool_runtime::session::WorkbenchPublicationOutcome>,
    pub detail: String,
    pub diagnostic: Option<tidepool_toolchain::failclass::FailureEnvelope>,
}

impl std::fmt::Display for KernelWorkbenchFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.point {
            tidepool_runtime::session::WorkbenchFailurePoint::InputUnit { index } => write!(
                formatter,
                "actor {} workbench input unit {} of {} failed: {}",
                self.actor,
                index + 1,
                self.total,
                self.detail
            )?,
            tidepool_runtime::session::WorkbenchFailurePoint::Publication {
                completed_input_units,
            } => match self.publication.as_ref() {
                Some(
                    tidepool_runtime::session::WorkbenchPublicationOutcome::RecoveryRequired {
                        ..
                    },
                ) => write!(
                    formatter,
                    "actor {} workbench publication requires authenticated recovery after {} completed input units: {}",
                    self.actor, completed_input_units, self.detail
                )?,
                _ => write!(
                    formatter,
                    "actor {} workbench publication rejected after {} completed input units: {}",
                    self.actor, completed_input_units, self.detail
                )?,
            },
            tidepool_runtime::session::WorkbenchFailurePoint::Finalization {
                completed_input_units,
            } => write!(
                formatter,
                "actor {} workbench finalization failed after {} completed input units: {}",
                self.actor, completed_input_units, self.detail
            )?,
        }
        if !self.receipts.is_empty() {
            formatter.write_str("\ninput receipts before failure:")?;
            for receipt in &self.receipts {
                write!(
                    formatter,
                    "\ninput unit {} ({:?}): {}",
                    receipt.index + 1,
                    receipt.status,
                    receipt.output
                )?;
                for operation in &receipt.operations {
                    write!(
                        formatter,
                        "\n  operation {}:{}:{} {:?} ({})",
                        operation.id.execution,
                        operation.id.input_unit_index + 1,
                        operation.id.effect_ordinal + 1,
                        operation.disposition,
                        operation.effect,
                    )?;
                }
            }
        }
        Ok(())
    }
}

impl std::error::Error for KernelWorkbenchFailure {}

pub type KernelInvocationReply = Result<serde_json::Value, KernelInvocationFailure>;
pub type KernelWorkbenchReply = Result<WorkbenchResponse, KernelInvocationFailure>;

/// The operation retains its single installation owner. Unbound frontends select
/// at mailbox admission; provider calls freeze their selected endpoint before it.
pub(crate) enum WorkbenchToolOperation {
    Unbound(Option<crate::resident_workbench::ActorRequestSnapshot>),
    Installed(std::sync::Arc<crate::resident_workbench::AdmittedToolCall>),
}

impl WorkbenchToolOperation {
    pub(crate) fn installation(&self) -> Option<&crate::InstalledToolLease> {
        match self {
            Self::Unbound(installation) => installation
                .as_ref()
                .map(|snapshot| snapshot.installation()),
            Self::Installed(call) => Some(call.selected().lease()),
        }
    }

    pub(crate) fn snapshot(&self) -> Option<&crate::resident_workbench::ActorRequestSnapshot> {
        match self {
            Self::Unbound(snapshot) => snapshot.as_ref(),
            Self::Installed(call) => Some(call.selected().snapshot()),
        }
    }

    pub(crate) fn selected(
        &self,
    ) -> Option<&std::sync::Arc<crate::resident_workbench::AdmittedToolCall>> {
        match self {
            Self::Unbound(_) => None,
            Self::Installed(call) => Some(call),
        }
    }
}

/// Common invocation data, independent of endpoint selection and semantic input.
#[derive(Default)]
pub(crate) struct WorkbenchInvocationMetadata {
    input: Option<serde_json::Value>,
    verbose: Option<bool>,
    execution: Option<tidepool_runtime::session::WorkbenchExecutionId>,
    checkpoint: Option<tidepool_runtime::session::ContextCheckpointBoundary>,
}

impl WorkbenchInvocationMetadata {
    pub(crate) fn capture(request: &WorkbenchRequest) -> Self {
        Self {
            input: request.input.clone(),
            verbose: request.verbose,
            execution: request.execution_id().cloned(),
            checkpoint: request.checkpoint_boundary().cloned(),
        }
    }

    pub(crate) fn apply(self, mut request: WorkbenchRequest) -> WorkbenchRequest {
        request.input = self.input;
        request.verbose = self.verbose;
        if let Some(execution) = self.execution {
            request = request.with_execution_id(execution);
        }
        if let Some(checkpoint) = self.checkpoint {
            request = request.with_checkpoint_boundary(checkpoint);
        }
        request
    }
}

/// Actor-owned authority beside the transport-neutral workbench request.
/// Semantic input is constructed here and cannot be independently replaced.
/// Transport identity and checkpoint metadata never contain a handler.
pub struct ActorWorkbenchInvocation {
    pub(crate) initial_context: bool,
    pub(crate) display_expansion: Option<((i64, i64, i64), i64)>,
    request: WorkbenchRequest,
    operation: WorkbenchToolOperation,
    pub(crate) hosted_checkpoint_capture:
        Option<std::sync::Arc<dyn crate::HostedCheckpointCapture>>,
    pub(crate) context_binding: Option<std::sync::Arc<dyn crate::HostedContextBinding>>,
}

impl ActorWorkbenchInvocation {
    pub fn unbound(request: WorkbenchRequest) -> Self {
        Self::issued(request, None, None, None)
    }

    pub fn request(&self) -> &WorkbenchRequest {
        &self.request
    }

    pub(crate) fn selected_endpoint(&self) -> Option<&exomonad_tool::EndpointPlan> {
        self.operation
            .selected()
            .map(|call| call.selected().endpoint().plan())
    }

    /// Execute an existing display callback through the actor's serialized mailbox.
    /// Identity and key are validated against the actor's retained display slots.
    pub fn for_display_expansion(identity: (i64, i64, i64), key: i64) -> Self {
        let mut invocation = Self::unbound(WorkbenchRequest::from_cell_input(""));
        invocation.display_expansion = Some((identity, key));
        invocation
    }

    pub(crate) fn issued(
        request: WorkbenchRequest,
        installation: Option<crate::resident_workbench::ActorRequestSnapshot>,
        hosted_checkpoint_capture: Option<std::sync::Arc<dyn crate::HostedCheckpointCapture>>,
        context_binding: Option<std::sync::Arc<dyn crate::HostedContextBinding>>,
    ) -> Self {
        Self {
            initial_context: false,
            display_expansion: None,
            request,
            operation: WorkbenchToolOperation::Unbound(installation),
            hosted_checkpoint_capture,
            context_binding,
        }
    }

    pub(crate) fn installed(
        call: crate::resident_workbench::AdmittedToolCall,
        metadata: WorkbenchInvocationMetadata,
        hosted_checkpoint_capture: Option<std::sync::Arc<dyn crate::HostedCheckpointCapture>>,
        context_binding: Option<std::sync::Arc<dyn crate::HostedContextBinding>>,
    ) -> Self {
        Self {
            initial_context: false,
            display_expansion: None,
            request: metadata.apply(call.request()),
            operation: WorkbenchToolOperation::Installed(std::sync::Arc::new(call)),
            hosted_checkpoint_capture,
            context_binding,
        }
    }

    /// Fix the execution identity before the scheduler records its pending step.
    /// An already-issued identity remains the correlation authority.
    pub(crate) fn issue_execution_identity(
        self,
        actor: ActorRef,
        control: &crate::WorkbenchExecutionControl,
    ) -> Self {
        if self.request.execution_id().is_some() {
            self
        } else {
            self.with_execution_id(control.execution_id(actor))
        }
    }

    pub(crate) fn with_execution_id(
        mut self,
        execution: tidepool_runtime::session::WorkbenchExecutionId,
    ) -> Self {
        self.request = self.request.with_execution_id(execution);
        self
    }

    pub(crate) fn with_checkpoint_boundary(
        mut self,
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> Self {
        self.request = self.request.with_checkpoint_boundary(boundary);
        self
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        WorkbenchRequest,
        WorkbenchToolOperation,
        Option<std::sync::Arc<dyn crate::HostedCheckpointCapture>>,
        Option<std::sync::Arc<dyn crate::HostedContextBinding>>,
    ) {
        (
            self.request,
            self.operation,
            self.hosted_checkpoint_capture,
            self.context_binding,
        )
    }
}

/// Distinguish ordinary continuation from release of the admitted root boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelResume {
    ContinueProgram,
    ReleaseRootStartup,
}

/// Every ordinary operation serialized through one local actor.
///
/// Actor creation is intentionally absent: the owning actor calls
/// `spawn_linked` and publishes the returned exact handle only after startup.
/// Waiting is also absent: callers await [`RetainedActorExit`] directly. Kill
/// remains a Ractor control signal; `Shutdown` is the cooperative typed-hook
/// path.
pub enum KernelMessage {
    /// Only the lifecycle coordinator can construct an admitted envelope.
    Admitted(AdmittedKernelEnvelope),
    ReplaceSpec {
        definition: Box<crate::SpecReplacementDefinition>,
        reply: RpcReplyPort<Result<(), crate::SpecReplacementError>>,
    },
    Replace {
        definition: Box<crate::ActorReplacementDefinition>,
        reply: RpcReplyPort<Result<LocalActorRef, KernelInvocationFailure>>,
    },
    Drain {
        reply: RpcReplyPort<Result<(), KernelBehaviorError>>,
    },
    DrainFence,
    ReplacementFence,
    AbortReplacement {
        reply: RpcReplyPort<Result<(), KernelBehaviorError>>,
    },
    ActivateReplacement {
        backlog: std::collections::VecDeque<KernelMessage>,
        draining: bool,
        reply: RpcReplyPort<Result<(), KernelBehaviorError>>,
    },
    Source(crate::SourceDelivery),
    RouteReady {
        watch: crate::WatchId,
    },
    SealHostedWork {
        reply: RpcReplyPort<crate::HostedWorkSeal>,
    },
    Cast {
        sender: ActorRef,
        request: MailboxValue,
    },
    Call {
        caller: ActorRef,
        ancestry: CallAncestry,
        request: MailboxValue,
        reply: RpcReplyPort<KernelCallReply>,
    },
    Tool {
        invocation: exomonad_tool::ToolInvocation,
        reply: RpcReplyPort<KernelInvocationReply>,
    },
    ToolWithHostedCheckpoint {
        invocation: exomonad_tool::ToolInvocation,
        capture: std::sync::Arc<dyn crate::HostedCheckpointCapture>,
        reply: RpcReplyPort<KernelInvocationReply>,
    },
    Workbench {
        invocation: ActorWorkbenchInvocation,
        control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
        reply: RpcReplyPort<KernelWorkbenchReply>,
    },
    /// Wakes the actor when its one admitted task returns a fenced completion.
    ActorStepCompleted {
        step: WorkbenchStepKey,
        outcome: Box<dyn std::any::Any + Send>,
    },
    ReconcileWorkbenchCancellation {
        invocation: Option<exomonad_tool::ToolInvocationContext>,
        execution: tidepool_runtime::session::WorkbenchExecutionId,
        reply: RpcReplyPort<crate::WorkbenchCancellationOutcome>,
    },
    ReconcileWorkbenchBoundary {
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
        reply: RpcReplyPort<crate::WorkbenchBoundaryReconciliation>,
    },
    ToolCompleted {
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
        reply: RpcReplyPort<KernelInvocationReply>,
    },
    ToolAborted {
        boundary: tidepool_runtime::session::ContextCheckpointBoundary,
        reply: RpcReplyPort<KernelInvocationReply>,
    },
    /// Drain one mailbox request retained while the resident behavior was
    /// parked on an external interaction rather than on `receive`.
    DrainMailbox,
    /// Resume actor-owned work after its initiating caller has been settled.
    Resume {
        kind: KernelResume,
    },
    ExternalApplicationFailed {
        failure: ExternalApplicationFailure,
        reply: RpcReplyPort<ExternalFailureDisposition>,
    },
    Shutdown {
        terminal: ActorTerminal,
        reply: RpcReplyPort<ActorTerminal>,
    },
}

impl KernelMessage {
    /// Inspect the dispatched payload while retaining its affine admission.
    pub(crate) fn payload(&self) -> &Self {
        match self {
            Self::Admitted(envelope) => envelope.payload(),
            message => message,
        }
    }

    /// The variant name alone. The derived-style `Debug` above carries
    /// request payloads with it; a trace field wants the shape of the
    /// dispatch and none of its content.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Admitted(_) => "Admitted",
            Self::Replace { .. } => "Replace",
            Self::ReplaceSpec { .. } => "ReplaceSpec",
            Self::Drain { .. } => "Drain",
            Self::DrainFence => "DrainFence",
            Self::ReplacementFence => "ReplacementFence",
            Self::AbortReplacement { .. } => "AbortReplacement",
            Self::ActivateReplacement { .. } => "ActivateReplacement",
            Self::Source(_) => "Source",
            Self::RouteReady { .. } => "RouteReady",
            Self::SealHostedWork { .. } => "SealHostedWork",
            Self::Cast { .. } => "Cast",
            Self::Call { .. } => "Call",
            Self::Tool { .. } => "Tool",
            Self::ToolWithHostedCheckpoint { .. } => "ToolWithHostedCheckpoint",
            Self::Workbench { .. } => "Workbench",
            Self::ActorStepCompleted { .. } => "ActorStepCompleted",
            Self::ReconcileWorkbenchCancellation { .. } => "ReconcileWorkbenchCancellation",
            Self::ReconcileWorkbenchBoundary { .. } => "ReconcileWorkbenchBoundary",
            Self::ToolCompleted { .. } => "ToolCompleted",
            Self::ToolAborted { .. } => "ToolAborted",
            Self::DrainMailbox => "DrainMailbox",
            Self::Resume { .. } => "Resume",
            Self::ExternalApplicationFailed { .. } => "ExternalApplicationFailed",
            Self::Shutdown { .. } => "Shutdown",
        }
    }
}

impl std::fmt::Debug for KernelMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admitted(_) => formatter.write_str("Admitted"),
            Self::Replace { .. } => formatter.write_str("Replace"),
            Self::ReplaceSpec { .. } => formatter.write_str("ReplaceSpec"),
            Self::Drain { .. } => formatter.write_str("Drain"),
            Self::DrainFence => formatter.write_str("DrainFence"),
            Self::ReplacementFence => formatter.write_str("ReplacementFence"),
            Self::AbortReplacement { .. } => formatter.write_str("AbortReplacement"),
            Self::ActivateReplacement {
                backlog, draining, ..
            } => formatter
                .debug_struct("ActivateReplacement")
                .field("backlog", &backlog.len())
                .field("draining", draining)
                .finish(),
            Self::Source(delivery) => formatter.debug_tuple("Source").field(delivery).finish(),
            Self::RouteReady { watch } => formatter.debug_tuple("RouteReady").field(watch).finish(),
            Self::SealHostedWork { .. } => formatter.write_str("SealHostedWork"),
            Self::Cast { sender, request } => formatter
                .debug_struct("Cast")
                .field("sender", sender)
                .field("request", request)
                .finish(),
            Self::Call {
                caller,
                ancestry,
                request,
                ..
            } => formatter
                .debug_struct("Call")
                .field("caller", caller)
                .field("ancestry", ancestry)
                .field("request", request)
                .finish_non_exhaustive(),
            Self::Tool { invocation, .. } => formatter
                .debug_struct("Tool")
                .field("invocation", invocation)
                .finish_non_exhaustive(),
            Self::ToolWithHostedCheckpoint { invocation, .. } => formatter
                .debug_struct("ToolWithHostedCheckpoint")
                .field("invocation", invocation)
                .finish_non_exhaustive(),
            Self::Workbench { invocation, .. } => formatter
                .debug_struct("Workbench")
                .field("request", &invocation.request)
                .finish_non_exhaustive(),
            Self::ActorStepCompleted { step, .. } => formatter
                .debug_tuple("ActorStepCompleted")
                .field(step)
                .finish(),
            Self::ReconcileWorkbenchCancellation { execution, .. } => formatter
                .debug_tuple("ReconcileWorkbenchCancellation")
                .field(execution)
                .finish(),
            Self::ReconcileWorkbenchBoundary { boundary, .. } => formatter
                .debug_tuple("ReconcileWorkbenchBoundary")
                .field(boundary)
                .finish(),
            Self::ToolCompleted { boundary, .. } => formatter
                .debug_tuple("ToolCompleted")
                .field(boundary)
                .finish(),
            Self::ToolAborted { boundary, .. } => formatter
                .debug_tuple("ToolAborted")
                .field(boundary)
                .finish(),
            Self::DrainMailbox => formatter.write_str("DrainMailbox"),
            Self::Resume { kind } => formatter.debug_tuple("Resume").field(kind).finish(),
            Self::ExternalApplicationFailed { failure, .. } => formatter
                .debug_struct("ExternalApplicationFailed")
                .field("failure", failure)
                .finish_non_exhaustive(),
            Self::Shutdown { terminal, .. } => formatter
                .debug_struct("Shutdown")
                .field("terminal", terminal)
                .finish_non_exhaustive(),
        }
    }
}

/// Exact local address paired with immutable terminal observation.
///
/// The Ractor PID is unique only for the process lifetime. A local host pairs
/// it with one durably claimed incarnation so exact handles remain distinct
/// after process restart.
#[derive(Clone)]
pub struct LocalActorRef {
    identity: ActorRef,
    address: RactorRef<KernelMessage>,
    terminal: RetainedActorExit,
    lifecycle: LifecycleHandle,
}

/// One incarnation's published hosted calls, from transport queuing through
/// native completion and provider acknowledgement. Multiple callers may queue
/// behind one active notebook; dropping one waiter cannot erase another call.
pub(crate) type HostedCellSlot = std::sync::Arc<HostedCellPublications>;

#[derive(Default)]
pub(crate) struct HostedCellPublications(parking_lot::Mutex<Vec<HostedCellEntry>>);

struct HostedCellEntry {
    control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
    provider: Option<crate::HostedOperationSettlement>,
    accepted: bool,
}

impl HostedCellPublications {
    pub(crate) fn publish_transport(
        &self,
        control: std::sync::Arc<crate::WorkbenchExecutionControl>,
    ) {
        self.0.lock().push(HostedCellEntry {
            control: Some(control),
            provider: None,
            accepted: false,
        });
    }

    pub(crate) fn accept(&self, control: &std::sync::Arc<crate::WorkbenchExecutionControl>) {
        let mut entries = self.0.lock();
        if let Some(entry) = entries.iter_mut().find(|entry| {
            entry
                .control
                .as_ref()
                .cloned()
                .or_else(|| entry.provider.as_ref().and_then(|owner| owner.control()))
                .is_some_and(|issued| std::sync::Arc::ptr_eq(&issued, control))
        }) {
            entry.accepted = true;
        }
    }

    pub(crate) fn claim(&self, control: &std::sync::Arc<crate::WorkbenchExecutionControl>) {
        let mut entries = self.0.lock();
        if let Some(entry) = entries.iter_mut().find(|entry| {
            entry
                .control
                .as_ref()
                .cloned()
                .or_else(|| entry.provider.as_ref().and_then(|owner| owner.control()))
                .is_some_and(|issued| std::sync::Arc::ptr_eq(&issued, control))
        }) {
            entry.accepted = true;
            // A sequential request may have yielded its active slot while
            // remaining queued. Claim restores work tracking, not authority.
            if entry.control.is_none() && control.terminal_reply().is_none() {
                entry.control = Some(control.clone());
            }
        } else if control.invocation.is_some() {
            entries.push(HostedCellEntry {
                control: Some(std::sync::Arc::clone(control)),
                provider: None,
                accepted: true,
            });
        }
    }

    pub(crate) fn withdraw_transport(
        &self,
        control: &std::sync::Arc<crate::WorkbenchExecutionControl>,
    ) {
        self.0.lock().retain(|entry| {
            entry.accepted
                || !entry
                    .control
                    .as_ref()
                    .is_some_and(|issued| std::sync::Arc::ptr_eq(issued, control))
        });
    }

    pub(crate) fn publish_provider_transport(
        &self,
        actor: crate::ActorRef,
        control: std::sync::Arc<crate::WorkbenchExecutionControl>,
    ) {
        let mut entries = self.0.lock();
        // One logical owner per operation. Native physical retries have no
        // independent provider acknowledgement: Harness's durable claim and
        // scheduler admit an OperationId only once. The native journal alone
        // may alias an exact terminal replay to this existing owner.
        let duplicate = entries
            .iter()
            .filter_map(|entry| entry.provider.as_ref())
            .any(|owner| control.invocation.as_ref() == Some(owner.key()));
        let provider = if duplicate {
            None
        } else {
            Some(crate::HostedOperationSettlement::issue(
                actor,
                control.clone(),
            ))
        };
        entries.push(HostedCellEntry {
            control: Some(control),
            provider,
            accepted: false,
        });
    }

    pub(crate) fn complete(&self, control: &std::sync::Arc<crate::WorkbenchExecutionControl>) {
        self.0.lock().retain_mut(|entry| {
            if !entry
                .control
                .as_ref()
                .is_some_and(|issued| std::sync::Arc::ptr_eq(issued, control))
            {
                return true;
            }
            if control.provider_replay.get().is_some() {
                // The exact journal replay already names the retained logical
                // owner. Drop this physical transport entry, not that owner.
                return false;
            }
            match &entry.provider {
                Some(provider) => {
                    // Deferred mailbox work releases active tracking without
                    // pretending native computation has reached its terminal.
                    if control.terminal_reply().is_some() {
                        provider.native_finished();
                    }
                    entry.control.take();
                    true
                }
                None => false,
            }
        });
    }

    pub(crate) fn take_all_and_clear(
        &self,
    ) -> Vec<std::sync::Arc<crate::WorkbenchExecutionControl>> {
        let mut controls = Vec::new();
        self.0.lock().retain_mut(|entry| {
            if let Some(control) = entry.control.take() {
                controls.push(control);
                if let Some(provider) = &entry.provider {
                    provider.native_finished();
                }
            }
            entry.provider.is_some()
        });
        controls
    }

    pub(crate) fn finalization_owner_lost(&self) {
        for provider in self
            .0
            .lock()
            .iter()
            .filter_map(|entry| entry.provider.as_ref())
        {
            if let Some(control) = provider.control() {
                control.provider_finalization.finish(Err(
                    "native actor ended without confirming this operation's finalization".into(),
                ));
            }
        }
    }

    pub(crate) fn retained_operation(
        &self,
        invocation: &crate::resident_tools::WorkbenchCallKey,
    ) -> Result<Option<crate::HostedOperationSettlement>, String> {
        let entries = self.0.lock();
        let mut matches = entries
            .iter()
            .filter_map(|entry| entry.provider.as_ref())
            .filter(|provider| provider.key() == invocation);
        let Some(owner) = matches.next() else {
            return Ok(None);
        };
        if matches.all(|other| owner.same_owner(other)) {
            Ok(Some(owner.clone()))
        } else {
            Err("one operation has contradictory native settlement owners".into())
        }
    }

    pub(crate) fn retained_boundary(
        &self,
        boundary: &tidepool_runtime::session::ContextCheckpointBoundary,
    ) -> Result<crate::HostedOperationSettlement, String> {
        let entries = self.0.lock();
        let mut matches = entries
            .iter()
            .filter_map(|entry| entry.provider.as_ref())
            .filter(|provider| provider.key().matches_boundary(boundary));
        let Some(owner) = matches.next() else {
            return Err("provider boundary has no issued native operation".into());
        };
        if owner.key().is_original_invocation() && matches.all(|other| owner.same_owner(other)) {
            Ok(owner.clone())
        } else {
            Err("nested or multiple native operations cannot certify the enclosing provider boundary".into())
        }
    }

    pub(crate) fn find(
        &self,
        predicate: impl Fn(&crate::WorkbenchExecutionControl) -> bool,
    ) -> Option<std::sync::Arc<crate::WorkbenchExecutionControl>> {
        self.0
            .lock()
            .iter()
            .filter_map(|entry| entry.control.as_ref())
            .find(|control| predicate(control))
            .cloned()
    }

    fn computing(&self) -> bool {
        self.0.lock().iter().any(|entry| {
            entry
                .control
                .as_ref()
                .is_some_and(|control| control.is_computing_hosted_cell())
        })
    }
}

impl std::fmt::Debug for LocalActorRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalActorRef")
            .field("identity", &self.identity)
            .field("status", &self.address.get_status())
            .field("terminal", &self.terminal)
            .finish()
    }
}

/// Actor-owned retirement intent is issued for the entire captured set before
/// any drain future or resource teardown can settle another member's wait.
pub(crate) struct RetirementBatch {
    actors: Vec<(LocalActorRef, ActorTerminal)>,
}

impl RetirementBatch {
    pub(crate) async fn issue(actors: Vec<(LocalActorRef, ActorTerminal)>) -> Self {
        let requests = actors
            .iter()
            .map(|(actor, terminal)| (actor.terminal.clone(), terminal.clone()))
            .collect::<Vec<_>>();
        RetainedActorExit::request_shutdown_batch(&requests, || {});
        let acknowledgements = actors
            .iter()
            .filter_map(|(actor, terminal)| {
                actor.lifecycle.dispatch_shutdown(terminal.clone()).ok()
            })
            .collect::<Vec<_>>();
        // Dispatch the captured set before awaiting a member's close fence.
        // Cleanup starts only after every reachable owner has acknowledged.
        for acknowledgement in acknowledgements {
            let _ = acknowledgement.await;
        }
        Self { actors }
    }

    pub(crate) fn into_actors(self) -> Vec<(LocalActorRef, ActorTerminal)> {
        self.actors
    }
}

impl LocalActorRef {
    pub(crate) fn unavailable_invocation(&self, detail: &str) -> KernelInvocationFailure {
        if self.terminal.get().is_some() {
            KernelInvocationFailure::ActorExited(self.identity)
        } else {
            KernelInvocationFailure::CleanupUnconfirmed {
                publication: None,
                actor: self.identity,
                detail: detail.into(),
                receipts: Vec::new(),
            }
        }
    }

    fn control_admission_failure(&self, failure: KernelCallFailure) -> KernelInvocationFailure {
        match failure {
            KernelCallFailure::TargetExited(_) => KernelInvocationFailure::ActorExited(self.identity),
            KernelCallFailure::TargetUnavailable(_) => self.unavailable_invocation(
                "actor control admission outcome is unavailable; inspect retained state before retrying",
            ),
            failure @ (KernelCallFailure::MailboxClosed(_)
            | KernelCallFailure::InstallationPending(_)
            | KernelCallFailure::RetirementPending(_)) => KernelInvocationFailure::Rejected {
                actor: self.identity,
                detail: failure.to_string(),
                receipts: Vec::new(),
                diagnostic: None,
            },
            KernelCallFailure::Handler { actor, detail, diagnostic } => KernelInvocationFailure::Failed {
                actor, detail, diagnostic, receipts: Vec::new(),
            },
            failure => KernelInvocationFailure::Failed {
                actor: self.identity,
                detail: failure.to_string(),
                receipts: Vec::new(),
                diagnostic: None,
            },
        }
    }

    pub(crate) fn hosted_cell(&self) -> &HostedCellSlot {
        self.lifecycle.hosted_cell()
    }

    /// Whether this incarnation is inside a model-visible `haskell` call that
    /// is computing (or still waiting to start) rather than sleeping. Codex
    /// cancels such a call before admitting new input and cannot finish that
    /// exchange until the cell ends, so the host's delivery pump defers
    /// native submission while this holds.
    #[must_use]
    pub fn hosted_cell_computing(&self) -> bool {
        self.lifecycle.hosted_cell().computing()
    }

    /// Observe whether this exact original hosted call currently has an armed
    /// workbench wait. The call can resume or settle immediately afterwards;
    /// this observation does not acknowledge cancellation.
    #[must_use]
    pub fn hosted_workbench_waiting(
        &self,
        invocation: &exomonad_tool::ToolInvocationContext,
    ) -> Option<tidepool_runtime::session::WorkbenchExecutionId> {
        let key = crate::resident_tools::WorkbenchCallKey::from(invocation.clone());
        if !key.is_original_invocation() {
            return None;
        }
        let execution = crate::resident_tools::execution_id(self.identity, &key);
        self.hosted_cell()
            .find(|control| {
                control.is_waiting_hosted_cell()
                    && control.invocation.as_ref().is_some_and(|key| {
                        crate::resident_tools::execution_id(self.identity, key) == execution
                    })
            })
            .map(|_| execution)
    }

    pub(crate) async fn abort_prepared_replacement(&self) -> Result<(), KernelBehaviorError> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.lifecycle
            .control(KernelMessage::AbortReplacement {
                reply: reply.into(),
            })
            .await
            .map_err(|error| KernelBehaviorError {
                detail: format!("prepared actor {:?}: {error}", self.identity),
                diagnostic: None,
            })?;
        receive.await.map_err(|_| KernelBehaviorError {
            detail: format!(
                "prepared actor {:?} cleanup outcome unavailable",
                self.identity
            ),
            diagnostic: None,
        })?
    }
    pub(crate) async fn fence_replacement(&self) -> Result<(), KernelBehaviorError> {
        self.lifecycle
            .close_with_fence(KernelMessage::ReplacementFence)
            .await
            .map_err(|error| KernelBehaviorError {
                detail: error.to_string(),
                diagnostic: None,
            })
    }
    /// Close admission after behavior validation. This acknowledges the fence;
    /// the retained terminal reports completion after accepted work is handled.
    pub async fn drain(&self) -> Result<(), KernelInvocationFailure> {
        if self.terminal.get().is_some() {
            return Ok(());
        }
        let result = self.request_drain().await;
        if result.is_err() && self.terminal.get().is_some() {
            return Ok(());
        }
        result
    }

    pub(crate) async fn replace_spec(
        &self,
        definition: crate::SpecReplacementDefinition,
    ) -> Result<(), crate::SpecReplacementError> {
        if self.terminal.get().is_some() {
            return Err(crate::SpecReplacementError::Unavailable);
        }
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.lifecycle
            .control(KernelMessage::ReplaceSpec {
                definition: Box::new(definition),
                reply: reply.into(),
            })
            .await
            .map_err(|_| crate::SpecReplacementError::Unavailable)?;
        receive.await.map_err(|_| {
            crate::SpecReplacementError::Failed(
                "spec replacement outcome unavailable; inspect target before retrying".into(),
            )
        })?
    }

    pub(crate) async fn replace(
        &self,
        definition: crate::ActorReplacementDefinition,
    ) -> Result<Self, KernelInvocationFailure> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.lifecycle
            .control(KernelMessage::Replace {
                definition: Box::new(definition),
                reply: reply.into(),
            })
            .await
            .map_err(|failure| self.control_admission_failure(failure))?;
        receive.await.map_err(|_| KernelInvocationFailure::Failed { receipts: Vec::new(),
            actor: self.identity,
            detail: "replacement outcome unavailable; inspect retained actor state before further action".into(),
            diagnostic: None,
        })?
    }

    async fn request_drain(&self) -> Result<(), KernelInvocationFailure> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.lifecycle
            .close_with_fence(KernelMessage::Drain {
                reply: reply.into(),
            })
            .await
            .map_err(|failure| self.control_admission_failure(failure))?;
        receive
            .await
            .map_err(|_| {
                self.unavailable_invocation("admitted actor drain outcome is unavailable")
            })?
            .map_err(|error| KernelInvocationFailure::Rejected {
                receipts: Vec::new(),
                actor: self.identity,
                detail: error.detail,
                diagnostic: error.diagnostic,
            })
    }
    pub(crate) fn with_identity_admission(
        address: RactorRef<KernelMessage>,
        terminal: RetainedActorExit,
        identity: ActorRef,
        lifecycle: LifecycleHandle,
    ) -> Self {
        Self {
            identity,
            address,
            terminal,
            lifecycle,
        }
    }

    pub fn lifecycle(&self) -> &LifecycleHandle {
        &self.lifecycle
    }

    /// Admission and queue insertion share the close fence. Accepted payloads
    /// belong to the existing Ractor mailbox even while execution is paused.
    pub(crate) async fn admit_mailbox(
        &self,
        message: KernelMessage,
    ) -> Result<(), KernelCallFailure> {
        self.lifecycle.enqueue(message).await
    }

    /// Create a short transaction lease through the original native owner.
    pub async fn admit_transaction(&self) -> Result<ActorAdmissionLease, KernelCallFailure> {
        self.lifecycle.transaction().await
    }

    pub async fn attach_native_provider(
        &self,
        observation: crate::ActorRuntimeObservationHandle,
        readiness: std::sync::Arc<tidepool_runtime::session::RuntimeDurablePublicReadiness>,
        probe: std::sync::Arc<dyn ProviderIdleProbe>,
    ) -> Result<NativeProviderAdmission, &'static str> {
        self.lifecycle
            .attach_provider(observation, readiness, probe)
            .await
    }

    /// Atomically fence new provider rounds and input before checking the Store.
    pub async fn retire_idle_by(
        &self,
        supervisor: ActorRef,
        terminal: ActorTerminal,
    ) -> Result<ActorTerminal, KernelInvocationFailure> {
        if let Some(existing) = self.terminal.get() {
            return Ok(existing);
        }
        let rejected = |detail: String| KernelInvocationFailure::Rejected {
            actor: self.identity,
            detail,
            receipts: Vec::new(),
            diagnostic: None,
        };
        self.lifecycle
            .claim_idle(terminal.clone())
            .await
            .map_err(rejected)?;
        self.retire_by(supervisor, terminal).await
    }

    pub async fn cast(
        &self,
        sender: ActorRef,
        request: MailboxValue,
    ) -> Result<(), KernelCallFailure> {
        self.admit_mailbox(KernelMessage::Cast { sender, request })
            .await
    }

    pub(crate) async fn source(
        &self,
        delivery: crate::SourceDelivery,
    ) -> Result<(), KernelCallFailure> {
        self.admit_mailbox(KernelMessage::Source(delivery)).await
    }

    /// Submit a typed workbench request through the exact incarnation owner.
    /// The owner retains admitted execution independently of its reply observer.
    pub async fn submit_workbench(
        &self,
        invocation: ActorWorkbenchInvocation,
        control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
        reply: RpcReplyPort<KernelWorkbenchReply>,
    ) -> Result<(), KernelCallFailure> {
        self.admit_mailbox(KernelMessage::Workbench {
            invocation,
            control,
            reply,
        })
        .await
    }

    pub async fn call(
        &self,
        caller: ActorRef,
        ancestry: CallAncestry,
        request: MailboxValue,
    ) -> KernelCallReply {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.admit_mailbox(KernelMessage::Call {
            caller,
            ancestry,
            request,
            reply: reply.into(),
        })
        .await?;
        receive
            .await
            .map_err(|_| self.lifecycle.unavailable_failure())?
    }

    #[must_use]
    pub fn identity(&self) -> ActorRef {
        self.identity
    }

    #[must_use]
    pub(crate) fn address(&self) -> &RactorRef<KernelMessage> {
        &self.address
    }

    #[must_use]
    pub fn terminal(&self) -> &RetainedActorExit {
        &self.terminal
    }

    pub async fn report_external_failure(
        &self,
        failure: ExternalApplicationFailure,
    ) -> Result<ExternalFailureDisposition, KernelInvocationFailure> {
        if self.terminal.get().is_some() {
            return Ok(ExternalFailureDisposition::AlreadyTerminal);
        }
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.lifecycle
            .control(KernelMessage::ExternalApplicationFailed {
                failure,
                reply: reply.into(),
            })
            .await
            .map_err(|failure| self.control_admission_failure(failure))?;
        receive.await.map_err(|_| {
            self.unavailable_invocation("admitted actor control outcome is unavailable")
        })
    }

    pub async fn seal_hosted_work(&self) -> Result<crate::HostedWorkSeal, KernelInvocationFailure> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.lifecycle
            .control(KernelMessage::SealHostedWork {
                reply: reply.into(),
            })
            .await
            .map_err(|failure| self.control_admission_failure(failure))?;
        receive.await.map_err(|_| {
            self.unavailable_invocation("admitted actor control outcome is unavailable")
        })
    }

    /// Observe the same actor-owned retirement after waiter loss. A legacy or
    /// forced terminal never becomes confirmed cleanup by observation.
    pub async fn shutdown_with_cleanup(
        &self,
        terminal: ActorTerminal,
    ) -> Result<crate::ResidentShutdown, KernelInvocationFailure> {
        let terminal = self.shutdown(terminal).await?;
        let cleanup = self
            .terminal
            .cleanup()
            .unwrap_or_else(|| crate::ResidentCleanupOutcome {
                actor: self.identity,
                hook: crate::CleanupComponentOutcome::Unconfirmed("terminal-only exit".into()),
                realm: crate::CleanupComponentOutcome::Unconfirmed("terminal-only exit".into()),
                children: crate::CleanupComponentOutcome::Unconfirmed("terminal-only exit".into()),
            });
        Ok(crate::ResidentShutdown { terminal, cleanup })
    }

    pub async fn shutdown(
        &self,
        terminal: ActorTerminal,
    ) -> Result<ActorTerminal, KernelInvocationFailure> {
        if let Some(existing) = self.terminal.get() {
            return Ok(existing);
        }
        self.send_shutdown(terminal).await
    }

    async fn send_shutdown(
        &self,
        terminal: ActorTerminal,
    ) -> Result<ActorTerminal, KernelInvocationFailure> {
        let _batch = RetirementBatch::issue(vec![(self.clone(), terminal.clone())]).await;
        let terminal = self.terminal.requested_shutdown().unwrap_or(terminal);
        let (reply, receive) = tokio::sync::oneshot::channel();
        if self
            .lifecycle
            .control(KernelMessage::Shutdown {
                terminal,
                reply: reply.into(),
            })
            .await
            .is_err()
        {
            // A concurrent bootstrap can finish between recording intent and
            // enqueueing the mailbox operation. Reuse only its published exit.
            return self.terminal.get().ok_or_else(|| {
                self.unavailable_invocation("actor retirement outcome is unavailable")
            });
        }
        match receive.await {
            Ok(terminal) => Ok(terminal),
            // Bootstrap may observe the request before draining its mailbox.
            // Its lifecycle owner still performs and publishes exact cleanup.
            Err(_) => self.terminal.get().ok_or_else(|| {
                self.unavailable_invocation("actor retirement outcome is unavailable")
            }),
        }
    }

    /// Request retirement and retain which supervisor received its acknowledgement.
    pub async fn retire_by(
        &self,
        supervisor: ActorRef,
        terminal: ActorTerminal,
    ) -> Result<ActorTerminal, KernelInvocationFailure> {
        if let Some(existing) = self.terminal.get() {
            return Ok(existing);
        }
        let result = self.send_shutdown(terminal).await?;
        if result.kind == crate::ActorExitKind::Cancelled {
            self.terminal.acknowledge_retirement(supervisor);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_ancestry_rejects_direct_and_indirect_reentry() {
        let a = ActorRef::first(crate::ActorId(1));
        let b = ActorRef::first(crate::ActorId(2));
        let path = CallAncestry::begin(a).enter(b).expect("a -> b");

        assert_eq!(path.actors(), &[a, b]);
        assert_eq!(
            path.enter(a),
            Err(KernelCallFailure::Cycle {
                target: a,
                ancestry: vec![a, b],
            })
        );
        assert_eq!(
            CallAncestry::begin(a).enter(a),
            Err(KernelCallFailure::Cycle {
                target: a,
                ancestry: vec![a],
            })
        );
    }

    #[test]
    fn workbench_failure_keeps_committed_prefix_and_exact_cause() {
        let failure = KernelWorkbenchFailure {
            actor: ActorRef::first(crate::ActorId(7)),
            receipts: vec![tidepool_runtime::session::WorkbenchItemReceipt {
                diagnostics: Vec::new(),
                index: 0,
                kind: None,
                span: None,
                source_items: Vec::new(),
                status: tidepool_runtime::session::WorkbenchItemStatus::Committed,
                output: "defined spotTaskText at generation 2".into(),
                value: None,
                warnings: Vec::new(),
                installed_bindings: vec!["spotTaskText".into()],
                operations: Vec::new(),
                terminal_transfer: None,
                failure_layer: None,
            }],
            point: tidepool_runtime::session::WorkbenchFailurePoint::InputUnit { index: 1 },
            total: 3,
            publication: None,
            diagnostic: None,
            detail: "actor protocol violation: unsupported resident actor request `MissingEffect`"
                .into(),
        };
        assert_eq!(
            failure.to_string(),
            "actor 7@1 workbench input unit 2 of 3 failed: actor protocol violation: unsupported resident actor request `MissingEffect`\ninput receipts before failure:\ninput unit 1 (Committed): defined spotTaskText at generation 2"
        );
    }

    #[test]
    fn publication_failure_follows_all_completed_receipts() {
        use tidepool_runtime::session::{
            WorkbenchFailurePoint, WorkbenchItemReceipt, WorkbenchItemStatus,
            WorkbenchOperationDisposition, WorkbenchOperationId, WorkbenchOperationReceipt,
            WorkbenchPublicationOutcome,
        };
        let receipts = (0..3)
            .map(|index| WorkbenchItemReceipt {
                index,
                kind: None,
                span: None,
                source_items: Vec::new(),
                status: WorkbenchItemStatus::Committed,
                output: format!("private result {index}"),
                value: None,
                diagnostics: Vec::new(),
                failure_layer: None,
                warnings: Vec::new(),
                installed_bindings: vec![format!("private{index}")],
                operations: vec![WorkbenchOperationReceipt {
                    display_publication: None,
                    display: None,
                    id: WorkbenchOperationId {
                        execution: tidepool_runtime::session::WorkbenchExecutionId::from_digest(
                            [index as u8; 16],
                        ),
                        input_unit_index: index,
                        effect_ordinal: 0,
                    },
                    effect: "receipt-bearing effect".into(),
                    disposition: WorkbenchOperationDisposition::Committed,
                }],
                terminal_transfer: None,
            })
            .collect();
        let failure = KernelWorkbenchFailure {
            actor: ActorRef::first(crate::ActorId(8)),
            receipts,
            point: WorkbenchFailurePoint::Publication {
                completed_input_units: 3,
            },
            total: 3,
            publication: Some(WorkbenchPublicationOutcome::Rejected {
                detail: "staged environment changed".into(),
            }),
            detail: "staged environment changed".into(),
            diagnostic: None,
        };
        let rendered = failure.to_string();
        assert!(rendered.contains("publication rejected after 3 completed input units"));
        assert!(rendered.contains("private result 2"));
        assert!(rendered.contains("receipt-bearing effect"));
        assert!(!rendered.contains("input unit 3 of 3 failed"));
        assert_eq!(failure.point.next_index(), 3);
        let mut uncertain = failure;
        uncertain.publication = Some(WorkbenchPublicationOutcome::RecoveryRequired {
            detail: "journal outcome unknown".into(),
        });
        assert!(uncertain
            .to_string()
            .contains("publication requires authenticated recovery after 3 completed input units"));
        assert!(uncertain
            .publication
            .as_ref()
            .unwrap()
            .public_bindings()
            .is_empty());
    }
}
