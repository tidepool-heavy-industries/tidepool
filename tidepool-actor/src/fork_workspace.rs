//! Narrow workspace reservation used only by atomic context-fork admission.
//!
//! The actor kernel owns the admission transaction, while the injected
//! service delegates Git and custody mechanics to the existing Worktree
//! owner. This capability is intentionally not the general model-facing
//! allocation effect.

use std::sync::Arc;

use tidepool_bridge_effects::{WtDirtyPolicy, WtWorktreeHandle, WtWorktreeSpec};

use crate::ActorRef;

#[derive(Debug, Clone)]
pub enum ForkWorkspaceSeed {
    Explicit(WtWorktreeSpec),
    BoundHead(WtDirtyPolicy),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct ForkWorkspaceAdmissionError {
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustodyRelease {
    Released,
    RetainedByActor,
}

/// A lease on exact actor/worktree custody. The implementation releases only
/// its own binding generation when the last runtime/host owner drops it.
/// Keeping the lease alive through host cleanup prevents early rebinding while
/// a provider process can still access the checkout.
pub trait ForkWorkspaceCustody: Send + Sync + 'static {
    fn actor_stopped(&self, terminal: &crate::ActorTerminal);
    /// Fence release before a process launch may have external effects.
    fn process_may_exist(&self);
    /// Permit release only after the exact process has been reaped.
    fn process_reaped(&self);
    fn release_after_process(
        self: Arc<Self>,
    ) -> Result<CustodyRelease, ForkWorkspaceAdmissionError>;
}

pub trait ForkWorkspaceAdmission: Send + Sync + 'static {
    /// Install custody before executing the child entry. This is separate from
    /// provider readiness; implementations must fail closed on stale ownership.

    fn install_custody(
        &self,
        actor: ActorRef,
        worktree: &str,
    ) -> Result<Arc<dyn ForkWorkspaceCustody>, ForkWorkspaceAdmissionError>;

    fn admit(
        &self,
        owner: ActorRef,
        actor_path: &str,
        seed: ForkWorkspaceSeed,
    ) -> Result<WtWorktreeHandle, ForkWorkspaceAdmissionError>;
}

pub(crate) type SharedForkWorkspaceAdmission = Arc<dyn ForkWorkspaceAdmission>;
