//! Provider-neutral conversation values shared by model backends and actor
//! sessions.

use serde::{Deserialize, Serialize};

/// One incremental piece of a streaming model response.
#[derive(Debug, Clone)]
pub enum StreamDelta {
    /// A chunk of the assistant's answer text.
    Text(String),
    /// A chunk of the provider's reasoning summary.
    Reasoning(String),
}

/// Optional destination for streamed response deltas. Dropping the receiver
/// stops observation without cancelling the provider request.
pub type StreamSink = tokio::sync::mpsc::UnboundedSender<StreamDelta>;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("provider auth invalid or expired: {0}")]
    Auth(String),
    #[error("provider call failed: {0}")]
    Api(String),
}

/// A message's actual provider role. Runtime-authored actor context uses
/// [`Role::Developer`], never a synthetic user message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    Developer,
    User,
    Assistant,
}

/// Opaque provider reasoning state replayed in its original position.
///
/// This is backend continuity data, not the human-readable reasoning summary.
/// It remains in memory and is deliberately excluded from durable messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasoningItem(pub serde_json::Value);

/// One provider-visible transcript item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// In-memory provider continuity, deliberately absent from durable message
    /// serialization. Persistence requires an explicit encrypted-state
    /// contract rather than accidentally serializing this field.
    #[serde(skip, default)]
    pub reasoning_items: Vec<ReasoningItem>,
}

/// A complete provider request assembled from an accumulating conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnRequest {
    pub messages: Vec<Message>,
    pub max_tokens: Option<u32>,
}

/// The provider-neutral result of one model response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnResponse {
    pub text: String,
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip, default)]
    pub reasoning_items: Vec<ReasoningItem>,
}

/// One provider-neutral model round. Streaming is observational: the returned
/// response is complete whether or not a sink is supplied.
pub trait ModelProvider: Send + Sync {
    fn complete(
        &self,
        request: TurnRequest,
        sink: Option<StreamSink>,
    ) -> impl std::future::Future<Output = Result<TurnResponse, ProviderError>> + Send;
}

/// Object-safe face of [`ModelProvider`] for long-lived provider ownership.
pub trait DynModelProvider: Send + Sync {
    fn complete_boxed<'a>(
        &'a self,
        request: TurnRequest,
        sink: Option<StreamSink>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<TurnResponse, ProviderError>> + Send + 'a>,
    >;
}

impl<P: ModelProvider> DynModelProvider for P {
    fn complete_boxed<'a>(
        &'a self,
        request: TurnRequest,
        sink: Option<StreamSink>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<TurnResponse, ProviderError>> + Send + 'a>,
    > {
        Box::pin(self.complete(request, sink))
    }
}

impl ModelProvider for dyn DynModelProvider + '_ {
    async fn complete(
        &self,
        request: TurnRequest,
        sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        self.complete_boxed(request, sink).await
    }
}

/// Provider-reported token accounting for one response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `None` means the provider did not report cache reads; it is not a
    /// reported zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    /// `None` means the provider did not report cache writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
}

/// Accumulating provider transcript.
///
/// Turn numbering, provider continuation ids, compaction policy, and actor
/// admission live above this value. A single provider turn may append several
/// input messages and one assistant response, so message position is not a
/// turn identity.
#[derive(Debug, Clone, Default)]
pub struct Conversation {
    messages: Vec<Message>,
}

impl Conversation {
    #[must_use]
    pub fn new(messages: Vec<Message>) -> Self {
        Self { messages }
    }

    pub fn append(&mut self, message: Message) {
        self.messages.push(message);
    }

    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    #[must_use]
    pub fn request(&self, max_tokens: Option<u32>) -> TurnRequest {
        TurnRequest {
            messages: self.messages.clone(),
            max_tokens,
        }
    }

    pub fn replace_with(&mut self, message: Message) {
        self.messages = vec![message];
    }
}

/// Tokens one turn actually consumed, as the backend reported them.
///
/// `Option`-free on purpose once present: a backend that reports usage reports
/// all of it. Callers represent unavailable measurements with `Option`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}

/// One durable provider observation; equal counts need not mean the same response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUsageObservation {
    pub id: String,
    pub timestamp: Option<String>,
    pub usage: TokenUsage,
}

/// Endpoints of the provider thread's completed-response history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUsageSnapshot {
    pub first: ProviderUsageObservation,
    pub latest: ProviderUsageObservation,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_discards_the_old_transcript() {
        let mut conversation = Conversation::default();
        conversation.append(Message {
            role: Role::User,
            content: "start".into(),
            reasoning_items: Vec::new(),
        });
        conversation.replace_with(Message {
            role: Role::Developer,
            content: "compacted".into(),
            reasoning_items: Vec::new(),
        });
        assert_eq!(conversation.messages()[0].content, "compacted");
    }

    fn sample_reasoning_item() -> ReasoningItem {
        ReasoningItem(serde_json::json!({
            "type": "reasoning",
            "id": "rs_1",
            "encrypted_content": "opaque-blob",
        }))
    }

    #[test]
    fn message_serialization_excludes_reasoning_items() {
        let plain = Message {
            role: Role::Assistant,
            content: "hello".to_string(),
            reasoning_items: Vec::new(),
        };
        let with_reasoning = Message {
            reasoning_items: vec![sample_reasoning_item()],
            ..plain.clone()
        };
        let plain_json = serde_json::to_string(&plain).expect("serialize plain message");
        let reasoning_json =
            serde_json::to_string(&with_reasoning).expect("serialize message with reasoning");
        assert_eq!(plain_json, reasoning_json);
        assert!(!plain_json.contains("reasoning_items"));
        assert!(!plain_json.contains("encrypted"));
    }

    #[test]
    fn usage_accepts_the_original_wire_shape() {
        let old = serde_json::json!({"input_tokens": 7, "output_tokens": 2});
        let usage: Usage = serde_json::from_value(old).expect("deserialize original usage");
        assert_eq!(usage.input_tokens, 7);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(usage.cached_input_tokens, None);
        assert_eq!(usage.cache_write_tokens, None);
    }

    #[test]
    fn response_serialization_excludes_reasoning_items() {
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
            serde_json::to_string(&plain).expect("serialize plain response"),
            serde_json::to_string(&with_reasoning).expect("serialize response with reasoning")
        );
    }
}
