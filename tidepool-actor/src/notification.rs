//! One-way notification commands addressed to the existing deployment owner.
//!
//! These one-shot handoffs are not response obligations or a receipt registry.
//! Rust checks actor authority before handing a command to the host; the host
//! checks the receipt against its exact durable inbox and retained payload owner.
use parking_lot::Mutex;
use tokio::sync::oneshot;

use crate::ActorRef;

#[derive(Debug, Clone, PartialEq, Eq, tidepool_bridge_derive::ToCore)]
pub enum NotificationError {
    #[core(module = "Tidepool.Effects.Core", name = "NotificationUnauthorized")]
    Unauthorized,
    #[core(module = "Tidepool.Effects.Core", name = "NotificationUnavailable")]
    Unavailable,
    #[core(module = "Tidepool.Effects.Core", name = "NotificationInvalidReceipt")]
    InvalidReceipt,
    #[core(
        module = "Tidepool.Effects.Core",
        name = "NotificationAdmissionUnconfirmed"
    )]
    Unconfirmed(String),
    #[core(module = "Tidepool.Effects.Core", name = "NotificationStorageFailure")]
    StorageFailure(String),
}

#[derive(Debug, Clone, PartialEq, Eq, tidepool_bridge_derive::ToCore)]
pub enum NotificationState {
    #[core(module = "Tidepool.Effects.Core", name = "NotificationAccepted")]
    Accepted,
    #[core(module = "Tidepool.Effects.Core", name = "NotificationPresented")]
    Presented,
    #[core(module = "Tidepool.Effects.Core", name = "NotificationUnconfirmed")]
    Unconfirmed,
}

/// An opaque observation locator, not authority to mutate or retry delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationReceipt {
    pub(crate) owner: ActorRef,
    pub(crate) target: ActorRef,
    pub(crate) inbox: String,
    pub(crate) sequence: u64,
}

impl NotificationReceipt {
    pub fn owner(&self) -> ActorRef {
        self.owner
    }
    pub fn target(&self) -> ActorRef {
        self.target
    }
    pub fn inbox(&self) -> &str {
        &self.inbox
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}

/// Host handoff for admission to one exact target's durable inbox.
pub struct NotificationSend {
    pub(crate) owner: ActorRef,
    pub(crate) target: ActorRef,
    pub(crate) message: String,
    pub(crate) reply:
        Mutex<Option<oneshot::Sender<Result<NotificationReceipt, NotificationError>>>>,
}

impl NotificationSend {
    pub fn owner(&self) -> ActorRef {
        self.owner
    }
    pub fn target(&self) -> ActorRef {
        self.target
    }
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Call only after the existing inbox has durably accepted the payload.
    /// A lost waiter does not undo publication and must not trigger replay.
    pub fn admitted(&self, inbox: String, sequence: u64) {
        if let Some(reply) = self.reply.lock().take() {
            let _ = reply.send(Ok(NotificationReceipt {
                owner: self.owner,
                target: self.target,
                inbox,
                sequence,
            }));
        }
    }

    pub fn rejected(&self, error: NotificationError) {
        if let Some(reply) = self.reply.lock().take() {
            let _ = reply.send(Err(error));
        }
    }
}

/// The host must validate owner, incarnation, inbox identity and retained row.
pub struct NotificationPoll {
    pub(crate) owner: ActorRef,
    pub(crate) receipt: NotificationReceipt,
    pub(crate) reply: Mutex<Option<oneshot::Sender<Result<NotificationState, NotificationError>>>>,
}

impl NotificationPoll {
    pub fn owner(&self) -> ActorRef {
        self.owner
    }
    pub fn receipt(&self) -> &NotificationReceipt {
        &self.receipt
    }
    pub fn observed(&self, result: Result<NotificationState, NotificationError>) {
        if let Some(reply) = self.reply.lock().take() {
            let _ = reply.send(result);
        }
    }
}

pub(crate) type NotificationReceiptWire = ((i64, i64), ((i64, i64), (String, i64)));

impl NotificationReceipt {
    pub(crate) fn from_wire(
        (owner, (target, (inbox, sequence))): NotificationReceiptWire,
    ) -> Result<Self, NotificationError> {
        let address = |(id, incarnation): (i64, i64)| -> Result<ActorRef, NotificationError> {
            if id < 0 || incarnation <= 0 {
                return Err(NotificationError::InvalidReceipt);
            }
            Ok(ActorRef {
                id: crate::ActorId(id as u64),
                incarnation: crate::Incarnation(incarnation as u64),
            })
        };
        if sequence <= 0 || inbox.is_empty() {
            return Err(NotificationError::InvalidReceipt);
        }
        Ok(Self {
            owner: address(owner)?,
            target: address(target)?,
            inbox,
            sequence: sequence as u64,
        })
    }

