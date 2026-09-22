//! Recent conversation turns, projected from one durable rollout.
//!
//! The sibling of `rollout_usage.rs`: same file, same turn boundaries
//! (`task_started`/`turn_started` opens, `task_complete`/`turn_complete`
//! closes, `turn_aborted` ends a turn without completing it), different
//! projection — content instead of token counts.
//!
//! One rollout file can hold more than one conversation: a forked or resumed
//! thread inherits its parent's records ahead of its own `session_meta`. Both
//! gates below exist for that reason. Boundary events carry no thread id, so
//! they are admitted only while the most recent `session_meta` names this
//! thread; items carry their own `thread_id` and are matched on it directly.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead};

use exomonad_model::{ConversationTurn, Role, TurnItem};
use serde_json::Value;

#[derive(Default)]
struct Turn {
    started_at: Option<String>,
    completed_at: Option<String>,
    items: Vec<TurnItem>,
    calls: BTreeSet<String>,
}

/// The last `count` recorded turns of `thread`, oldest first, including the
/// active turn. An active turn has no completion timestamp and contains only
/// items already present in the rollout; a pending tool result is never
/// invented. Aborted turns are excluded. `count` of zero returns nothing.
pub(super) fn read_conversation(
    reader: impl BufRead,
    thread: &str,
    count: usize,
) -> io::Result<Vec<ConversationTurn>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut own_thread = false;
    // Turn identifiers in the order the file first mentions them.
    let mut order = Vec::<String>::new();
    let mut turns = BTreeMap::<String, Turn>::new();
    let mut aborted = BTreeSet::<String>::new();
    for line in reader.lines() {
        let line = line?;
        // A torn tail becomes readable on a later read. Skipping it can only
        // withhold turns, never invent one.
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let kind = value.get("type").and_then(Value::as_str);
        let payload = &value["payload"];
        if kind == Some("session_meta") {
            own_thread = payload.get("id").and_then(Value::as_str) == Some(thread);
            continue;
        }
        if kind != Some("event_msg") {
            continue;
        }
        let Some(turn) = payload.get("turn_id").and_then(Value::as_str) else {
            continue;
        };
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let event = payload.get("type").and_then(Value::as_str);
        let item_event = matches!(event, Some("item_started" | "item_completed"));
        // Items carry their own thread id and remain attributable even when a
        // later session_meta in the inherited rollout changed the boundary
        // owner. Boundary events have no thread id, so they use own_thread.
        if (item_event && payload.get("thread_id").and_then(Value::as_str) != Some(thread))
            || (!item_event && !own_thread)
        {
            continue;
        }
        if !turns.contains_key(turn) {
            order.push(turn.to_owned());
        }
        let state = turns.entry(turn.to_owned()).or_default();
        match event {
            Some("task_started" | "turn_started") => state.started_at = timestamp,
            Some("task_complete" | "turn_complete") => {
                state.completed_at = timestamp;
            }
            Some("turn_aborted") => {
                aborted.insert(turn.to_owned());
            }
            Some("item_started" | "item_completed") => {
                if let Some(items) = project_item(
                    &payload["item"],
                    event == Some("item_completed"),
                    &mut state.calls,
                ) {
                    state.items.extend(items);
                }
            }
            _ => {}
        }
    }
    let mut selected: Vec<ConversationTurn> = order
        .into_iter()
        .filter(|turn| !aborted.contains(turn))
        .filter_map(|turn| {
            let state = turns.remove(&turn)?;
            Some(ConversationTurn {
                turn,
                started_at: state.started_at,
                completed_at: state.completed_at,
                items: state.items,
            })
        })
        .collect();
    if selected.len() > count {
        selected.drain(..selected.len() - count);
    }
    Ok(selected)
}

