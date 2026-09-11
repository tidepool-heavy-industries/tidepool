//! Transport-neutral handle for one resident Haskell tool policy.
//!
//! The policy owns no machine, continuation, scheduler, or host protocol. It
//! exposes an immutable tool surface and submits typed invocations to the
//! owning local actor.

use std::sync::Arc;
use std::{future::Future, pin::Pin};

use tidepool_runtime::session::{ResidentHole, WorkbenchExecutionId, WorkbenchRequest};
use tidepool_tool::{HostedTool, ToolInvocation, ToolInvocationContext};
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

pub struct WorkbenchExecutionControl {
    phase: std::sync::atomic::AtomicU8,
    sleep_outcome: std::sync::atomic::AtomicU8,
    changed: tokio::sync::Notify,
    settlement: tokio::sync::watch::Sender<Option<crate::KernelWorkbenchReply>>,
}

impl WorkbenchExecutionControl {
    pub(crate) fn untracked() -> Arc<Self> {
        Arc::new(Self {
            phase: std::sync::atomic::AtomicU8::new(WORKBENCH_IDLE),
            sleep_outcome: std::sync::atomic::AtomicU8::new(SLEEP_NONE),
            changed: tokio::sync::Notify::new(),
            settlement: tokio::sync::watch::channel(None).0,
        })
    }

    fn new(_execution: WorkbenchExecutionId) -> Arc<Self> {
        Self::untracked()
    }