    pub(crate) fn into_wire(self) -> Result<NotificationReceiptWire, NotificationError> {
        if self.sequence == 0 || self.inbox.is_empty() {
            return Err(NotificationError::InvalidReceipt);
        }
        let integer = |value| i64::try_from(value).map_err(|_| NotificationError::InvalidReceipt);
        Ok((
            (
                integer(self.owner.id.0)?,
                integer(self.owner.incarnation.0)?,
            ),
            (
                (
                    integer(self.target.id.0)?,
                    integer(self.target.incarnation.0)?,
                ),
                (self.inbox, integer(self.sequence)?),
            ),
        ))
    }
}

impl NotificationSend {
    pub(crate) fn new(
        owner: ActorRef,
        target: ActorRef,
        message: String,
    ) -> (
        Self,
        oneshot::Receiver<Result<NotificationReceipt, NotificationError>>,
    ) {
        let (reply, receive) = oneshot::channel();
        (
            Self {
                owner,
                target,
                message,
                reply: Mutex::new(Some(reply)),
            },
            receive,
        )
    }
}

impl NotificationPoll {
    pub(crate) fn new(
        owner: ActorRef,
        receipt: NotificationReceipt,
    ) -> Result<
        (
            Self,
            oneshot::Receiver<Result<NotificationState, NotificationError>>,
        ),
        NotificationError,
    > {
        if receipt.owner != owner {
            return Err(NotificationError::Unauthorized);
        }
        let (reply, receive) = oneshot::channel();
        Ok((
            Self {
                owner,
                receipt,
                reply: Mutex::new(Some(reply)),
            },
            receive,
        ))
    }
}

// Bound the host handoff without retrying a command whose publication may have
// occurred. This is not a provider presentation timeout or a request deadline.
pub(crate) async fn receive_admission(
    receive: oneshot::Receiver<Result<NotificationReceipt, NotificationError>>,
) -> Result<NotificationReceipt, NotificationError> {
    match tokio::time::timeout(std::time::Duration::from_secs(30), receive).await {
        Ok(Ok(result)) => result,
        _ => Err(NotificationError::Unconfirmed(
            "notification host admission result unavailable".into(),
        )),
    }
}

pub(crate) async fn receive_observation(
    receive: oneshot::Receiver<Result<NotificationState, NotificationError>>,
) -> Result<NotificationState, NotificationError> {
    match tokio::time::timeout(std::time::Duration::from_secs(30), receive).await {
        Ok(Ok(result)) => result,
        _ => Err(NotificationError::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(id: u64, incarnation: u64) -> ActorRef {
        ActorRef {
            id: crate::ActorId(id),
            incarnation: crate::Incarnation(incarnation),
        }
    }

    #[tokio::test]
    async fn notification_admission_correlates_once_without_a_response_obligation() {
        let (command, receive) = NotificationSend::new(actor(1, 2), actor(3, 4), "one way".into());
        assert_eq!(command.owner(), actor(1, 2));
        assert_eq!(command.target(), actor(3, 4));
        assert_eq!(command.message(), "one way");
        command.admitted("run/3/4".into(), 7);
        command.rejected(NotificationError::Unavailable);
        let receipt = receive_admission(receive).await.unwrap();
        assert_eq!(
            receipt.clone().into_wire().unwrap(),
            ((1, 2), ((3, 4), ("run/3/4".into(), 7)))
        );
        assert_eq!(
            NotificationReceipt::from_wire(receipt.clone().into_wire().unwrap()).unwrap(),
            receipt
        );
        assert!(matches!(
            NotificationPoll::new(actor(1, 3), receipt.clone()),
            Err(NotificationError::Unauthorized)
        ));
        assert!(matches!(
            NotificationPoll::new(actor(9, 2), receipt.clone()),
            Err(NotificationError::Unauthorized)
        ));
        let (poll, receive) = NotificationPoll::new(actor(1, 2), receipt).unwrap();
        poll.observed(Ok(NotificationState::Accepted));
        poll.observed(Ok(NotificationState::Presented));
        assert_eq!(
            receive_observation(receive).await,
            Ok(NotificationState::Accepted)
        );
    }

    #[tokio::test]
    async fn notification_lost_host_reply_is_uncertain_not_retryable_admission() {
        let (command, receive) =
            NotificationSend::new(actor(1, 1), actor(2, 1), "may be published".into());
        drop(command);
        assert!(matches!(
            receive_admission(receive).await,
            Err(NotificationError::Unconfirmed(_))
        ));
        let (command, receive) =
            NotificationSend::new(actor(1, 1), actor(2, 1), "not published".into());
        command.rejected(NotificationError::Unavailable);
        assert_eq!(
            receive_admission(receive).await,
            Err(NotificationError::Unavailable)
        );
    }

    #[test]
    fn notification_receipt_rejects_malformed_wire_before_host_observation() {
        let good = ((1, 1), (2, 1), "run/2/1".to_owned(), 5);
        assert!(
            NotificationReceipt::from_wire((good.0, (good.1, (good.2.clone(), good.3)))).is_ok()
        );
        for bad in [
            ((-1, 1), good.1, good.2.clone(), good.3),
            ((1, 0), good.1, good.2.clone(), good.3),
            (good.0, (2, -1), good.2.clone(), good.3),
            (good.0, good.1, String::new(), good.3),
            (good.0, good.1, good.2.clone(), 0),
            (good.0, good.1, good.2.clone(), -1),
        ] {
            assert_eq!(
                NotificationReceipt::from_wire((bad.0, (bad.1, (bad.2, bad.3)))),
                Err(NotificationError::InvalidReceipt)
            );
        }
        let too_large = NotificationReceipt {
            owner: actor(u64::MAX, 1),
            target: actor(2, 1),
            inbox: good.2,
            sequence: 5,
        };
        assert_eq!(
            too_large.into_wire(),
            Err(NotificationError::InvalidReceipt)
        );
    }
}
