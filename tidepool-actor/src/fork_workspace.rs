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

/// Exact actor/worktree custody shared by the kernel and its host.
/// Before process submission, the last owner settles its binding generation.
/// After submission may have occurred, custody is conservatively retained.
/// Legacy tmux cannot prove exact termination. A service namespace witness
/// alone also cannot establish host HTTP/resident-work quiescence.
pub trait ForkWorkspaceCustody: Send + Sync + 'static {
    /// Observe the first terminal; not-completed must not mean still active.
    fn actor_stopped(&self, terminal: &crate::ActorTerminal);
    /// Irreversibly fence release before process launch may have external effects.
    fn process_may_exist(&self);
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
