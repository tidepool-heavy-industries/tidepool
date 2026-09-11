//! Local message protocol and exact handles for the Ractor-owned kernel.
//!
//! This protocol is deliberately process-local and non-serializable. JSON is
//! reserved for real external boundaries; live Haskell roots move directly
//! through these messages under Rust custody.

use ractor::{ActorRef as RactorRef, RpcReplyPort};
use tidepool_repr::SessionId;
use tidepool_runtime::session::{WorkbenchRequest, WorkbenchResponse};

use crate::{
    ActorRef, ActorTerminal, ExternalApplicationFailure, ExternalFailureDisposition,
    KernelBehaviorError, MailboxValue, RetainedActorExit,
};

/// The exact synchronous-call path currently occupying a chain of actors.
///
/// A callee extends the path before running its handler. Re-entering any actor
/// already in the path is rejected before request custody changes hands.
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
    #[error("synchronous call to {target:?} would re-enter {ancestry:?}")]
    Cycle {
        target: ActorRef,
        ancestry: Vec<ActorRef>,
    },
    #[error("target actor {0:?} has exited")]
    TargetExited(ActorRef),
    #[error("target actor {0:?} is unavailable")]
    TargetUnavailable(ActorRef),
    #[error("target actor {0:?} has closed mailbox admission")]
    MailboxClosed(ActorRef),
    #[error(
        "call from {caller:?} in session {caller_session} to {target:?} in session {target_session} crosses a machine boundary"
    )]
    MachineBoundary {
        caller: ActorRef,
        caller_session: SessionId,
        target: ActorRef,
        target_session: SessionId,
    },
    #[error("target actor {actor:?} failed while handling the call: {detail}")]
    Handler { actor: ActorRef, detail: String },
}

pub type KernelCallReply = Result<MailboxValue, KernelCallFailure>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KernelInvocationFailure {
    #[error("actor {0:?} has exited")]
    ActorExited(ActorRef),
    #[error("actor {actor:?} rejected the invocation: {detail}")]
    Rejected { actor: ActorRef, detail: String },
    #[error("actor {actor:?} invocation failed: {detail}")]
    Failed { actor: ActorRef, detail: String },
    #[error(transparent)]
    Workbench(#[from] KernelWorkbenchFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelWorkbenchFailure {
    pub actor: ActorRef,
    pub receipts: Vec<tidepool_runtime::session::WorkbenchItemReceipt>,
    pub failed_index: usize,
    pub total: usize,
    pub detail: String,
}

impl std::fmt::Display for KernelWorkbenchFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "actor {:?} workbench input unit {} of {} failed: {}",
            self.actor,
            self.failed_index + 1,
            self.total,
            self.detail
        )?;
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

/// Every ordinary operation serialized through one local actor.
///
/// Actor creation is intentionally absent: the owning actor calls
/// `spawn_linked` and publishes the returned exact handle only after startup.
/// Waiting is also absent: callers await [`RetainedActorExit`] directly. Kill
/// remains a Ractor control signal; `Shutdown` is the cooperative typed-hook
/// path.
pub enum KernelMessage {
    Replace {
        definition: crate::ActorReplacementDefinition,
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
        invocation: tidepool_tool::ToolInvocation,
        reply: RpcReplyPort<KernelInvocationReply>,
    },
    Workbench {
        request: WorkbenchRequest,
        control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
        reply: RpcReplyPort<KernelWorkbenchReply>,
    },
    ReconcileWorkbenchCancellation {
        invocation: Option<tidepool_tool::ToolInvocationContext>,
        execution: tidepool_runtime::session::WorkbenchExecutionId,
        reply: RpcReplyPort<crate::WorkbenchCancellationOutcome>,
    },
    ToolCompleted {
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
        reply: RpcReplyPort<KernelInvocationReply>,
    },
    AbortPendingForks {
        reply: RpcReplyPort<KernelInvocationReply>,
    },
    ReleaseFork {
        scope: tidepool_codegen::scope::ScopeId,
    },
    /// Drain one mailbox request retained while the resident behavior was
    /// parked on an external interaction rather than on `receive`.
    DrainMailbox,
    /// Resume actor-owned work after its initiating caller has been settled.
    Resume,
    ExternalApplicationFailed {
        failure: ExternalApplicationFailure,
        reply: RpcReplyPort<ExternalFailureDisposition>,
    },
    Shutdown {
        terminal: ActorTerminal,
        reply: RpcReplyPort<ActorTerminal>,
    },
}

