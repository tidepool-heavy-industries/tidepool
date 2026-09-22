//! Reading an actor's own conversation.
//!
//! The kernel knows that an actor may have a conversation and what a turn of
//! one looks like; it does not know that a coding backend exists, where a
//! conversation is recorded, or how to find it. The host installs a reader
//! that closes over those, and the kernel calls it with nothing but the
//! executing actor's identity — so there is no argument through which a
//! caller could name someone else's conversation.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tidepool_bridge_effects::{RfConversationTurn, RfError, RfRole, RfTurnItem};
use tidepool_model::{ConversationTurn, Role, TurnItem};

/// Why an actor's own conversation could not be returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationUnavailable {
    /// This actor has no conversation bound to it. An operator proxy and a
    /// context with no backend application are both this. No other actor's
    /// conversation stands in for a missing one.
    Unbound,
    /// A conversation is bound, and reading it failed.
    Unreadable(String),
}

pub type ConversationFuture =
    Pin<Box<dyn Future<Output = Result<Vec<ConversationTurn>, ConversationUnavailable>> + Send>>;

/// Installed once when constructing a forest. Called with the actor whose turn
/// is executing and how many latest turns it asked for, including that turn.
pub type ConversationReader =
    Arc<dyn Fn(crate::ActorRef, usize) -> ConversationFuture + Send + Sync>;

/// The answer a caller receives, as the wire types its Haskell row declares.
pub fn reflection(
    outcome: Result<Vec<ConversationTurn>, ConversationUnavailable>,
) -> Result<Vec<RfConversationTurn>, RfError> {
    match outcome {
        Ok(turns) => Ok(turns.into_iter().map(turn).collect()),
        Err(ConversationUnavailable::Unbound) => Err(RfError::ReflectUnbound),
        Err(ConversationUnavailable::Unreadable(detail)) => Err(RfError::ReflectUnreadable(detail)),
    }
}

fn turn(turn: ConversationTurn) -> RfConversationTurn {
    RfConversationTurn {
        identity: turn.turn,
        started_at: turn.started_at,
        completed_at: turn.completed_at,
        items: turn.items.into_iter().map(item).collect(),
    }
}

fn item(item: TurnItem) -> RfTurnItem {
    match item {
        TurnItem::Message { role, text } => RfTurnItem::TurnMessage(
            match role {
                Role::System => RfRole::RoleSystem,
                Role::Developer => RfRole::RoleDeveloper,
                Role::User => RfRole::RoleUser,
                Role::Assistant => RfRole::RoleAssistant,
            },
            text,
        ),
        TurnItem::ToolCall {
            call,
            tool,
            arguments,
        } => RfTurnItem::TurnToolCall(call, tool, arguments),
        TurnItem::ToolResult { call, output } => RfTurnItem::TurnToolResult(call, output),
    }
}

/// How many latest turns a request for `count` turns actually asks for.
///
/// A count at or below zero asks for none, and nothing is read.
#[must_use]
pub fn requested(count: i64) -> usize {
    usize::try_from(count).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nonpositive_count_asks_for_no_turns() {
        assert_eq!(requested(0), 0);
        assert_eq!(requested(-1), 0);
        assert_eq!(requested(i64::MIN), 0);
        assert_eq!(requested(3), 3);
    }

    #[test]
    fn an_unbound_context_reports_itself_rather_than_borrowing_a_conversation() {
        assert_eq!(
            reflection(Err(ConversationUnavailable::Unbound)),
            Err(RfError::ReflectUnbound)
        );
        assert_eq!(
            reflection(Err(ConversationUnavailable::Unreadable("torn".into()))),
            Err(RfError::ReflectUnreadable("torn".into()))
        );
    }

    #[test]
    fn a_turn_crosses_with_its_items_in_order() {
        let crossed = reflection(Ok(vec![ConversationTurn {
            turn: "t1".into(),
            started_at: Some("2026-09-17T00:00:00Z".into()),
            completed_at: None,
            items: vec![
                TurnItem::Message {
                    role: Role::User,
                    text: "do the thing".into(),
                },
                TurnItem::ToolCall {
                    call: "c1".into(),
                    tool: "lookup".into(),
                    arguments: "{}".into(),
                },
                TurnItem::ToolResult {
                    call: "c1".into(),
                    output: "found".into(),
                },
            ],
        }]))
        .unwrap();
        assert_eq!(
            crossed,
            vec![RfConversationTurn {
                identity: "t1".into(),
                started_at: Some("2026-09-17T00:00:00Z".into()),
                completed_at: None,
                items: vec![
                    RfTurnItem::TurnMessage(RfRole::RoleUser, "do the thing".into()),
                    RfTurnItem::TurnToolCall("c1".into(), "lookup".into(), "{}".into()),
                    RfTurnItem::TurnToolResult("c1".into(), "found".into()),
                ],
            }]
        );
    }
}