/// One rollout item as conversation items, or nothing when the item carries
/// no conversation content this contract represents.
///
/// A backend record that holds both a call and its result becomes two items
/// sharing one `call` identifier, so the pair survives as a pair.
fn project_item(
    item: &Value,
    completed: bool,
    calls: &mut BTreeSet<String>,
) -> Option<Vec<TurnItem>> {
    match item.get("type").and_then(Value::as_str)? {
        "UserMessage" if completed => Some(vec![TurnItem::Message {
            role: Role::User,
            text: joined_text(item.get("content")?),
        }]),
        "AgentMessage" if completed => Some(vec![TurnItem::Message {
            role: Role::Assistant,
            text: joined_text(item.get("content")?),
        }]),
        "CommandExecution" => {
            let call = item.get("id")?.as_str()?.to_owned();
            let command = item
                .get("command")?
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ");
            let mut projected = Vec::new();
            if calls.insert(call.clone()) {
                projected.push(TurnItem::ToolCall {
                    call: call.clone(),
                    tool: "command".into(),
                    arguments: command,
                });
            }
            if completed {
                projected.push(TurnItem::ToolResult {
                    call,
                    output: item
                        .get("aggregated_output")
                        .or_else(|| item.get("stdout"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                });
            }
            Some(projected)
        }
        "DynamicToolCall" => {
            let call = item.get("id")?.as_str()?.to_owned();
            let arguments = item.get("arguments").map_or_else(String::new, |value| {
                value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned)
            });
            let mut projected = Vec::new();
            if calls.insert(call.clone()) {
                projected.push(TurnItem::ToolCall {
                    call: call.clone(),
                    tool: item.get("tool")?.as_str()?.to_owned(),
                    arguments,
                });
            }
            if completed {
                projected.push(TurnItem::ToolResult {
                    call,
                    output: item
                        .get("content_items")
                        .map_or_else(String::new, joined_text),
                });
            }
            Some(projected)
        }
        _ => None,
    }
}

/// Provider content arrives as a list of parts whose own tags vary by record
/// (`text`, `Text`, `inputText`). The text is the part that matters here, so
/// every part carrying one contributes it.
fn joined_text(content: &Value) -> String {
    content
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source(values: &[Value]) -> String {
        values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn own(thread: &str) -> Value {
        json!({"type":"session_meta","payload":{"id":thread}})
    }

    fn started(turn: &str) -> Value {
        json!({"type":"event_msg","timestamp":"2026-09-17T00:00:00.000Z",
            "payload":{"type":"task_started","turn_id":turn}})
    }

    fn complete(turn: &str) -> Value {
        json!({"type":"event_msg","timestamp":"2026-09-17T00:00:01.000Z",
            "payload":{"type":"task_complete","turn_id":turn}})
    }

    fn user(thread: &str, turn: &str, text: &str) -> Value {
        json!({"type":"event_msg","payload":{"type":"item_completed",
            "thread_id":thread,"turn_id":turn,
            "item":{"type":"UserMessage","id":"u","content":[{"type":"text","text":text}]}}})
    }

    fn agent(thread: &str, turn: &str, text: &str) -> Value {
        json!({"type":"event_msg","payload":{"type":"item_completed",
            "thread_id":thread,"turn_id":turn,
            "item":{"type":"AgentMessage","id":"a","content":[{"type":"Text","text":text}]}}})
    }

    fn tool(thread: &str, turn: &str, name: &str, output: &str) -> Value {
        json!({"type":"event_msg","payload":{"type":"item_completed",
            "thread_id":thread,"turn_id":turn,
            "item":{"type":"DynamicToolCall","id":"call-1","tool":name,
                "arguments":{"query":"doc unfold"},"status":"completed",
                "content_items":[{"type":"inputText","text":output}]}}})
    }

    fn read(values: &[Value], thread: &str, count: usize) -> Vec<ConversationTurn> {
        read_conversation(source(values).as_bytes(), thread, count).unwrap()
    }

    #[test]
    fn latest_turns_arrive_oldest_first_and_include_the_turn_in_progress() {
        let mut lines = vec![own("me")];
        for turn in ["one", "two", "three"] {
            lines.extend([
                started(turn),
                user("me", turn, turn),
                agent("me", turn, "answered"),
                complete(turn),
            ]);
        }
        // The turn the caller is executing right now: opened, never completed.
        lines.extend([started("now"), user("me", "now", "reflect")]);

        let all = read(&lines, "me", 10);
        assert_eq!(
            all.iter().map(|t| t.turn.as_str()).collect::<Vec<_>>(),
            ["one", "two", "three", "now"],
            "the unfinished active turn is included"
        );
        let last_two = read(&lines, "me", 2);
        assert_eq!(
            last_two.iter().map(|t| t.turn.as_str()).collect::<Vec<_>>(),
            ["three", "now"],
            "the last N are the newest N, still oldest first"
        );
        assert_eq!(
            last_two[0].items,
            vec![
                TurnItem::Message {
                    role: Role::User,
                    text: "three".into()
                },
                TurnItem::Message {
                    role: Role::Assistant,
                    text: "answered".into()
                },
            ]
        );
        assert!(read(&lines, "me", 0).is_empty());
        assert_eq!(last_two[1].completed_at, None);
    }

    #[test]
    fn an_aborted_turn_is_not_a_completed_turn() {
        let lines = vec![
            own("me"),
            started("one"),
            user("me", "one", "go"),
            complete("one"),
            started("two"),
            user("me", "two", "stop"),
            json!({"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"two"}}),
        ];
        assert_eq!(
            read(&lines, "me", 10)
                .iter()
                .map(|t| t.turn.as_str())
                .collect::<Vec<_>>(),
            ["one"]
        );
    }

    #[test]
    fn inherited_history_in_one_file_stays_with_the_thread_that_recorded_it() {
        // A forked conversation's rollout replays the parent's records before
        // its own session_meta. Neither thread may read the other's turns.
        let lines = vec![
            own("parent"),
            started("p1"),
            user("parent", "p1", "parent instruction"),
            complete("p1"),
            own("child"),
            started("c1"),
            user("child", "c1", "child instruction"),
            complete("c1"),
        ];
        let child = read(&lines, "child", 10);
        assert_eq!(
            child.iter().map(|t| t.turn.as_str()).collect::<Vec<_>>(),
            ["c1"]
        );
        assert_eq!(
            child[0].items,
            vec![TurnItem::Message {
                role: Role::User,
                text: "child instruction".into()
            }]
        );
        let parent = read(&lines, "parent", 10);
        assert_eq!(
            parent.iter().map(|t| t.turn.as_str()).collect::<Vec<_>>(),
            ["p1"]
        );
        assert_eq!(read(&lines, "stranger", 10), Vec::new());
    }

    #[test]
    fn a_tool_call_and_its_result_stay_a_pair() {
        let lines = vec![
            own("me"),
            started("one"),
            tool(
                "me",
                "one",
                "lookup",
                "doc unfold\n  Use unfold to describe a frontier.",
            ),
            complete("one"),
        ];
        let turn = read(&lines, "me", 1).remove(0);
        assert_eq!(
            turn.items,
            vec![
                TurnItem::ToolCall {
                    call: "call-1".into(),
                    tool: "lookup".into(),
                    arguments: json!({"query":"doc unfold"}).to_string(),
                },
                TurnItem::ToolResult {
                    call: "call-1".into(),
                    output: "doc unfold\n  Use unfold to describe a frontier.".into(),
                },
            ]
        );
        assert_eq!(turn.started_at.as_deref(), Some("2026-09-17T00:00:00.000Z"));
        assert_eq!(
            turn.completed_at.as_deref(),
            Some("2026-09-17T00:00:01.000Z")
        );
    }

    #[test]
    fn active_tool_call_is_visible_without_a_fabricated_result() {
        let lines = vec![
            own("me"),
            started("now"),
            user("me", "now", "inspect it"),
            json!({"type":"event_msg","payload":{"type":"item_started",
                "thread_id":"me","turn_id":"now",
                "item":{"type":"DynamicToolCall","id":"pending","tool":"bash",
                    "arguments":{"cmd":"just quick"}}}}),
        ];
        let turn = read(&lines, "me", 1).remove(0);
        assert_eq!(turn.completed_at, None);
        assert_eq!(
            turn.items,
            vec![
                TurnItem::Message {
                    role: Role::User,
                    text: "inspect it".into()
                },
                TurnItem::ToolCall {
                    call: "pending".into(),
                    tool: "bash".into(),
                    arguments: json!({"cmd":"just quick"}).to_string(),
                },
            ]
        );
    }
}