impl std::fmt::Debug for KernelMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Replace { .. } => formatter.write_str("Replace"),
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
            Self::Workbench { request, .. } => formatter
                .debug_struct("Workbench")
                .field("request", request)
                .finish_non_exhaustive(),
            Self::ReconcileWorkbenchCancellation { execution, .. } => formatter
                .debug_tuple("ReconcileWorkbenchCancellation")
                .field(execution)
                .finish(),
            Self::ToolCompleted { boundary, .. } => formatter
                .debug_tuple("ToolCompleted")
                .field(boundary)
                .finish(),
            Self::AbortPendingForks { .. } => formatter.write_str("AbortPendingForks"),
            Self::ReleaseFork { scope } => {
                formatter.debug_tuple("ReleaseFork").field(scope).finish()
            }
            Self::DrainMailbox => formatter.write_str("DrainMailbox"),
            Self::Resume => formatter.write_str("Resume"),
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

#[derive(Clone, Default)]
pub(crate) struct MailboxAdmission(std::sync::Arc<parking_lot::Mutex<AdmissionState>>);

#[derive(Default)]
enum AdmissionState {
    #[default]
    Open,
    Closed,
}

impl MailboxAdmission {
    pub(crate) fn close(&self) {
        *self.0.lock() = AdmissionState::Closed;
    }

    pub(crate) fn close_with_fence(
        &self,
        address: &RactorRef<KernelMessage>,
    ) -> Result<(), ractor::MessagingErr<KernelMessage>> {
        self.fence(address, KernelMessage::DrainFence)
    }

    fn fence(
        &self,
        address: &RactorRef<KernelMessage>,
        fence: KernelMessage,
    ) -> Result<(), ractor::MessagingErr<KernelMessage>> {
        let mut admission = self.0.lock();
        address.send_message(fence)?;
        *admission = AdmissionState::Closed;
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
    pub(crate) async fn abort_prepared_replacement(&self) -> Result<(), KernelBehaviorError> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.address
            .send_message(KernelMessage::AbortReplacement {
                reply: reply.into(),
            })
            .map_err(|error| KernelBehaviorError {
                detail: format!("prepared actor {:?}: {error}", self.identity),
            })?;
        receive.await.map_err(|_| KernelBehaviorError {
            detail: format!(
                "prepared actor {:?} cleanup outcome unavailable",
                self.identity
            ),
        })?
    }
    pub(crate) fn fence_replacement(&self) -> Result<(), KernelBehaviorError> {
        self.admission
            .fence(&self.address, KernelMessage::ReplacementFence)
            .map_err(|error| KernelBehaviorError {
                detail: error.to_string(),
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

    pub(crate) async fn replace(
        &self,
        definition: crate::ActorReplacementDefinition,
    ) -> Result<Self, KernelInvocationFailure> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.address
            .send_message(KernelMessage::Replace {
                definition,
                reply: reply.into(),
            })
            .map_err(|_| KernelInvocationFailure::ActorExited(self.identity))?;
        receive.await.map_err(|_| KernelInvocationFailure::Failed {
            actor: self.identity,
            detail: "replacement outcome unavailable; inspect retained actor state before further action".into(),
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
                actor: self.identity,
                detail: error.detail,
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

    /// Admission and queue insertion share the close fence. Accepted payloads
    /// belong to the existing Ractor mailbox even while execution is paused.
    fn admit_mailbox(&self, message: KernelMessage) -> Result<(), KernelCallFailure> {
        let admission = self.admission.0.lock();
        if matches!(*admission, AdmissionState::Closed) {
            return Err(KernelCallFailure::MailboxClosed(self.identity));
        }
        self.address
            .send_message(message)
            .map_err(|_| KernelCallFailure::TargetExited(self.identity))
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
                index: 0,
                status: tidepool_runtime::session::WorkbenchItemStatus::Committed,
                output: "defined spotTaskText at generation 2".into(),
                warnings: Vec::new(),
                installed_bindings: vec!["spotTaskText".into()],
                operations: Vec::new(),
                terminal_transfer: None,
            }],
            failed_index: 1,
            total: 3,
            detail: "actor protocol violation: unsupported resident actor request `MissingEffect`"
                .into(),
        };
        assert_eq!(
            failure.to_string(),
            "actor ActorRef { id: ActorId(7), incarnation: Incarnation(1) } workbench input unit 2 of 3 failed: actor protocol violation: unsupported resident actor request `MissingEffect`\ninput receipts before failure:\ninput unit 1 (Committed): defined spotTaskText at generation 2"
        );
    }
}
