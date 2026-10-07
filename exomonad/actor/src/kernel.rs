//! Local message protocol and exact handles for the Ractor-owned kernel.
//!
//! This protocol is deliberately process-local and non-serializable. JSON is
//! reserved for real external boundaries; live Haskell roots move directly
//! through these messages under Rust ownership.

use ractor::{ActorRef as RactorRef, RpcReplyPort};
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
            Self::ActorExited(_) => &[],
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
            Self::ActorExited(_) => None,
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
                Some(tidepool_runtime::session::WorkbenchPublicationOutcome::DurabilityUnconfirmed { .. }) => write!(
                    formatter,
                    "actor {} workbench publication durability remains unconfirmed after {} completed input units: {}",
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

/// Actor-owned authority beside the transport-neutral workbench request.
/// The runtime request and its exact execution journal never contain a handler.
pub struct ActorWorkbenchInvocation {
    pub(crate) display_expansion: Option<((i64, i64, i64), i64)>,
    pub request: WorkbenchRequest,
    pub(crate) installed_tools: Option<crate::resident_workbench::InstalledToolLease>,
    pub(crate) hosted_checkpoint_capture:
        Option<std::sync::Arc<dyn crate::HostedCheckpointCapture>>,
    pub(crate) context_binding: Option<std::sync::Arc<dyn crate::HostedContextBinding>>,
    pub(crate) selected_tool: Option<exomonad_tool::HostedTool>,
}

impl ActorWorkbenchInvocation {
    pub fn unbound(request: WorkbenchRequest) -> Self {
        Self {
            display_expansion: None,
            request,
            installed_tools: None,
            hosted_checkpoint_capture: None,
            context_binding: None,
            selected_tool: None,
        }
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
        installed_tools: Option<crate::resident_workbench::InstalledToolLease>,
        hosted_checkpoint_capture: Option<std::sync::Arc<dyn crate::HostedCheckpointCapture>>,
    ) -> Self {
        Self {
            display_expansion: None,
            request,
            installed_tools,
            hosted_checkpoint_capture,
            context_binding: None,
            selected_tool: None,
        }
    }

    pub(crate) fn with_context(
        mut self,
        binding: Option<std::sync::Arc<dyn crate::HostedContextBinding>>,
        selected_tool: Option<exomonad_tool::HostedTool>,
    ) -> Self {
        self.context_binding = binding;
        self.selected_tool = selected_tool;
        self
    }
}

/// Distinguish a program continuation from confirmation of an already
/// visible child surface. A repeated confirmation never resumes effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelResume {
    ContinueProgram,
    ConfirmChildDurability,
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
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
        reply: RpcReplyPort<crate::WorkbenchBoundaryReconciliation>,
    },
    ToolCompleted {
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
        reply: RpcReplyPort<KernelInvocationReply>,
    },
    ToolAborted {
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
        reply: RpcReplyPort<KernelInvocationReply>,
    },
    ReleaseFork {
        release: crate::ForkChildRelease,
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
    /// The variant name alone. The derived-style `Debug` above carries
    /// request payloads with it; a trace field wants the shape of the
    /// dispatch and none of its content.
    pub fn kind(&self) -> &'static str {
        match self {
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
            Self::ReleaseFork { .. } => "ReleaseFork",
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
            Self::Replace { .. } => formatter.write_str("Replace"),
            Self::ReplaceSpec { .. } => formatter.write_str("ReplaceSpec"),
            Self::Drain { .. } => formatter.write_str("Drain"),
            Self::DrainFence => formatter.write_str("DrainFence"),
            Self::ReplacementFence => formatter.write_str("ReplacementFence"),
            Self::AbortReplacement { .. } => formatter.write_str("AbortReplacement"),
            Self::ActivateReplacement { backlog, draining } => formatter
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
            Self::ReleaseFork { release } => {
                formatter.debug_tuple("ReleaseFork").field(release).finish()
            }
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
    admission: MailboxAdmission,
}

/// Admission shares its fence with the incarnation's hosted-cell slot: both
/// are per-incarnation state every handle to the actor (the directory's and
/// the spawner's alike) must observe identically.
#[derive(Clone, Default)]
pub(crate) struct MailboxAdmission(std::sync::Arc<AdmissionOwner>, HostedCellSlot);

/// One incarnation's published hosted calls, from transport queuing through
/// actor-owned completion. Multiple callers may be queued behind one active
/// notebook; dropping one waiter must not erase another call's control.
pub(crate) type HostedCellSlot = std::sync::Arc<HostedCellPublications>;

#[derive(Default)]
pub(crate) struct HostedCellPublications(parking_lot::Mutex<Vec<HostedCellEntry>>);

struct HostedCellEntry {
    control: std::sync::Arc<crate::WorkbenchExecutionControl>,
    accepted: bool,
}

impl HostedCellPublications {
    pub(crate) fn publish_transport(
        &self,
        control: std::sync::Arc<crate::WorkbenchExecutionControl>,
    ) {
        self.0.lock().push(HostedCellEntry {
            control,
            accepted: false,
        });
    }

    pub(crate) fn accept(&self, control: &std::sync::Arc<crate::WorkbenchExecutionControl>) {
        let mut entries = self.0.lock();
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| std::sync::Arc::ptr_eq(&entry.control, control))
        {
            entry.accepted = true;
        }
    }

    pub(crate) fn claim(&self, control: &std::sync::Arc<crate::WorkbenchExecutionControl>) {
        let mut entries = self.0.lock();
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| std::sync::Arc::ptr_eq(&entry.control, control))
        {
            entry.accepted = true;
        } else if control.invocation.is_some() {
            entries.push(HostedCellEntry {
                control: std::sync::Arc::clone(control),
                accepted: true,
            });
        }
    }

    pub(crate) fn withdraw_transport(
        &self,
        control: &std::sync::Arc<crate::WorkbenchExecutionControl>,
    ) {
        self.0
            .lock()
            .retain(|entry| entry.accepted || !std::sync::Arc::ptr_eq(&entry.control, control));
    }

    pub(crate) fn complete(&self, control: &std::sync::Arc<crate::WorkbenchExecutionControl>) {
        self.0
            .lock()
            .retain(|entry| !std::sync::Arc::ptr_eq(&entry.control, control));
    }

    pub(crate) fn take_all_and_clear(
        &self,
    ) -> Vec<std::sync::Arc<crate::WorkbenchExecutionControl>> {
        self.0.lock().drain(..).map(|entry| entry.control).collect()
    }

    pub(crate) fn find(
        &self,
        predicate: impl Fn(&crate::WorkbenchExecutionControl) -> bool,
    ) -> Option<std::sync::Arc<crate::WorkbenchExecutionControl>> {
        self.0
            .lock()
            .iter()
            .find(|entry| predicate(&entry.control))
            .map(|entry| std::sync::Arc::clone(&entry.control))
    }

    fn computing(&self) -> bool {
        self.0
            .lock()
            .iter()
            .any(|entry| entry.control.is_computing_hosted_cell())
    }
}

