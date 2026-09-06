//! One-way notification commands addressed to the existing deployment owner.
//!
//! These one-shot handoffs are not response obligations or a receipt registry.
//! Rust checks actor authority before handing a command to the host; the host
//! checks the receipt against its exact durable inbox and retained payload owner.
use parking_lot::Mutex;
use tokio::sync::oneshot;

use crate::ActorRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationError {
    Unauthorized,
    Unavailable,
    InvalidReceipt,
    Unconfirmed(String),
    StorageFailure(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationState {
    Accepted,
    Presented,
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
