//! Affine run-custody transfer reservations.
use super::*;

pub(crate) struct WorkerTransferReservation {
    pub(super) id: u64,
    pub(super) owner: RactorRef<RunControlMessage>,
    pub(super) armed: bool,
}
impl WorkerTransferReservation {
    pub(crate) async fn commit(mut self) -> Result<(), DirectoryError> {
        let (reply, receiver) = oneshot::channel();
        self.owner
            .send_message(RunControlMessage::CommitWorkerTransfer { id: self.id, reply })
            .map_err(|_| DirectoryError::Unavailable)?;
        receiver.await.map_err(|_| DirectoryError::Unavailable)??;
        self.armed = false;
        Ok(())
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
}
impl ResourceTransferReservation {
    pub(crate) async fn commit(mut self) -> Result<(), DirectoryError> {
        let (reply, receiver) = oneshot::channel();
        self.owner
            .send_message(RunControlMessage::CommitResourceTransfer { id: self.id, reply })
            .map_err(|_| DirectoryError::Unavailable)?;
        receiver.await.map_err(|_| DirectoryError::Unavailable)??;
        self.armed = false;
        Ok(())
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
    pub(super) destination_run: bool,
}
pub(super) struct ResourceTransfer {
    pub(super) source: ResourceLifetime,
    pub(super) destination: ResourceLifetime,
    pub(super) resources: Vec<ractor::ActorId>,
}
