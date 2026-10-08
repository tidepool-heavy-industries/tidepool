//! Workspace attachment admitted for the executing actor principal.
//! The injected service delegates backing and grants to the workspace owner.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tidepool_bridge_effects::WtWorktreeHandle;

use crate::ActorRef;

#[derive(Debug, Clone, tidepool_bridge_derive::FromHaskell)]
pub enum WorkspaceSeedWire {
    CurrentCheckout,
    CommittedSource(tidepool_bridge_effects::WtWorktreeSource),
}

#[derive(Debug, Clone, tidepool_bridge_derive::FromHaskell)]
pub enum SpawnWorkspaceWire {
    SameDirectory,
    ExistingDirectory(tidepool_bridge_effects::WtWorkspaceHandle),
    ForkDirectory(WorkspaceSeedWire),
}

pub type WorkspaceSelection = SpawnWorkspaceWire;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct WorkspaceAdmissionError {
    pub detail: String,
}

/// Exact actor/worktree ownership shared by the kernel and its host.
/// Before process submission, the last owner settles its binding generation.
/// After submission may have occurred, owned resources are conservatively retained.
/// Legacy tmux cannot prove exact termination. A service namespace witness
/// alone also cannot establish host HTTP/resident-work quiescence.
pub trait WorkspaceCustody: std::any::Any + Send + Sync {
    fn transfer_to(
        &self,
        _successor: ActorRef,
    ) -> Result<Arc<dyn WorkspaceCustody>, WorkspaceAdmissionError> {
        Err(WorkspaceAdmissionError {
            detail: "workspace owner does not support custody transfer".into(),
        })
    }
    /// Observe the first terminal; not-completed must not mean still active.
    fn actor_stopped(&self, terminal: &crate::ActorTerminal);
    /// Irreversibly fence release before process launch may have external effects.
    fn process_may_exist(&self);
}

/// Installer invoked once, on the exact successor incarnation, to bind
/// retained host resources and produce the owned handle for that actor.
type WorkspaceInstall = dyn FnOnce(ActorRef) -> Result<Arc<dyn WorkspaceCustody>, WorkspaceAdmissionError>
    + Send
    + Sync;

/// Owned preparation travels into child bootstrap. Its installer may retain
/// host resources that cannot be reconstructed from the model-facing receipt.
/// Installation consumes the preparation and binds it to one exact incarnation.
pub struct PreparedWorkspaceAttachment {
    handle: WtWorktreeHandle,
    install: Box<WorkspaceInstall>,
}

impl PreparedWorkspaceAttachment {
    pub fn new(
        handle: WtWorktreeHandle,
        install: impl FnOnce(ActorRef) -> Result<Arc<dyn WorkspaceCustody>, WorkspaceAdmissionError>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            handle,
            install: Box::new(install),
        }
    }

    pub fn handle(&self) -> &WtWorktreeHandle {
        &self.handle
    }

    pub fn install(
        self,
        actor: ActorRef,
    ) -> Result<Arc<dyn WorkspaceCustody>, WorkspaceAdmissionError> {
        (self.install)(actor)
    }
}

pub type WorkspaceAdmissionFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<PreparedWorkspaceAttachment, WorkspaceAdmissionError>>
            + Send
            + 'a,
    >,
>;

pub trait WorkspaceAdmission: Send + Sync + 'static {
    fn prepare(
        &self,
        _owner: ActorRef,
        _selection: WorkspaceSelection,
        _access: Option<crate::WorkspaceAccess>,
    ) -> WorkspaceAdmissionFuture<'_> {
        Box::pin(async {
            Err(WorkspaceAdmissionError {
                detail: "workspace attachment admission is unavailable".into(),
            })
        })
    }
}

pub(crate) type SharedWorkspaceAdmission = Arc<dyn WorkspaceAdmission>;
