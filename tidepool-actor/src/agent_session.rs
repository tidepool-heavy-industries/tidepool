use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_model::{Conversation, Message, Role, TurnRequest, TurnResponse, Usage};
use tidepool_model_output::extract_haskell_blocks;

use crate::{
    ActorEvent, ActorRef, ActorRegistry, ActorRegistryError, ActorRole, ActorTurnKind,
    EventCausality, ModelUsage, TurnLease,
};

/// One actor's accumulating model transcript and legal-boundary queue.
#[derive(Clone)]
pub struct ActorAgentSession {
    registry: ActorRegistry,
    actor: ActorRef,
    state: Arc<Mutex<SessionState>>,
}

struct SessionState {
    conversation: Conversation,
    queued: Vec<Message>,
    next_turn: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantTurn {
    pub turn: u64,
    pub reply: String,
    pub blocks: Vec<String>,
    pub usage: Usage,
    pub reasoning: Option<String>,
}

impl ActorAgentSession {
    /// Mount a fresh accumulating conversation on one exact actor incarnation.
    /// Inputs remain explicit: callers queue System, Developer, or User
    /// context before opening the first provider boundary.
    #[must_use]
    pub fn new(registry: ActorRegistry, actor: ActorRef) -> Self {
        Self {
            registry,
            actor,
            state: Arc::new(Mutex::new(SessionState {
                conversation: Conversation::default(),
                queued: Vec::new(),
                next_turn: 0,
            })),
        }
    }

    pub fn queue_system(&self, content: impl Into<String>) {
        self.queue_input(Role::System, content.into());
    }

    pub fn queue_developer(&self, content: impl Into<String>) {
        self.queue_input(Role::Developer, content.into());
    }

    pub fn queue_user(&self, content: impl Into<String>) {
        self.queue_input(Role::User, content.into());
    }

    fn queue_input(&self, role: Role, content: String) {
        self.state.lock().queued.push(Message {
            role,
            content,
            reasoning_items: Vec::new(),
        });
    }

    /// Admit one provider turn and inject all queued inputs immediately before
    /// assembling its request. The returned guard holds the actor's exclusive
    /// turn lease until completion or transport failure.
    pub fn begin_provider_turn(
        &self,
        max_tokens: Option<u32>,
    ) -> Result<PendingProviderTurn, ActorRegistryError> {
        let lease = self
            .registry
            .begin_turn(self.actor, ActorTurnKind::Provider)?;
        let (turn, request, injected) = {
            let mut state = self.state.lock();
            let queued = std::mem::take(&mut state.queued);
            let mut injected = Vec::with_capacity(queued.len());
            for message in queued {
                state.conversation.append(message.clone());
                injected.push(message);
            }
            (
                state.next_turn,
                state.conversation.request(max_tokens),
                injected,
            )
        };
        for message in injected {
            // The actor can reach a terminal state after turn admission. The
            // transcript still audits the request we prepared; returning here
            // drops the lease before any provider request escapes.
            self.registry.record_event(
                self.actor,
                turn_causality(turn),
                ActorEvent::ModelMessage {
                    turn,
                    role: actor_role(message.role),
                    content: message.content,
                    usage: None,
                    reasoning: None,
                    injected: true,
                },
            )?;
        }
        Ok(PendingProviderTurn {
            session: self.clone(),
            request,
            turn,
            _lease: lease,
        })
    }

    #[must_use]
    pub fn transcript(&self) -> Vec<Message> {
        self.state.lock().conversation.messages().to_vec()
    }
}

/// Holds actor admission across one provider request. Dropping it after a
/// transport failure releases the actor turn.
pub struct PendingProviderTurn {
    session: ActorAgentSession,
    request: TurnRequest,
    turn: u64,
    _lease: TurnLease,
}

impl PendingProviderTurn {
    #[must_use]
    pub fn request(&self) -> &TurnRequest {
        &self.request
    }

