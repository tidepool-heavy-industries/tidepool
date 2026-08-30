//! Provider-neutral conversation values shared by model backends and actor
//! sessions.

use serde::{Deserialize, Serialize};

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
}