#[derive(Default)]
struct AdmissionOwner {
    state: parking_lot::Mutex<AdmissionState>,
    released: tokio::sync::Notify,
}

#[derive(Default)]
struct AdmissionState {
    phase: AdmissionPhase,
    transactions: usize,
    provider: Option<NativeProvider>,
}

#[derive(Default, PartialEq, Eq)]
enum AdmissionPhase {
    #[default]
    Open,
    IdleRetirementClaim,
    Closed,
}

#[derive(PartialEq, Eq)]
enum NativeProviderState {
    NotStarted,
    Active,
    Idle,
    NeedsAttention,
}

struct NativeProvider {
    state: NativeProviderState,
    probe: std::sync::Arc<dyn Fn() -> Result<bool, String> + Send + Sync>,
}

/// Native round admission and idle retirement share the actor's mailbox fence.
/// The provider lease never holds a short Store transaction across inference.
#[derive(Clone)]
pub struct NativeProviderAdmission {
    owner: std::sync::Arc<AdmissionOwner>,
    observation: crate::ActorRuntimeObservationHandle,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum NativeProviderStartError {
    #[error("idle retirement is being checked")]
    RetirementPending,
    #[error("actor admission is closed")]
    Closed,
    #[error("native provider round cannot start: {0}")]
    Invalid(&'static str),
}

pub struct NativeProviderTurnLease {
    admission: NativeProviderAdmission,
    observation: Option<crate::ProviderTurnLease>,
}

impl NativeProviderAdmission {
    /// Wait for the current retirement decision, then re-read durable work.
    /// The notification is registered before checking the fence.
    pub async fn wait_for_retirement_decision(&self) {
        loop {
            let changed = self.owner.released.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.owner.state.lock().phase != AdmissionPhase::IdleRetirementClaim {
                return;
            }
            changed.await;
        }
    }

    pub fn begin_provider_turn(
        &self,
        thread: String,
        turn: String,
    ) -> Result<NativeProviderTurnLease, NativeProviderStartError> {
        let mut state = self.owner.state.lock();
        match state.phase {
            AdmissionPhase::IdleRetirementClaim => {
                return Err(NativeProviderStartError::RetirementPending)
            }
            AdmissionPhase::Closed => return Err(NativeProviderStartError::Closed),
            AdmissionPhase::Open => {}
        }
        let provider = state
            .provider
            .as_mut()
            .ok_or(NativeProviderStartError::Invalid(
                "native provider is not attached",
            ))?;
        if provider.state == NativeProviderState::Active {
            return Err(NativeProviderStartError::Invalid(
                "a native provider round is already active",
            ));
        }
        let observation = self
            .observation
            .begin_provider_turn(thread, turn)
            .map_err(NativeProviderStartError::Invalid)?;
        provider.state = NativeProviderState::Active;
        Ok(NativeProviderTurnLease {
            admission: self.clone(),
            observation: Some(observation),
        })
    }
}

impl NativeProviderTurnLease {
    pub fn succeed(mut self) {
        self.finish(NativeProviderState::Idle, |observation| {
            observation.succeed()
        });
    }

    pub fn fail(mut self, failure: exomonad_model::ProviderFailure) {
        self.finish(NativeProviderState::NeedsAttention, |observation| {
            observation.fail(failure)
        });
    }

    pub fn interrupt(mut self) {
        self.finish(NativeProviderState::NeedsAttention, |observation| {
            observation.interrupt()
        });
    }

    fn finish(
        &mut self,
        outcome: NativeProviderState,
        publish: impl FnOnce(crate::ProviderTurnLease),
    ) {
        let Some(observation) = self.observation.take() else {
            return;
        };
        let mut state = self.admission.owner.state.lock();
        publish(observation);
        if let Some(provider) = state.provider.as_mut() {
            provider.state = outcome;
        }
    }
}

impl Drop for NativeProviderTurnLease {
    fn drop(&mut self) {
        self.finish(NativeProviderState::NeedsAttention, |observation| {
            observation.interrupt()
        });
    }
}

/// Temporarily closes all ingress while the durable owner checks pending input.
/// A forced close dominates an abandoned claim and is never reopened by Drop.
struct IdleRetirementClaim(std::sync::Arc<AdmissionOwner>);

impl IdleRetirementClaim {
    fn commit(self) -> Result<(), &'static str> {
        let mut state = self.0.state.lock();
        if state.phase != AdmissionPhase::IdleRetirementClaim {
            return Err("actor closed while idle retirement was being checked");
        }
        state.phase = AdmissionPhase::Closed;
        self.0.released.notify_waiters();
        Ok(())
    }
}

impl Drop for IdleRetirementClaim {
    fn drop(&mut self) {
        let mut state = self.0.state.lock();
        if state.phase == AdmissionPhase::IdleRetirementClaim {
            state.phase = AdmissionPhase::Open;
            self.0.released.notify_waiters();
        }
    }
}

/// Protects a synchronous host transaction from cooperative actor retirement.
/// Drop before waking the actor or awaiting dispatched work: accepted actor work
/// has its own completion owner and must never retain this admission lease.
/// Forced process/actor loss is not a confirmed cooperative retirement.
#[must_use]
pub struct ActorAdmissionLease(std::sync::Arc<AdmissionOwner>);

impl Drop for ActorAdmissionLease {
    fn drop(&mut self) {
        let mut state = self.0.state.lock();
        state.transactions -= 1;
        if state.transactions == 0 {
            self.0.released.notify_waiters();
        }
    }
}

impl MailboxAdmission {
    pub(crate) fn hosted_cell(&self) -> &HostedCellSlot {
        &self.1
    }
    fn attach_provider(
        &self,
        observation: crate::ActorRuntimeObservationHandle,
        probe: std::sync::Arc<dyn Fn() -> Result<bool, String> + Send + Sync>,
    ) -> Result<NativeProviderAdmission, &'static str> {
        let mut state = self.0.state.lock();
        if state.phase != AdmissionPhase::Open || state.provider.is_some() {
            return Err("native provider cannot attach to a closed or already attached actor");
        }
        state.provider = Some(NativeProvider {
            state: NativeProviderState::NotStarted,
            probe,
        });
        Ok(NativeProviderAdmission {
            owner: self.0.clone(),
            observation,
        })
    }

