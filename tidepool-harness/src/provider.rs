//! Calling-model provider boundary. The harness knows turns and
//! token counts, never providers — segment 60 supplies the two impls
//! (ChatGPT-subscription OAuth, API-key) behind this one trait, passing
//! one shared behavior suite. This is NOT the in-program `Llm` effect
//! (tidepool-handlers); different consumer, different budget accounting.
//!
//! Both impls (`oauth`, `api_key`) route chat calls through `genai` (`http`
//! submodule) rather than hand-rolled request/response JSON, and OAuth's
//! mechanics ride the `openai-auth` crate rather than a hand-rolled PKCE
//! flow — see `oauth`'s module doc for what that crate covers and the R0
//! callback-port constraint it implies.

pub mod api_key;
pub(crate) mod http;
pub mod oauth;
pub(crate) mod paths;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRequest {
    pub messages: Vec<Message>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnResponse {
    pub text: String,
    pub usage: Usage,
    /// The model's reasoning-summary text, when the provider surfaces one
    /// (the ChatGPT Codex backend emits `reasoning_summary_text` deltas at
    /// `summary:"auto"`). `None` for providers/turns without a summary. This
    /// is the "thinking" shown in the observatory, distinct from the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

/// Recorded into the event log / meters per turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// One incremental piece of a streaming turn, pushed to a [`StreamSink`] as the
/// provider reads the model's SSE stream — so the observatory can show tokens
/// (and thinking) as they arrive rather than only when the turn completes.
#[derive(Debug, Clone)]
pub enum StreamDelta {
    /// A chunk of the assistant's answer text.
    Text(String),
    /// A chunk of the reasoning-summary ("thinking") text.
    Reasoning(String),
}

/// Where a streaming provider pushes [`StreamDelta`]s. Unbounded: the consumer
/// (the harness's live-turn reader) drains promptly and never blocks the
/// provider's read loop; a dropped receiver just means no one is watching, and
/// `send` failing is ignored (the turn still completes and is logged).
pub type StreamSink = tokio::sync::mpsc::UnboundedSender<StreamDelta>;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider auth invalid or expired: {0}")]
    Auth(String),
    #[error("provider call failed: {0}")]
    Api(String),
}

/// One turn in, one completion out. If `sink` is `Some`, the provider ALSO
/// pushes [`StreamDelta`]s as it reads the model's stream (token-by-token +
/// thinking); the returned [`TurnResponse`] is still the complete turn either
/// way, so a caller that passes `None` gets the same result buffered.
pub trait ModelProvider: Send + Sync {
    fn complete(
        &self,
        req: TurnRequest,
        sink: Option<StreamSink>,
    ) -> impl std::future::Future<Output = Result<TurnResponse, ProviderError>> + Send;
}

/// Object-safe (`dyn`-compatible) face of [`ModelProvider`]. The `-> impl
/// Future` in `ModelProvider` (RPITIT) is not dyn-compatible, so the harness —
/// which stores its provider behind `Arc<dyn …>` so the engine and every forked
/// answerer share ONE signed-in client — drives this trait instead. Blanket-
/// impl'd for every `ModelProvider` by boxing the future. This is the standard
/// "async trait object" bridge, kept local so the ergonomic `impl Future`
/// surface stays the one providers implement.
pub trait DynModelProvider: Send + Sync {
    fn complete_boxed<'a>(
        &'a self,
        req: TurnRequest,
        sink: Option<StreamSink>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<TurnResponse, ProviderError>> + Send + 'a>,
    >;
}

impl<P: ModelProvider> DynModelProvider for P {
    fn complete_boxed<'a>(
        &'a self,
        req: TurnRequest,
        sink: Option<StreamSink>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<TurnResponse, ProviderError>> + Send + 'a>,
    > {
        Box::pin(self.complete(req, sink))
    }
}

/// So a `dyn DynModelProvider` (what the harness stores) is itself usable
/// wherever a `ModelProvider` is expected.
impl ModelProvider for dyn DynModelProvider + '_ {
    async fn complete(
        &self,
        req: TurnRequest,
        sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        self.complete_boxed(req, sink).await
    }
}
