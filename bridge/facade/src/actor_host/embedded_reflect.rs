//! Reflect projection over one embedded actor's exact Store binding.

use std::sync::Arc;

use exomonad_actor::{
    ActorRef, ConversationReader, ConversationUnavailable, ConversationTurn, Role, TurnItem,
};
use harness::{
    embedding::HostIdentity,
    item::Item,
    model::{ConversationIdentity, RequestId},
    store::Store,
};

const MAX_RECENT_TURNS: usize = 100;

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
            let head = store
                .embedded_agent_head(&identity)
                .map_err(unreadable)?;
            let Some(head) = head else {
                return Ok(Vec::new());
            };
            let origin = ConversationIdentity::Embedded {
                run: identity.run,
                actor: identity.actor,
                incarnation: identity.incarnation,
            };
            let state = store
                .context_request_state(&head, &origin)
                .map_err(unreadable)?;
            Ok(recent_turns(
                state.history,
                count.min(MAX_RECENT_TURNS),
            ))
        })
    })
}

fn unreadable(error: impl std::fmt::Display) -> ConversationUnavailable {
    ConversationUnavailable::Unreadable(error.to_string())
}

fn recent_turns(
    history: Vec<(RequestId, harness::item::ItemHash, Item)>,
    limit: usize,
) -> Vec<ConversationTurn> {
    if limit == 0 {
        return Vec::new();
    }

    let mut turns = Vec::new();
    for (request, _, item) in history.into_iter().rev() {
        if turns.last().is_none_or(|turn: &ConversationTurn| turn.turn != request.0) {
            if turns.len() == limit {
                break;
            }
            turns.push(ConversationTurn {
                turn: request.0,
                started_at: None,
                completed_at: None,
                items: Vec::new(),
            });
        }
        if let Some(item) = project_item(&item) {
            turns.last_mut().expect("the request turn was just added").items.push(item);
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
    use harness::{item::ItemHash, model::RequestId};
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

        let latest = recent_turns(history.clone(), 1);
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

        assert_eq!(recent_turns(history, 10).len(), 2);
        assert!(recent_turns(vec![item(json!({"type":"reasoning"}))], 10)[0]
            .items
            .is_empty());
    }
}
