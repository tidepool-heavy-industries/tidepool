//! Local message protocol and exact handles for the Ractor-owned kernel.
//!
//! This protocol is deliberately process-local and non-serializable. JSON is
//! reserved for real external boundaries; live Haskell roots move directly
//! through these messages under Rust custody.

use ractor::{ActorRef as RactorRef, RpcReplyPort};
use tidepool_repr::SessionId;
use tidepool_runtime::session::{WorkbenchRequest, WorkbenchResponse};

use crate::{
    ActorRef, ActorTerminal, ExternalApplicationFailure, ExternalFailureDisposition, MailboxValue,
    RetainedActorExit,
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
}

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
    Mcp {
        name: String,
        arguments: serde_json::Value,
        reply: RpcReplyPort<KernelInvocationReply>,
    },
    Workbench {
        request: WorkbenchRequest,
        reply: RpcReplyPort<KernelWorkbenchReply>,
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

impl std::fmt::Debug for KernelMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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
            Self::Mcp {
                name, arguments, ..
            } => formatter
                .debug_struct("Mcp")
                .field("name", name)
                .field("arguments", arguments)
                .finish_non_exhaustive(),
            Self::Workbench { request, .. } => formatter
                .debug_struct("Workbench")
                .field("request", request)
                .finish_non_exhaustive(),
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
/// The Ractor PID is unique for the process lifetime. Tidepool currently keeps
/// incarnation `1` in its Haskell-facing address for migration compatibility;
/// no restart or same-lineage substitution semantics are implied.
#[derive(Clone)]
pub struct LocalActorRef {
    identity: ActorRef,
    address: RactorRef<KernelMessage>,
    terminal: RetainedActorExit,
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
    #[must_use]
    pub fn new(address: RactorRef<KernelMessage>, terminal: RetainedActorExit) -> Self {
        Self {
            identity: ActorRef::first(crate::ActorId(address.get_id().pid())),
            address,
            terminal,
        }
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

    pub async fn shutdown(
        &self,
        terminal: ActorTerminal,
    ) -> Result<ActorTerminal, KernelInvocationFailure> {
        if let Some(existing) = self.terminal.get() {
            return Ok(existing);
        }
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.address
            .send_message(KernelMessage::Shutdown {
                terminal,
                reply: reply.into(),
            })
            .map_err(|_| KernelInvocationFailure::ActorExited(self.identity))?;
        receive
            .await
            .map_err(|_| KernelInvocationFailure::ActorExited(self.identity))
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
}
