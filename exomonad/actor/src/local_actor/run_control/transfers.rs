//! Affine run-custody transfer reservations.
use super::*;

pub(crate) struct WorkerTransferReservation {
    pub(super) id: u64,
    pub(super) owner: RactorRef<RunControlMessage>,
    pub(super) armed: bool,
    pub(super) parent_lease: Option<crate::kernel::lifecycle::ActorAdmissionLease>,
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkerTransferError {
    #[error("worker transfer was not admitted")]
    NotAdmitted,
    #[error("worker transfer settlement is unconfirmed")]
    SettlementUnconfirmed,
    #[error(transparent)]
    Directory(DirectoryError),
    #[error("worker retention publication refused: {0:?}")]
    Publication(crate::resident_actor::AgentRetentionError),
}

impl WorkerTransferReservation {
    pub(crate) fn publish(
        mut self,
        publication: crate::resident_actor::WorkerRetentionPublication,
    ) -> Result<
        futures_util::future::BoxFuture<'static, Result<(), WorkerTransferError>>,
        WorkerTransferError,
    > {
        let (reply, receiver) = oneshot::channel();
        self.owner
            .send_message(RunControlMessage::CommitWorkerTransfer {
                id: self.id,
                publication,
                parent_lease: self.parent_lease.take(),
                reply,
            })
            .map_err(|_| WorkerTransferError::NotAdmitted)?;
        self.armed = false;
        Ok(Box::pin(async move {
            receiver
                .await
                .map_err(|_| WorkerTransferError::SettlementUnconfirmed)?
        }))
    }
}
impl Drop for WorkerTransferReservation {
    fn drop(&mut self) {
        if self.armed {
            let _ = self
                .owner
                .send_message(RunControlMessage::AbortWorkerTransfer { id: self.id });
        }
    }
}
pub(crate) struct ResourceTransferReservation {
    pub(super) id: u64,
    pub(super) owner: RactorRef<RunControlMessage>,
    pub(super) armed: bool,
    pub(super) parent_lease: Option<crate::kernel::lifecycle::ActorAdmissionLease>,
}
impl ResourceTransferReservation {
    pub(crate) fn try_commit(
        mut self,
    ) -> Result<futures_util::future::BoxFuture<'static, Result<(), DirectoryError>>, DirectoryError>
    {
        let (reply, receiver) = oneshot::channel();
        self.owner
            .send_message(RunControlMessage::CommitResourceTransfer { id: self.id, reply })
            .map_err(|_| DirectoryError::Unavailable)?;
        self.armed = false;
        Ok(Box::pin(async move {
            let result = receiver.await.map_err(|_| DirectoryError::Unavailable)?;
            drop(self);
            result
        }))
    }
    pub(crate) fn commit(
        self,
    ) -> futures_util::future::BoxFuture<'static, Result<(), DirectoryError>> {
        match self.try_commit() {
            Ok(receipt) => receipt,
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
}
impl Drop for ResourceTransferReservation {
    fn drop(&mut self) {
        if self.armed {
            let _ = self
                .owner
                .send_message(RunControlMessage::AbortResourceTransfer { id: self.id });
        }
    }
}

pub(super) struct WorkerTransfer {
    pub(super) parent: ActorRef,
    pub(super) child: ActorRef,
    pub(super) source: SpawnOwnership,
    pub(super) destination_run: bool,
}
pub(super) struct ResourceTransfer {
    pub(super) source: ResourceLifetime,
    pub(super) destination: ResourceLifetime,
    pub(super) resources: Vec<ractor::ActorId>,
}
