//! Portable proposed operator HTTP v1 schema. Copy with serde + serde_json only.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactRequest {
    pub actor: exomonad_actor::ActorRef,
    pub relative_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DisplayExpansionRequest {
    pub identity: (i64, i64, i64),
    pub key: i64,
}

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub protocol_version: u32,
    pub session: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitRequest {
    pub source: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubmitResponse {
    pub blocks: Vec<Block>,
    pub outcome: Outcome,
    pub receipt: Option<Receipt>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
pub enum Block {
    Output(String),
    Diagnostic(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Rejected,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub display: String,
    pub structured: Option<serde_json::Value>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    /// Frozen effect receipts survive invocation or cleanup failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Receipt>,
}
