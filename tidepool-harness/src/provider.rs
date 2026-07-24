//! Calling-model provider boundary (F4). The harness knows turns and
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
}

/// Recorded into the event log / meters per turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider auth invalid or expired: {0}")]
    Auth(String),
    #[error("provider call failed: {0}")]
    Api(String),
}

/// One turn in, one completion out. Streaming is optional in R0.
pub trait ModelProvider: Send + Sync {
    fn complete(
        &self,
        req: TurnRequest,
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
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<TurnResponse, ProviderError>> + Send + 'a>,
    >;
}

impl<P: ModelProvider> DynModelProvider for P {
    fn complete_boxed<'a>(
        &'a self,
        req: TurnRequest,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<TurnResponse, ProviderError>> + Send + 'a>,
    > {
        Box::pin(self.complete(req))
    }
}

/// So a `dyn DynModelProvider` (what the harness stores) is itself usable
/// wherever a `ModelProvider` is expected.
impl ModelProvider for dyn DynModelProvider + '_ {
    async fn complete(&self, req: TurnRequest) -> Result<TurnResponse, ProviderError> {
        self.complete_boxed(req).await
    }
}
