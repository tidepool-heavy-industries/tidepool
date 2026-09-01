//! Backend-neutral substrate for durable actor-node delivery and proxying.

#![warn(clippy::unwrap_used, clippy::expect_used)]

mod inbox;

pub use inbox::{DurableEnvelope, DurableInbox, InboxError};
