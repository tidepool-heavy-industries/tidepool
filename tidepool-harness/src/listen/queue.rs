//! Operator-listen payload adapter over the shared durable delivery queue.

use std::path::PathBuf;

use tidepool_node::DurableInbox;

use super::Frame;

pub use tidepool_node::InboxError as QueueError;

/// Rendered-text specialization used by the operator listen channel.
pub struct FrameQueue {
    inbox: DurableInbox<String>,
}

impl FrameQueue {
    pub fn open(frames_path: PathBuf, cursor_path: PathBuf) -> Result<Self, QueueError> {
        Ok(Self {
            inbox: DurableInbox::open(frames_path, cursor_path)?,
        })
    }

    pub fn publish(&self, text: &str) -> Result<Frame, QueueError> {
        let envelope = self.inbox.publish(text.to_string())?;
        Ok(Frame {
            seq: envelope.sequence,
            text: envelope.payload,
        })
    }

    pub fn pending(&self) -> Result<Vec<Frame>, QueueError> {
        Ok(self
            .inbox
            .pending()?
            .into_iter()
            .map(|envelope| Frame {
                seq: envelope.sequence,
                text: envelope.payload,
            })
            .collect())
    }

    pub fn cursor(&self) -> Result<u64, QueueError> {
        Ok(self.inbox.cursor())
    }

    pub fn ack(&self, seq: u64) -> Result<(), QueueError> {
        self.inbox.acknowledge(seq)
    }
}