    pub(crate) fn arm_sleep(&self) {
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
            if self.phase.load(std::sync::atomic::Ordering::Acquire) == WORKBENCH_CANCEL_REQUESTED {
                return;
            }
            self.changed.notified().await;
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
        let _ = self.phase.compare_exchange(
            WORKBENCH_EXPIRED,
            WORKBENCH_IDLE,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
    }

    pub(crate) fn settle(&self, reply: crate::KernelWorkbenchReply) {
        self.settlement.send_replace(Some(reply));
        self.changed.notify_waiters();
    }

    pub(crate) fn request_cancellation(&self) -> bool {
        let claimed = self
            .phase
            .compare_exchange(
                WORKBENCH_SLEEPING,
                WORKBENCH_CANCEL_REQUESTED,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok();
        if claimed {
            self.changed.notify_waiters();
        }
        claimed
    }

    pub(crate) fn cancellation_requested(&self) -> bool {
        self.phase.load(std::sync::atomic::Ordering::Acquire) == WORKBENCH_CANCEL_REQUESTED
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

    fn terminal_reply(&self) -> Option<crate::KernelWorkbenchReply> {
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

/// A policy waiting for its next invocation.
pub(crate) struct ResidentToolAwait {
    pub(crate) continuation: ResidentHole,
    pub(crate) declarations: Vec<tidepool_tool::ToolDeclaration>,
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
    /// Settle unacknowledged forks when a hosted connection reattaches.
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
    active_workbench:
        Arc<parking_lot::Mutex<Option<(WorkbenchExecutionId, Arc<WorkbenchExecutionControl>)>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct WorkbenchCallKey {
    thread_id: String,
    turn_id: String,
    call_id: String,
    namespace: Option<String>,
}

impl From<ToolInvocationContext> for WorkbenchCallKey {
    fn from(context: ToolInvocationContext) -> Self {
        Self {
            thread_id: context.thread_id,
            turn_id: context.turn_id,
            call_id: context.call_id,
            namespace: context.namespace,
        }
    }
}

fn execution_id(actor: crate::ActorRef, operation: &WorkbenchCallKey) -> WorkbenchExecutionId {
    fn field(hasher: &mut blake3::Hasher, value: &[u8]) {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value);
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tidepool.workbench-execution.v1\0");
    hasher.update(&actor.id.0.to_le_bytes());
    hasher.update(&actor.incarnation.0.to_le_bytes());
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
            active_workbench: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    #[cfg(test)]
    pub(crate) async fn wait_until_sleeping(&self, invocation: &ToolInvocationContext) {
        let execution = execution_id(self.actor.identity(), &invocation.clone().into());
        loop {
            let sleeping = self
                .active_workbench
                .lock()
                .as_ref()
                .filter(|(active, _)| active == &execution)
                .is_some_and(|(_, control)| {
                    control.phase.load(std::sync::atomic::Ordering::Acquire) == WORKBENCH_SLEEPING
                });
            if sleeping {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    pub(crate) async fn cancel_workbench(
        &self,
        invocation: ToolInvocationContext,
    ) -> Result<WorkbenchCancellationOutcome, ResidentToolError> {
        let execution = execution_id(self.actor.identity(), &invocation.into());
        let control = self
            .active_workbench
            .lock()
            .as_ref()
            .filter(|(active, _)| active == &execution)
            .map(|(_, control)| Arc::clone(control));
        let Some(control) = control else {
            let (reply, receive) = oneshot::channel();
            self.actor
                .address()
                .send_message(crate::KernelMessage::ReconcileWorkbenchCancellation {
                    execution,
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
        let claimed = control.request_cancellation();
        let phase = control.phase.load(std::sync::atomic::Ordering::Acquire);
        if !claimed
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
            .address()
            .send_message(crate::KernelMessage::Tool {
                invocation,
                reply: response.into(),
            })
            .map_err(|_| ResidentToolError::Unavailable("the owning actor has stopped".into()))?;
        receive
            .await
            .map_err(|_| {
                ResidentToolError::Unavailable(
                    "the actor stopped before settling the invocation".into(),
                )
            })?
            .map_err(ResidentToolError::Invocation)
    }

    pub(crate) async fn reattach(&self) -> Result<serde_json::Value, ResidentToolError> {
        let (reply, receive) = oneshot::channel();
        self.actor
            .address()
            .send_message(crate::KernelMessage::AbortPendingForks {
                reply: reply.into(),
            })
            .map_err(|_| ResidentToolError::Unavailable("the owning actor has stopped".into()))?;
        receive
            .await
            .map_err(|_| {
                ResidentToolError::Unavailable("actor stopped during reattachment".into())
            })?
            .map_err(ResidentToolError::Invocation)
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

    pub(crate) async fn dispatch_workbench(
        &self,
        mut request: WorkbenchRequest,
        invocation: Option<ToolInvocationContext>,
    ) -> Result<serde_json::Value, ResidentToolError> {
        let _turn = self.dispatch_gate.lock().await;
        if let Some(invocation) = invocation {
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
            let control = WorkbenchExecutionControl::new(execution.clone());
            *self.active_workbench.lock() = Some((execution, Arc::clone(&control)));
            return self.dispatch_registered_workbench(request, control).await;
        }
        let execution = WorkbenchExecutionId::from_digest([0; 16]);
        let control = WorkbenchExecutionControl::new(execution);
        self.dispatch_registered_workbench(request, control).await
    }

    async fn dispatch_registered_workbench(
        &self,
        request: WorkbenchRequest,
        control: Arc<WorkbenchExecutionControl>,
    ) -> Result<serde_json::Value, ResidentToolError> {
        let (response, receive) = oneshot::channel();
        if self
            .actor
            .address()
            .send_message(crate::KernelMessage::Workbench {
                request,
                control: Some(Arc::clone(&control)),
                reply: response.into(),
            })
            .is_err()
        {
            let error = crate::KernelInvocationFailure::ActorExited(self.actor.identity());
            control.settle(Err(error.clone()));
            self.active_workbench.lock().take();
            return Err(ResidentToolError::Invocation(error));
        }
        let reply = receive.await.map_err(|_| {
            ResidentToolError::Unavailable(
                "the actor stopped before settling the workbench invocation".into(),
            )
        })?;
        control.settle(reply.clone());
        self.active_workbench.lock().take();
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
            items: Vec::new(),
            next_index: 0,
            total: 1,
        })
    }

    #[tokio::test]
    async fn exact_workbench_control_linearizes_cancellation_and_settlement() {
        let execution = WorkbenchExecutionId::from_digest([7; 16]);
        let control = WorkbenchExecutionControl::new(execution);
        control.arm_sleep();
        assert!(control.request_cancellation());
        assert!(!control.request_cancellation());
        control.acknowledge_cancellation();
        control.settle(terminal_reply());
        assert_eq!(
            control.phase.load(std::sync::atomic::Ordering::Acquire),
            WORKBENCH_CANCELLED
        );
        assert!(control.settled().await.is_ok());
    }

    #[test]
    fn exact_workbench_control_gives_expiry_one_winner() {
        let execution = WorkbenchExecutionId::from_digest([8; 16]);
        let control = WorkbenchExecutionControl::new(execution);
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
    fn exact_workbench_control_never_turns_an_unproved_abort_into_cancellation() {
        let execution = WorkbenchExecutionId::from_digest([9; 16]);
        let control = WorkbenchExecutionControl::new(execution.clone());
        control.arm_sleep();
        assert!(control.request_cancellation());
        assert!(matches!(
            control.cancellation_outcome(execution, terminal_reply()),
            WorkbenchCancellationOutcome::Unconfirmed { .. }
        ));
    }
}
