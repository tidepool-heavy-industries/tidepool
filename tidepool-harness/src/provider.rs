//! Calling-model provider boundary. The harness knows turns and
//! token counts, never providers — two impls (ChatGPT-subscription OAuth,
//! API-key) sit behind this one trait, sharing one behavior suite. This is
//! NOT the in-program `Llm` effect
//! (tidepool-handlers); different consumer, different budget accounting.
//!
//! The API-key impl routes chat calls through `genai` (`http` submodule);
//! the OAuth impl hand-rolls its own Responses-API call instead (see
//! `oauth`'s module doc for why genai can't drive it), though its login
//! mechanics still ride the `openai-auth` crate rather than a hand-rolled
//! PKCE flow.

pub mod api_key;
pub(crate) mod http;
pub mod oauth;
pub(crate) mod paths;
pub mod settings;

pub use tidepool_model::{Message, ReasoningItem, Role, TurnRequest, TurnResponse, Usage};

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
/// Future` in `ModelProvider` (RPITIT) is not dyn-compatible, so callers that
/// need `Arc<dyn ModelProvider>` drive this trait instead. Blanket-impl'd for
/// every `ModelProvider` by boxing the future — the standard "async trait
/// object" bridge, kept local so the ergonomic `impl Future` surface stays the
/// one providers implement.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_reasoning_item() -> ReasoningItem {
        ReasoningItem(serde_json::json!({
            "type": "reasoning",
            "id": "rs_1",
            "encrypted_content": "opaque-blob",
        }))
    }

    /// The wire-format guard the design depends on: `reasoning_items` is
    /// `#[serde(skip)]`, so a `Message`'s serialized form must not depend on
    /// it at all — this is what keeps the encrypted payload out of the
    /// durable log (`Event::TurnDelta` never holds a `Message`, but nothing
    /// stops a FUTURE log event from serializing one, so the field itself
    /// must be silent regardless).
    #[test]
    fn message_serialization_is_unaffected_by_reasoning_items() {
        let plain = Message {
            role: Role::Assistant,
            content: "hello".to_string(),
            reasoning_items: Vec::new(),
        };
        let with_reasoning = Message {
            reasoning_items: vec![sample_reasoning_item()],
            ..plain.clone()
        };
        let plain_json = serde_json::to_string(&plain).unwrap();
        let with_reasoning_json = serde_json::to_string(&with_reasoning).unwrap();
        assert_eq!(
            plain_json, with_reasoning_json,
            "a Message's serialized wire form must not depend on reasoning_items"
        );
        assert!(!plain_json.contains("reasoning_items"));
        assert!(!plain_json.contains("encrypted"));
    }

    /// `cache_write_tokens` is additive + `serde(default)` — a `Usage`
    /// serialized before the field existed (no `cached_input_tokens` either,
    /// matching the oldest on-disk shape) must still deserialize, landing
    /// both new-ish fields as `None`.
    #[test]
    fn usage_deserializes_from_pre_cache_write_tokens_wire_shape() {
        let old = serde_json::json!({"input_tokens": 7, "output_tokens": 2});
        let usage: Usage = serde_json::from_value(old).expect("old Usage shape must deserialize");
        assert_eq!(usage.input_tokens, 7);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(usage.cached_input_tokens, None);
        assert_eq!(usage.cache_write_tokens, None);
    }

    #[test]
    fn turn_response_serialization_is_unaffected_by_reasoning_items() {
        let plain = TurnResponse {
            text: "hi".to_string(),
            usage: Usage::default(),
            reasoning: None,
            reasoning_items: Vec::new(),
        };
        let with_reasoning = TurnResponse {
            reasoning_items: vec![sample_reasoning_item()],
            ..plain.clone()
        };
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            serde_json::to_string(&with_reasoning).unwrap()
        );
    }
}
