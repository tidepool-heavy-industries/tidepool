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

/// A lease on exact actor/worktree custody. The implementation releases only
/// its own binding generation when the last runtime/host owner drops it.
/// Keeping the lease alive through host cleanup prevents early rebinding while
/// a provider process can still access the checkout.
pub trait ForkWorkspaceCustody: Send + Sync + 'static {}

pub trait ForkWorkspaceAdmission: Send + Sync + 'static {
    /// Install custody before executing the child entry. This is separate from
    /// provider readiness; implementations must fail closed on stale ownership.
    ///
    /// The default explicitly leaves pre-bootstrap custody unsupported while
    /// composition roots migrate to this contract.
    fn install_custody(
        &self,
        _actor: ActorRef,
        _worktree: &str,
    ) -> Result<Arc<dyn ForkWorkspaceCustody>, ForkWorkspaceAdmissionError> {
        Err(ForkWorkspaceAdmissionError {
            detail: "pre-bootstrap worktree custody is not implemented".into(),
        })
    }

    fn admit(
        &self,
        owner: ActorRef,
        actor_path: &str,
        seed: ForkWorkspaceSeed,
    ) -> Result<WtWorktreeHandle, ForkWorkspaceAdmissionError>;
}

pub(crate) type SharedForkWorkspaceAdmission = Arc<dyn ForkWorkspaceAdmission>;