    fn claim_idle(&self) -> Result<IdleRetirementClaim, String> {
        let probe = {
            let mut state = self.0.state.lock();
            if state.phase != AdmissionPhase::Open || state.transactions != 0 {
                return Err("actor is closing or has an admitted transaction".into());
            }
            let provider = state
                .provider
                .as_ref()
                .ok_or("native provider has not attached")?;
            if provider.state != NativeProviderState::Idle {
                return Err("native provider is not confirmed idle".into());
            }
            let probe = provider.probe.clone();
            state.phase = AdmissionPhase::IdleRetirementClaim;
            probe
        };
        let claim = IdleRetirementClaim(self.0.clone());
        // Never acquire the Store mutex under the admission mutex. Existing
        // short transactions are refused above; ingress stays fenced here.
        if !probe()? {
            return Err("native provider has pending durable work or lost its binding".into());
        }
        Ok(claim)
    }

    pub(crate) fn close(&self) {
        self.0.state.lock().phase = AdmissionPhase::Closed;
        self.0.released.notify_waiters();
    }

    fn transaction(&self) -> Option<ActorAdmissionLease> {
        let mut state = self.0.state.lock();
        if state.phase != AdmissionPhase::Open {
            return None;
        }
        state.transactions += 1;
        Some(ActorAdmissionLease(std::sync::Arc::clone(&self.0)))
    }

