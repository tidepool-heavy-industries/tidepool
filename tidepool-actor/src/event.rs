use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ActorRef, CallId, MessageId, WaitId};

/// Durable ordering envelope shared by native actor events and compatibility
/// adapters. `stream_sequence` orders one journal; `actor_sequence` orders the
/// exact incarnation independently of unrelated actors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorEventRecord {
    pub stream_sequence: u64,
    pub actor_sequence: u64,
    pub actor: ActorRef,
    #[serde(default, skip_serializing_if = "EventCausality::is_empty")]
    pub causality: EventCausality,
    pub event: ActorEvent,
}

/// Optional identities connecting an event to the work that caused it.
/// Strings are opaque runtime identities, not serialized live values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventCausality {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<ActorRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
}

impl EventCausality {
    pub fn is_empty(&self) -> bool {
        self.owner.is_none()
            && self.turn.is_none()
            && self.block.is_none()
            && self.operation.is_none()
    }
}

/// Neutral facts emitted by the actor kernel and by legacy-event adapters.
/// The JSON values below are boundary payloads or rendered legacy evidence;
/// closures and other live values are represented only by opaque identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ActorEvent {
    Created {
        owner: Option<ActorRef>,
        label: String,
        effect_stack: Vec<String>,
    },
    Started {
        initiator: StartInitiator,
    },
    Ready,
    Exited {
        kind: ActorExitKind,
        summary: String,
        #[serde(default)]
        owner_observing: bool,
    },
    ModelMessage {
        turn: u64,
        role: ActorRole,
        content: String,
        usage: Option<ModelUsage>,
        reasoning: Option<String>,
        injected: bool,
    },
    MailboxAccepted {
        message: MessageId,
        sender: ActorRef,
        kind: MailboxMessageKind,
    },
    MailboxDequeued {
        message: MessageId,
    },
    CallSettled {
        call: CallId,
        disposition: CallDisposition,
    },
    WaitRegistered {
        wait: WaitId,
        waiter: ActorRef,
    },
    WaitSettled {
        wait: WaitId,
        disposition: WaitDisposition,
    },
    ConversationForked {
        parent: ActorRef,
        parent_turn: u64,
    },
    HaskellBatchStarted {
        source: String,
        input: Option<Value>,
    },
    HaskellCompiled {
        asks: Vec<(u64, String)>,
        bound: Option<(String, String)>,
    },
    EffectSettled {
        sequence: u64,
        tag: String,
        request: Value,
        response: Value,
    },
    Suspended {
        suspension: String,
        site: Option<u64>,
        answer_type: Option<String>,
        prompt: String,
        fork: bool,
    },
    SuspensionAnswerAttempt {
        suspension: String,
        source: String,
        disposition: AnswerDisposition,
    },
    Resumed {
        suspension: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartInitiator {
    Operator,
    Policy,
    Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorExitKind {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorRole {
    System,
    Developer,
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxMessageKind {
    Cast,
    Call,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallDisposition {
    Replied,
    TargetExited,
    DeliveryAbandoned,
    CallerExited,
    CallerCancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitDisposition {
    Observed,
    WaiterCancelled,
    WaiterExited,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum AnswerDisposition {
    Consumed,
    Rejected { error: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorId, ActorRef};

    #[test]
    fn event_record_round_trips_without_live_value_encoding() {
        let record = ActorEventRecord {
            stream_sequence: 9,
            actor_sequence: 2,
            actor: ActorRef::first(ActorId(4)),
            causality: EventCausality {
                turn: Some(1),
                block: Some(2),
                ..EventCausality::default()
            },
            event: ActorEvent::HaskellCompiled {
                asks: vec![(7, "ReviewFindings".into())],
                bound: Some(("review".into(), "Candidate -> Review".into())),
            },
        };

        let json = serde_json::to_string(&record).expect("serialize event");
        let decoded = serde_json::from_str(&json).expect("deserialize event");
        assert_eq!(record, decoded);
    }
}
