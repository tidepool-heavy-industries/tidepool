use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_model::{Conversation, Message, Role, TurnRequest, TurnResponse, Usage};
use tidepool_model_output::extract_haskell_blocks;

use crate::{
    ActorEvent, ActorRef, ActorRegistry, ActorRegistryError, ActorRole, ActorTurnKind,
    EventCausality, ModelUsage, StartingActor, TurnLease,
};

/// One actor's accumulating model transcript and legal-boundary queue.
#[derive(Clone)]
pub struct ActorAgentSession {
    registry: ActorRegistry,
    actor: ActorRef,
    state: Arc<Mutex<AgentSessionState>>,
}

pub(crate) struct AgentSessionState {
    conversation: Conversation,
    queued: Vec<Message>,
    next_turn: u64,
}

impl AgentSessionState {
    pub(crate) fn new() -> Self {
        Self {
            conversation: Conversation::default(),
            queued: Vec::new(),
            next_turn: 0,
        }
    }
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
    /// Attach to the one accumulating conversation owned by this exact actor
    /// incarnation. Repeated attachment returns another handle to the same
    /// transcript and legal-boundary queue.
    pub fn attach(registry: ActorRegistry, actor: ActorRef) -> Result<Self, ActorRegistryError> {
        let state = registry.attach_agent_session(actor)?;
        Ok(Self {
            registry,
            actor,
            state,
        })
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
        self.begin_provider_turn_with_lease(max_tokens, lease)
    }

    /// Begin the prompted startup session before readiness publication. The
    /// private capability, rather than an `ActorRef`, authorizes this one
    /// initializing actor's provider turn.
    pub fn begin_startup_provider_turn(
        &self,
        starting: &StartingActor,
        max_tokens: Option<u32>,
    ) -> Result<PendingProviderTurn, ActorRegistryError> {
        let lease = self.registry.begin_startup_provider_turn(starting)?;
        let capability = lease.session_context().actor;
        if capability != self.actor {
            return Err(ActorRegistryError::StartupSessionMismatch {
                session: self.actor,
                capability,
            });
        }
        self.begin_provider_turn_with_lease(max_tokens, lease)
    }

    fn begin_provider_turn_with_lease(
        &self,
        max_tokens: Option<u32>,
        lease: TurnLease,
    ) -> Result<PendingProviderTurn, ActorRegistryError> {
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
                ActorDescriptor::new(
                    "agent",
                    std::iter::empty::<String>(),
                    ActorPlacement {
                        session: tidepool_repr::SessionId(1),
                        resource_scope: RealmId::ROOT,
                        lexical_scope: ScopeId::ROOT,
                    },
                ),
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
        let session = ActorAgentSession::attach(registry.clone(), actor).expect("attach session");
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
        let session = ActorAgentSession::attach(registry.clone(), actor).expect("attach session");
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

    #[test]
    fn repeated_attachment_reuses_the_exact_actors_transcript() {
        let registry = ActorRegistry::new();
        let actor = ready_actor(&registry);
        let first = ActorAgentSession::attach(registry.clone(), actor).expect("first attachment");
        first.queue_user("shared opening");
        let second = ActorAgentSession::attach(registry.clone(), actor).expect("second attachment");

        let pending = second
            .begin_provider_turn(None)
            .expect("shared provider turn");
        assert_eq!(pending.request().messages.len(), 1);
        pending
            .complete(response("shared reply"))
            .expect("complete shared turn");
        assert_eq!(first.transcript(), second.transcript());

        registry
            .finish(
                actor,
                crate::ActorTerminal {
                    kind: crate::ActorExitKind::Completed,
                    summary: "done".into(),
                },
            )
            .expect("finish actor");
        assert!(matches!(
            ActorAgentSession::attach(registry, actor),
            Err(ActorRegistryError::Exited(exited)) if exited == actor
        ));
    }

    #[test]
    fn startup_session_runs_before_readiness_without_publishing_early() {
        let registry = ActorRegistry::new();
        let starting = registry
            .begin_start(
                None,
                ActorDescriptor::new(
                    "starting agent",
                    std::iter::empty::<String>(),
                    ActorPlacement {
                        session: tidepool_repr::SessionId(1),
                        resource_scope: RealmId::fresh(),
                        lexical_scope: ScopeId::ROOT,
                    },
                ),
                StartInitiator::Runtime,
            )
            .expect("begin startup");
        let startup = registry
            .startup_agent_session(&starting)
            .expect("attach startup session");
        startup.queue_user("configure behavior");

        assert!(matches!(
            startup.begin_provider_turn(None),
            Err(ActorRegistryError::Initializing(_))
        ));
        let pending = startup
            .begin_startup_provider_turn(&starting, None)
            .expect("startup capability admits provider turn");
        assert_eq!(pending.request().messages[0].content, "configure behavior");
        pending
            .complete(response("```haskell\ninitialPolicy\n```"))
            .expect("record startup response");

        let actor = registry.publish_ready(starting).expect("publish ready");
        let ready = ActorAgentSession::attach(registry, actor).expect("reattach after readiness");
        assert_eq!(
            ready.transcript().len(),
            2,
            "startup transcript is retained"
        );
    }

    #[test]
    fn a_startup_capability_cannot_drive_another_actors_session() {
        let registry = ActorRegistry::new();
        let first = registry
            .begin_start(
                None,
                ActorDescriptor::new(
                    "first",
                    std::iter::empty::<String>(),
                    ActorPlacement {
                        session: tidepool_repr::SessionId(1),
                        resource_scope: RealmId::fresh(),
                        lexical_scope: ScopeId::ROOT,
                    },
                ),
                StartInitiator::Runtime,
            )
            .expect("begin first startup");
        let second = registry
            .begin_start(
                None,
                ActorDescriptor::new(
                    "second",
                    std::iter::empty::<String>(),
                    ActorPlacement {
                        session: tidepool_repr::SessionId(1),
                        resource_scope: RealmId::fresh(),
                        lexical_scope: ScopeId::ROOT,
                    },
                ),
                StartInitiator::Runtime,
            )
            .expect("begin second startup");
        let second_session = registry
            .startup_agent_session(&second)
            .expect("attach second session");

        assert!(matches!(
            second_session.begin_startup_provider_turn(&first, None),
            Err(ActorRegistryError::StartupSessionMismatch { .. })
        ));
    }
}