    /// Append the provider response, release turn admission, and expose every
    /// fenced Haskell block in source order for the resident executor.
    pub fn complete(self, response: TurnResponse) -> Result<AssistantTurn, ActorRegistryError> {
        let reply = response.text;
        let usage = response.usage;
        let reasoning = response.reasoning;
        let message = Message {
            role: Role::Assistant,
            content: reply.clone(),
            reasoning_items: response.reasoning_items,
        };
        {
            let mut state = self.session.state.lock();
            state.conversation.append(message);
            state.next_turn += 1;
        }
        self.session.registry.record_event(
            self.session.actor,
            turn_causality(self.turn),
            ActorEvent::ModelMessage {
                turn: self.turn,
                role: ActorRole::Assistant,
                content: reply.clone(),
                usage: Some(model_usage(usage)),
                reasoning: reasoning.clone(),
                injected: false,
            },
        )?;
        Ok(AssistantTurn {
            turn: self.turn,
            blocks: extract_haskell_blocks(&reply),
            reply,
            usage,
            reasoning,
        })
    }
}

fn actor_role(role: Role) -> ActorRole {
    match role {
        Role::System => ActorRole::System,
        Role::Developer => ActorRole::Developer,
        Role::User => ActorRole::User,
        Role::Assistant => ActorRole::Assistant,
    }
}

fn turn_causality(turn: u64) -> EventCausality {
    EventCausality {
        turn: Some(turn),
        ..EventCausality::default()
    }
}

fn model_usage(usage: Usage) -> ModelUsage {
    ModelUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        cache_write_tokens: usage.cache_write_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorDescriptor, ActorPlacement, StartInitiator};
    use tidepool_codegen::{scope::ScopeId, suspension::RealmId};

    fn ready_actor(registry: &ActorRegistry) -> ActorRef {
        let starting = registry
            .begin_start(
                None,
                ActorDescriptor {
                    label: "agent".into(),
                    effect_stack: vec![],
                    placement: ActorPlacement {
                        session: tidepool_repr::SessionId(1),
                        resource_scope: RealmId::ROOT,
                        lexical_scope: ScopeId::ROOT,
                    },
                },
                StartInitiator::Runtime,
            )
            .expect("begin startup");
        registry.publish_ready(starting).expect("publish actor")
    }

    fn response(text: &str) -> TurnResponse {
        TurnResponse {
            text: text.into(),
            usage: Usage::default(),
            reasoning: None,
            reasoning_items: Vec::new(),
        }
    }

    #[test]
    fn lifecycle_fact_queued_during_inference_enters_after_assistant_response() {
        let registry = ActorRegistry::new();
        let actor = ready_actor(&registry);
        let session = ActorAgentSession::new(registry.clone(), actor);
        session.queue_user("review this");
        let pending = session
            .begin_provider_turn(None)
            .expect("begin provider turn");
        assert_eq!(pending.request().messages.len(), 1);
        session.queue_developer("child exited");
        let assistant = pending
            .complete(response("```haskell\npure ()\n```"))
            .expect("complete provider turn");
        assert_eq!(assistant.blocks, ["pure ()"]);

        let next = session
            .begin_provider_turn(None)
            .expect("begin next provider turn");
        let roles: Vec<_> = next.request().messages.iter().map(|m| m.role).collect();
        assert_eq!(roles, [Role::User, Role::Assistant, Role::Developer]);

        let message_turns: Vec<_> = registry
            .events()
            .into_iter()
            .filter_map(|record| match record.event {
                ActorEvent::ModelMessage { turn, .. } => Some(turn),
                _ => None,
            })
            .collect();
        assert_eq!(message_turns, [0, 0, 1]);
    }

    #[test]
    fn dropped_provider_turn_releases_admission_without_duplicating_input() {
        let registry = ActorRegistry::new();
        let actor = ready_actor(&registry);
        let session = ActorAgentSession::new(registry.clone(), actor);
        session.queue_user("start");
        let pending = session
            .begin_provider_turn(None)
            .expect("begin provider turn");
        drop(pending);
        session
            .begin_provider_turn(None)
            .expect("transport failure released admission");
        let user_events = registry
            .events()
            .into_iter()
            .filter(|record| {
                matches!(
                    record.event,
                    ActorEvent::ModelMessage {
                        role: ActorRole::User,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(user_events, 1, "transport retry must not duplicate input");
    }
}
