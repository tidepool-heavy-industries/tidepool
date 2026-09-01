//! Backend-neutral substrate for durable actor-node delivery and proxying.

#![warn(clippy::unwrap_used, clippy::expect_used)]

mod inbox;
mod proxy;

pub use inbox::{DurableEnvelope, DurableInbox, InboxError};
pub use proxy::{
    accept_proxy, connect_proxy, proxy_stdio, NodeCredential, NodeHandshake, NodeProxyError,
    NODE_PROTOCOL_VERSION,
};

/// One model-visible MCP tool declaration, independent of who serves it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolDeclaration {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}
