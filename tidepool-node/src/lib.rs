//! Backend-neutral substrate for durable actor-node delivery and proxying.

#![warn(clippy::unwrap_used, clippy::expect_used)]

mod inbox;
mod proxy;

pub use inbox::{DurableEnvelope, DurableInbox, InboxError};
pub use proxy::{
    accept_proxy, connect_proxy, proxy_stdio, NodeCredential, NodeHandshake, NodeProxyError,
    NODE_PROTOCOL_VERSION,
};