    /// Admission must already be closed. Register the waiter before inspecting
    /// the count so the final lease's drop cannot be lost between them.
    pub(crate) async fn wait_transactions(&self) {
        loop {
            let released = self.0.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            {
                let state = self.0.state.lock();
                debug_assert!(state.phase != AdmissionPhase::Open);
                if state.transactions == 0 {
                    return;
                }
            }
            released.await;
        }
    }

    pub(crate) fn close_with_fence(
        &self,
        address: &RactorRef<KernelMessage>,
    ) -> Result<(), Box<ractor::MessagingErr<KernelMessage>>> {
        self.fence(address, KernelMessage::DrainFence)
    }

    fn fence(
        &self,
        address: &RactorRef<KernelMessage>,
        fence: KernelMessage,
    ) -> Result<(), Box<ractor::MessagingErr<KernelMessage>>> {
        let mut admission = self.0.state.lock();
        address.send_message(fence).map_err(Box::new)?;
        admission.phase = AdmissionPhase::Closed;
        self.0.released.notify_waiters();
        Ok(())
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

impl LocalActorRef {
    pub(crate) fn hosted_cell(&self) -> &HostedCellSlot {
        &self.admission.1
    }

    /// Whether this incarnation is inside a model-visible `haskell` call that
    /// is computing (or still waiting to start) rather than sleeping. Codex
    /// cancels such a call before admitting new input and cannot finish that
    /// exchange until the cell ends, so the host's delivery pump defers
    /// native submission while this holds.
    #[must_use]
    pub fn hosted_cell_computing(&self) -> bool {
        self.admission.1.computing()
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
        self.address
            .send_message(KernelMessage::AbortReplacement {
                reply: reply.into(),
            })
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
    pub(crate) fn fence_replacement(&self) -> Result<(), KernelBehaviorError> {
        self.admission
            .fence(&self.address, KernelMessage::ReplacementFence)
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
        self.address
            .send_message(KernelMessage::ReplaceSpec {
                definition: Box::new(definition),
                reply: reply.into(),
            })
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
        self.address
            .send_message(KernelMessage::Replace {
                definition: Box::new(definition),
                reply: reply.into(),
            })
            .map_err(|_| KernelInvocationFailure::ActorExited(self.identity))?;
        receive.await.map_err(|_| KernelInvocationFailure::Failed { receipts: Vec::new(),
            actor: self.identity,
            detail: "replacement outcome unavailable; inspect retained actor state before further action".into(),
            diagnostic: None,
        })?
    }

    async fn request_drain(&self) -> Result<(), KernelInvocationFailure> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.address
            .send_message(KernelMessage::Drain {
                reply: reply.into(),
            })
            .map_err(|_| KernelInvocationFailure::ActorExited(self.identity))?;
        receive
            .await
            .map_err(|_| KernelInvocationFailure::ActorExited(self.identity))?
            .map_err(|error| KernelInvocationFailure::Rejected {
                receipts: Vec::new(),
                actor: self.identity,
                detail: error.detail,
                diagnostic: error.diagnostic,
            })
    }
    #[must_use]
    pub fn new(address: RactorRef<KernelMessage>, terminal: RetainedActorExit) -> Self {
        Self::new_in_incarnation(address, terminal, crate::Incarnation::FIRST)
    }

    #[must_use]
    pub fn new_in_incarnation(
        address: RactorRef<KernelMessage>,
        terminal: RetainedActorExit,
        incarnation: crate::Incarnation,
    ) -> Self {
        Self::with_admission(address, terminal, incarnation, MailboxAdmission::default())
    }

    pub(crate) fn with_admission(
        address: RactorRef<KernelMessage>,
        terminal: RetainedActorExit,
        incarnation: crate::Incarnation,
        admission: MailboxAdmission,
    ) -> Self {
        Self {
            identity: ActorRef {
                id: crate::ActorId(address.get_id().pid()),
                incarnation,
            },
            address,
            terminal,
            admission,
        }
    }

    pub(crate) fn with_identity_admission(
        address: RactorRef<KernelMessage>,
        terminal: RetainedActorExit,
        identity: ActorRef,
        admission: MailboxAdmission,
    ) -> Self {
        Self {
            identity,
            address,
            terminal,
            admission,
        }
    }

    /// Admission and queue insertion share the close fence. Accepted payloads
    /// belong to the existing Ractor mailbox even while execution is paused.
    pub(crate) fn admit_mailbox(&self, message: KernelMessage) -> Result<(), KernelCallFailure> {
        let admission = self.admission.0.state.lock();
        if admission.phase != AdmissionPhase::Open {
            return Err(KernelCallFailure::MailboxClosed(self.identity));
        }
        self.address
            .send_message(message)
            .map_err(|_| KernelCallFailure::TargetExited(self.identity))
    }

    /// Admit a short synchronous Store/binding transaction. This is not an
    /// execution lease; dispatch must enqueue under the actor's mailbox fence.
    pub fn admit_transaction(&self) -> Result<ActorAdmissionLease, KernelCallFailure> {
        self.admission
            .transaction()
            .ok_or(KernelCallFailure::MailboxClosed(self.identity))
    }

    /// Attach the durable native work probe once for this exact incarnation.
    pub fn attach_native_provider(
        &self,
        observation: crate::ActorRuntimeObservationHandle,
        probe: impl Fn() -> Result<bool, String> + Send + Sync + 'static,
    ) -> Result<NativeProviderAdmission, &'static str> {
        self.admission
            .attach_provider(observation, std::sync::Arc::new(probe))
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
        self.admission
            .claim_idle()
            .map_err(rejected)?
            .commit()
            .map_err(|detail| rejected(detail.into()))?;
        self.retire_by(supervisor, terminal).await
    }

    pub fn cast(&self, sender: ActorRef, request: MailboxValue) -> Result<(), KernelCallFailure> {
        self.admit_mailbox(KernelMessage::Cast { sender, request })
    }

    pub(crate) fn source(&self, delivery: crate::SourceDelivery) -> Result<(), KernelCallFailure> {
        self.admit_mailbox(KernelMessage::Source(delivery))
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
        })?;
        receive
            .await
            .map_err(|_| KernelCallFailure::TargetExited(self.identity))?
    }

    #[must_use]
    pub fn identity(&self) -> ActorRef {
        self.identity
    }

    #[must_use]
    pub fn address(&self) -> &RactorRef<KernelMessage> {
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
        self.address
            .send_message(KernelMessage::ExternalApplicationFailed {
                failure,
                reply: reply.into(),
            })
            .map_err(|_| KernelInvocationFailure::ActorExited(self.identity))?;
        receive
            .await
            .map_err(|_| KernelInvocationFailure::ActorExited(self.identity))
    }

    pub async fn seal_hosted_work(&self) -> Result<crate::HostedWorkSeal, KernelInvocationFailure> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.address
            .send_message(KernelMessage::SealHostedWork {
                reply: reply.into(),
            })
            .map_err(|_| KernelInvocationFailure::ActorExited(self.identity))?;
        receive
            .await
            .map_err(|_| KernelInvocationFailure::ActorExited(self.identity))
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
        self.admission.close();
        let terminal = self.terminal.request_shutdown(terminal);
        let (reply, receive) = tokio::sync::oneshot::channel();
        if self
            .address
            .send_message(KernelMessage::Shutdown {
                terminal,
                reply: reply.into(),
            })
            .is_err()
        {
            // A concurrent bootstrap can finish between recording intent and
            // enqueueing the mailbox operation. Reuse only its published exit.
            return self
                .terminal
                .get()
                .ok_or(KernelInvocationFailure::ActorExited(self.identity));
        }
        match receive.await {
            Ok(terminal) => Ok(terminal),
            // Bootstrap may observe the request before draining its mailbox.
            // Its lifecycle owner still performs and publishes exact cleanup.
            Err(_) => self
                .terminal
                .get()
                .ok_or(KernelInvocationFailure::ActorExited(self.identity)),
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

    fn native_admission(
        probe: impl Fn() -> Result<bool, String> + Send + Sync + 'static,
    ) -> (MailboxAdmission, NativeProviderAdmission) {
        let admission = MailboxAdmission::default();
        let provider = admission
            .attach_provider(
                crate::ActorRuntimeObservationHandle::default(),
                std::sync::Arc::new(probe),
            )
            .unwrap();
        (admission, provider)
    }

    fn succeed_native(provider: &NativeProviderAdmission, turn: &str) {
        provider
            .begin_provider_turn("thread".into(), turn.into())
            .unwrap()
            .succeed();
    }

    #[tokio::test]
    async fn idle_retirement_fences_input_and_rounds_until_claim_is_released() {
        let (admission, provider) = native_admission(|| Ok(true));
        succeed_native(&provider, "first");
        let claim = admission.claim_idle().unwrap();
        assert!(admission.transaction().is_none());
        assert!(matches!(
            provider.begin_provider_turn("thread".into(), "next".into()),
            Err(NativeProviderStartError::RetirementPending)
        ));
        let waiting = provider.wait_for_retirement_decision();
        tokio::pin!(waiting);
        assert!(futures_util::poll!(&mut waiting).is_pending());
        drop(claim);
        assert!(futures_util::poll!(&mut waiting).is_ready());
        assert!(admission.transaction().is_some());
        succeed_native(&provider, "next");
    }

    #[test]
    fn idle_retirement_refuses_admitted_input_and_durable_work_before_wake() {
        let pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let read = pending.clone();
        let (admission, provider) =
            native_admission(move || Ok(!read.load(std::sync::atomic::Ordering::SeqCst)));
        succeed_native(&provider, "first");
        let input = admission.transaction().unwrap();
        assert!(admission.claim_idle().is_err());
        pending.store(true, std::sync::atomic::Ordering::SeqCst);
        drop(input);
        // Durable commit exists, but no wake has reached the provider.
        assert!(admission.claim_idle().is_err());
        assert!(admission.transaction().is_some());
        let round = provider
            .begin_provider_turn("thread".into(), "second".into())
            .unwrap();
        assert!(admission.claim_idle().is_err());
        pending.store(false, std::sync::atomic::Ordering::SeqCst);
        round.succeed();
        admission.claim_idle().unwrap().commit().unwrap();
        assert!(admission.transaction().is_none());
        assert!(matches!(
            provider.begin_provider_turn("thread".into(), "third".into()),
            Err(NativeProviderStartError::Closed)
        ));
    }

    #[test]
    fn idle_retirement_does_not_treat_abandoned_or_failed_rounds_as_idle() {
        let (admission, provider) = native_admission(|| Ok(true));
        assert!(admission.claim_idle().is_err());
        drop(
            provider
                .begin_provider_turn("thread".into(), "first".into())
                .unwrap(),
        );
        assert!(admission.claim_idle().is_err());
        provider
            .begin_provider_turn("thread".into(), "second".into())
            .unwrap()
            .fail(exomonad_model::ProviderFailure::RequestRejected);
        assert!(admission.claim_idle().is_err());
        succeed_native(&provider, "third");
        assert!(admission.claim_idle().is_ok());
    }

    #[tokio::test]
    async fn idle_retirement_forced_close_dominates_claim_rollback() {
        let (admission, provider) = native_admission(|| Ok(true));
        succeed_native(&provider, "first");
        let claim = admission.claim_idle().unwrap();
        let waiting = provider.wait_for_retirement_decision();
        tokio::pin!(waiting);
        assert!(futures_util::poll!(&mut waiting).is_pending());
        admission.close();
        assert!(futures_util::poll!(&mut waiting).is_ready());
        assert!(claim.commit().is_err());
        assert!(admission.transaction().is_none());
        assert!(matches!(
            provider.begin_provider_turn("thread".into(), "second".into()),
            Err(NativeProviderStartError::Closed)
        ));
    }

    #[test]
    fn idle_retirement_probe_failure_reopens_admission() {
        let (admission, provider) = native_admission(|| Err("Store unavailable".into()));
        succeed_native(&provider, "first");
        assert!(admission.claim_idle().is_err());
        assert!(admission.transaction().is_some());
        succeed_native(&provider, "second");
    }

    #[tokio::test]
    async fn admission_close_waits_for_all_transactions_and_rejects_new_ones() {
        let admission = MailboxAdmission::default();
        let first = admission.transaction().unwrap();
        let second = admission.transaction().unwrap();
        admission.close();
        assert!(admission.transaction().is_none());
        let waiting = admission.wait_transactions();
        tokio::pin!(waiting);
        assert!(futures_util::poll!(&mut waiting).is_pending());
        drop(first);
        assert!(futures_util::poll!(&mut waiting).is_pending());
        drop(second);
        assert!(futures_util::poll!(&mut waiting).is_ready());
        // A last drop before waiter registration also completes immediately.
        admission.wait_transactions().await;
    }

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
        uncertain.publication = Some(WorkbenchPublicationOutcome::DurabilityUnconfirmed {
            bindings: vec!["visibleWrite".into()],
            detail: "journal outcome unknown".into(),
        });
        assert!(uncertain
            .to_string()
            .contains("publication durability remains unconfirmed after 3 completed input units"));
        assert_eq!(
            uncertain.publication.as_ref().unwrap().public_bindings(),
            &["visibleWrite".to_string()]
        );
    }
}
