//! Reflect projection over one embedded actor's exact Store binding.

use std::sync::Arc;

use exomonad_actor::{
    ActorRef, ConversationReader, ConversationRole as Role, ConversationTurn,
    ConversationTurnState, ConversationUnavailable, TurnItem,
};
use harness::{
    embedding::HostIdentity,
    item::Item,
    model::RequestId,
    store::{EmbeddedConversationHistory, EmbeddedModelResponseState, Store},
};

pub(super) fn run_conversation_reader(
    store: Arc<Store>,
    recovery: Arc<exomonad_actor::ActorRecoveryJournal>,
) -> ConversationReader {
    conversation_reader(
        store,
        Arc::new(move |actor| {
            let conversation = recovery.active_application_conversation(actor)?;
            super::embedded_recovery::identity_from_conversation(&conversation).ok()
        }),
    )
}

pub(super) fn conversation_reader(
    store: Arc<Store>,
    identity_for: Arc<dyn Fn(ActorRef) -> Option<HostIdentity> + Send + Sync>,
) -> ConversationReader {
    Arc::new(move |actor, count| {
        let store = store.clone();
        let identity_for = identity_for.clone();
        Box::pin(async move {
            if count == 0 {
                return Ok(Vec::new());
            }
            let Some(identity) = identity_for(actor) else {
                return Err(ConversationUnavailable::Unbound);
            };
            let history = store
                .embedded_conversation_history(&identity)
                .map_err(unreadable)?;
            Ok(recent_turns(history, count))
        })
    })
}

fn unreadable(error: impl std::fmt::Display) -> ConversationUnavailable {
    ConversationUnavailable::Unreadable(error.to_string())
}

fn recent_turns(history: EmbeddedConversationHistory, limit: usize) -> Vec<ConversationTurn> {
    if limit == 0 {
        return Vec::new();
    }
    let EmbeddedConversationHistory {
        head,
        history,
        mut responses,
    } = history;
    let mut turns = Vec::new();
    let mut add_turn = |request: RequestId| ConversationTurn {
        state: match responses
            .remove(&request)
            .expect("snapshot observed every history request")
        {
            EmbeddedModelResponseState::InProgress => ConversationTurnState::InProgress,
            EmbeddedModelResponseState::Completed { response_id } => {
                ConversationTurnState::Completed {
                    provider_response_id: response_id,
                }
            }
            EmbeddedModelResponseState::Interrupted => ConversationTurnState::Interrupted,
            EmbeddedModelResponseState::Unknown => ConversationTurnState::Unknown,
        },
        turn: request.0,
        started_at: None,
        completed_at: None,
        items: Vec::new(),
    };
    // An admitted model request may not have emitted any items yet.
    if let Some(head) =
        head.filter(|head| history.last().is_none_or(|(request, _, _)| request != head))
    {
        turns.push(add_turn(head));
    }
    for (request, _, item) in history.into_iter().rev() {
        if turns
            .last()
            .is_none_or(|turn: &ConversationTurn| turn.turn != request.0)
        {
            if turns.len() == limit {
                break;
            }
            turns.push(add_turn(request));
        }
        if let Some(item) = project_item(&item) {
            turns
                .last_mut()
                .expect("the request turn was just added")
                .items
                .push(item);
        }
    }
    for turn in &mut turns {
        turn.items.reverse();
    }
    turns.reverse();
    turns
}

fn project_item(item: &Item) -> Option<TurnItem> {
    let value = &item.0;
    match value.get("type")?.as_str()? {
        "message" => Some(TurnItem::Message {
            role: match value.get("role")?.as_str()? {
                "system" => Role::System,
                "developer" => Role::Developer,
                "user" => Role::User,
                "assistant" => Role::Assistant,
                _ => return None,
            },
            text: message_text(value.get("content")?),
        }),
        "function_call" | "custom_tool_call" => Some(TurnItem::ToolCall {
            call: value.get("call_id")?.as_str()?.to_owned(),
            tool: value.get("name")?.as_str()?.to_owned(),
            arguments: rendered(value.get("arguments").or_else(|| value.get("input"))?),
        }),
        "function_call_output" | "custom_tool_call_output" => Some(TurnItem::ToolResult {
            call: value.get("call_id")?.as_str()?.to_owned(),
            output: rendered(value.get("output").or_else(|| value.get("content"))?),
        }),
        _ => None,
    }
}

fn message_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn rendered(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::recent_turns;
    use harness::{
        item::ItemHash,
        model::RequestId,
        store::{EmbeddedConversationHistory, EmbeddedModelResponseState},
    };

    fn snapshot(
        history: Vec<(RequestId, ItemHash, harness::item::Item)>,
    ) -> EmbeddedConversationHistory {
        EmbeddedConversationHistory {
            head: history.last().map(|(request, _, _)| request.clone()),
            responses: history
                .iter()
                .map(|(request, _, _)| (request.clone(), EmbeddedModelResponseState::Unknown))
                .collect(),
            history,
        }
    }
    use serde_json::json;

    fn item(value: serde_json::Value) -> (RequestId, ItemHash, harness::item::Item) {
        (
            RequestId("request".into()),
            ItemHash("hash".into()),
            harness::item::Item(value),
        )
    }

    #[test]
    fn projects_recent_effective_history_in_recorded_order_and_bounds_turns() {
        let history = vec![
            (
                RequestId("older".into()),
                ItemHash("h1".into()),
                harness::item::Item(json!({
                    "type":"message", "role":"user", "content":[{"type":"input_text","text":"old"}]
                })),
            ),
            (
                RequestId("latest".into()),
                ItemHash("h2".into()),
                harness::item::Item(json!({
                    "type":"function_call", "call_id":"call-1", "name":"lookup", "arguments":{"q":"now"}
                })),
            ),
            (
                RequestId("latest".into()),
                ItemHash("h3".into()),
                harness::item::Item(json!({
                    "type":"function_call_output", "call_id":"call-1", "output":"found"
                })),
            ),
        ];

        let latest = recent_turns(snapshot(history.clone()), 1);
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].turn, "latest");
        assert_eq!(latest[0].started_at, None);
        assert_eq!(latest[0].completed_at, None);
        assert_eq!(latest[0].items.len(), 2);
        assert!(matches!(
            &latest[0].items[0],
            exomonad_actor::TurnItem::ToolCall { call, tool, arguments }
                if call == "call-1" && tool == "lookup" && arguments == r#"{"q":"now"}"#
        ));
        assert!(matches!(
            &latest[0].items[1],
            exomonad_actor::TurnItem::ToolResult { call, output }
                if call == "call-1" && output == "found"
        ));

        assert_eq!(recent_turns(snapshot(history), 10).len(), 2);
        assert!(
            recent_turns(snapshot(vec![item(json!({"type":"reasoning"}))]), 10)[0]
                .items
                .is_empty()
        );
    }
}
