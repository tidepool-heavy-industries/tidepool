//! Issuer-held compiled reply decoder, retained by one native request draft.

use std::sync::Arc;
use tidepool_runtime::session::RootCustody;

pub(crate) const REPLY_TOOL: &str = "reply";

pub(crate) struct ReplyToolDefinition {
    pub(crate) installation: serde_json::Value,
    pub(crate) installer: Arc<RootCustody>,
}

/// One mounted request's decoder and exact recipient. Cloning preserves the
/// original request even when the actor presents another queued activation.
pub struct RequestReplyActivation {
    actor: crate::ActorRef,
    request: crate::RequestId,
    pub(crate) lease: crate::InstalledToolLease,
    settled: std::sync::atomic::AtomicBool,
}

impl RequestReplyActivation {
    pub(crate) fn new(actor: crate::ActorRef, request: crate::RequestId, lease: crate::InstalledToolLease) -> Self {
        Self { actor, request, lease, settled: false.into() }
    }
    pub fn actor(&self) -> crate::ActorRef { self.actor }
    pub fn request(&self) -> crate::RequestId { self.request }
    pub fn is_settled(&self) -> bool { self.settled.load(std::sync::atomic::Ordering::Acquire) }
    pub(crate) fn mark_settled(&self) { self.settled.store(true, std::sync::atomic::Ordering::Release); }
    pub(crate) fn select(self: &Arc<Self>, name: &str) -> Option<crate::resident_workbench::InstalledToolSelection> {
        let mut selected = self.lease.select(name)?;
        selected.reply_activation = Some(self.clone());
        Some(selected)
    }
    pub(crate) fn declarations(&self) -> Arc<[exomonad_tool::HostedTool]> {
        self.lease.tools().expect("reply activation retains its prepared tools").declaration_snapshot()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedInvocationRole { Domain, Observation, Settlement }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedInvocationState { Queued, Running }

#[derive(Debug, Clone)]
pub struct HostedPendingInvocation {
    pub invocation: exomonad_tool::ToolInvocationContext,
    pub name: String,
    pub state: HostedInvocationState,
}

#[derive(Debug, thiserror::Error)]
pub enum RequestReplyAdmissionError {
    #[error("request-owned operations remain pending: {0:?}; inspect status and use yield before retrying reply")]
    Pending(Vec<HostedPendingInvocation>),
    #[error("reply rejected: {0:?}")]
    Refused(crate::ReplyError),
    #[error("reply admission unavailable: {0}")]
    Unavailable(String),
}
