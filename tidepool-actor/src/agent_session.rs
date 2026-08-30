use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_model::{
    Conversation, DynModelProvider, Message, ProviderError, Role, StreamSink, TurnRequest,
    TurnResponse, Usage,
};
use tidepool_model_output::extract_haskell_blocks;

use crate::mount::{install_actor_context, ActorRunTarget};
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

#[derive(Debug, thiserror::Error)]
pub enum AgentSessionError {
    #[error(transparent)]
    Registry(#[from] ActorRegistryError),
    #[error(transparent)]
    Provider(#[from] ProviderError),
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

    /// Admit one complete, possibly multi-round model/Haskell interaction.
    /// The returned lease remains exclusive across provider retries,
    /// fenced-Haskell execution, and corrective rounds.
    pub fn begin_agent_session(&self) -> Result<AdmittedAgentSession, ActorRegistryError> {
        let lease = self
            .registry
            .begin_turn(self.actor, ActorTurnKind::AgentSession)?;
        Ok(AdmittedAgentSession {
            session: self.clone(),
            lease,
        })
    }

    /// Admit the prompted startup session before readiness publication. The
    /// private capability, rather than an `ActorRef`, authorizes this one
    /// initializing actor's entire model/Haskell interaction.
    pub fn begin_startup_agent_session(
        &self,
        starting: &StartingActor,
    ) -> Result<AdmittedAgentSession, ActorRegistryError> {
        let lease = self.registry.begin_startup_agent_session(starting)?;
        let capability = lease.session_context().actor;
        if capability != self.actor {
            return Err(ActorRegistryError::StartupSessionMismatch {
                session: self.actor,
                capability,
            });
        }
        Ok(AdmittedAgentSession {
            session: self.clone(),
            lease,
        })
    }

    #[must_use]
    pub fn transcript(&self) -> Vec<Message> {
        self.state.lock().conversation.messages().to_vec()
    }
}

/// Exclusive admission for one complete agent session. Provider calls borrow
/// this guard; dropping an individual call never releases actor admission.
pub struct AdmittedAgentSession {
    session: ActorAgentSession,
    lease: TurnLease,
}

impl AdmittedAgentSession {
    /// Install this actor's exact execution context on a checked-out resident
    /// target without acquiring a second actor turn. Fenced Haskell executed
    /// through the target remains part of this admitted agent session.
    pub fn install_execution<Target>(&self, target: &mut Target) -> Result<(), Target::Error>
    where
        Target: ActorRunTarget,
    {
        let context = self.lease.session_context();
        install_actor_context(target, &context)
    }

    /// Inject queued inputs at a legal provider boundary and assemble one
    /// request inside this admitted session. A transport failure consumes only
    /// the pending round; the caller may retry without releasing admission or
    /// duplicating already-injected input.
    pub fn begin_provider_round(
        &mut self,
        max_tokens: Option<u32>,
    ) -> Result<PendingProviderRound<'_>, ActorRegistryError> {
        let (turn, request, injected) = {
            let mut state = self.session.state.lock();
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
            // prevents a provider request from escaping this admitted session.
            self.session.registry.record_event(
                self.session.actor,
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
        Ok(PendingProviderRound {
            admitted: self,
            request,
            turn,
        })
    }

    /// Run one provider round inside this admitted session. Transport failure
    /// leaves the enclosing admission and transcript available for an exact
    /// retry; only a completed response is appended as an assistant message.
    pub async fn run_provider_round(
        &mut self,
        provider: &dyn DynModelProvider,
        max_tokens: Option<u32>,
        sink: Option<StreamSink>,
    ) -> Result<AssistantTurn, AgentSessionError> {
        let pending = self.begin_provider_round(max_tokens)?;
        let response = provider
            .complete_boxed(pending.request().clone(), sink)
            .await?;
        Ok(pending.complete(response)?)
    }
}

/// One provider request borrowed from an admitted agent session.
pub struct PendingProviderRound<'a> {
    admitted: &'a mut AdmittedAgentSession,
    request: TurnRequest,
    turn: u64,
}

impl PendingProviderRound<'_> {
    #[must_use]
    pub fn request(&self) -> &TurnRequest {
        &self.request
    }

    /// Append the provider response and expose every fenced Haskell block in
    /// source order. The enclosing agent-session admission remains held.
    pub fn complete(self, response: TurnResponse) -> Result<AssistantTurn, ActorRegistryError> {
        let session = &self.admitted.session;
        let reply = response.text;
        let usage = response.usage;
        let reasoning = response.reasoning;
        let message = Message {
            role: Role::Assistant,
            content: reply.clone(),
            reasoning_items: response.reasoning_items,
        };
        {
            let mut state = session.state.lock();
            state.conversation.append(message);
            state.next_turn += 1;
        }
        session.registry.record_event(
            session.actor,
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
    use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
    use tidepool_model::{ModelProvider, ProviderError, StreamSink, TurnRequest};
    use tidepool_runtime::session::SessionRunContext;

    #[derive(Default)]
    struct RecordingTarget {
        context: Option<SessionRunContext>,
    }

    struct FailsOnce {
        attempts: std::sync::atomic::AtomicUsize,
    }

    impl ModelProvider for FailsOnce {
        async fn complete(
            &self,
            _request: TurnRequest,
            _sink: Option<StreamSink>,
        ) -> Result<TurnResponse, ProviderError> {
            if self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                == 0
            {
                Err(ProviderError::Api("temporary".into()))
            } else {
                Ok(response("```haskell\npure ()\n```"))
            }
        }
    }

    impl ActorRunTarget for RecordingTarget {
        type Error = std::convert::Infallible;

        fn install_actor_execution(
            &mut self,
            context: SessionRunContext,
            _effect_policy: EffectRunPolicy,
            _live_payload: LivePayloadPolicy,
        ) -> Result<(), Self::Error> {
            self.context = Some(context);
            Ok(())
        }
    }

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
        let mut admitted = session.begin_agent_session().expect("admit agent session");
        let pending = admitted
            .begin_provider_round(None)
            .expect("begin provider round");
        assert_eq!(pending.request().messages.len(), 1);
        session.queue_developer("child exited");
        let assistant = pending
            .complete(response("```haskell\npure ()\n```"))
            .expect("complete provider turn");
        assert_eq!(assistant.blocks, ["pure ()"]);

        let next = admitted
            .begin_provider_round(None)
            .expect("begin next provider round");
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
    fn dropped_provider_round_keeps_session_admission_without_duplicating_input() {
        let registry = ActorRegistry::new();
        let actor = ready_actor(&registry);
        let session = ActorAgentSession::attach(registry.clone(), actor).expect("attach session");
        session.queue_user("start");
        let mut admitted = session.begin_agent_session().expect("admit agent session");
        let pending = admitted
            .begin_provider_round(None)
            .expect("begin provider round");
        drop(pending);
        assert!(matches!(
            registry.begin_turn(actor, ActorTurnKind::Haskell),
            Err(ActorRegistryError::Busy {
                active: ActorTurnKind::AgentSession,
                ..
            })
        ));
        let retry = admitted
            .begin_provider_round(None)
            .expect("transport failure leaves the session retryable");
        assert_eq!(retry.request().messages.len(), 1);
        drop(retry);
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
        drop(admitted);
        registry
            .begin_turn(actor, ActorTurnKind::Haskell)
            .expect("dropping the admitted session releases actor admission");
    }

    #[test]
    fn fenced_execution_uses_the_agent_sessions_existing_admission() {
        let registry = ActorRegistry::new();
        let actor = ready_actor(&registry);
        let session = ActorAgentSession::attach(registry.clone(), actor).expect("attach session");
        let admitted = session.begin_agent_session().expect("admit agent session");
        let mut target = RecordingTarget::default();

        admitted
            .install_execution(&mut target)
            .expect("install exact actor execution context");

        let expected = registry
            .session_context(actor)
            .expect("actor context")
            .run_context();
        assert_eq!(target.context, Some(expected));
        assert!(matches!(
            registry.begin_turn(actor, ActorTurnKind::Haskell),
            Err(ActorRegistryError::Busy {
                active: ActorTurnKind::AgentSession,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn provider_transport_retry_stays_in_one_admitted_session() {
        let registry = ActorRegistry::new();
        let actor = ready_actor(&registry);
        let session = ActorAgentSession::attach(registry.clone(), actor).expect("attach session");
        session.queue_user("start");
        let mut admitted = session.begin_agent_session().expect("admit agent session");
        let provider = FailsOnce {
            attempts: std::sync::atomic::AtomicUsize::new(0),
        };

        assert!(matches!(
            admitted.run_provider_round(&provider, None, None).await,
            Err(AgentSessionError::Provider(ProviderError::Api(_)))
        ));
        assert!(matches!(
            registry.begin_turn(actor, ActorTurnKind::Mailbox),
            Err(ActorRegistryError::Busy {
                active: ActorTurnKind::AgentSession,
                ..
            })
        ));

        let assistant = admitted
            .run_provider_round(&provider, None, None)
            .await
            .expect("retry provider round");
        assert_eq!(assistant.blocks, ["pure ()"]);
        assert_eq!(session.transcript().len(), 2);
    }

    #[test]
    fn repeated_attachment_reuses_the_exact_actors_transcript() {
        let registry = ActorRegistry::new();
        let actor = ready_actor(&registry);
        let first = ActorAgentSession::attach(registry.clone(), actor).expect("first attachment");
        first.queue_user("shared opening");
        let second = ActorAgentSession::attach(registry.clone(), actor).expect("second attachment");

        let mut admitted = second.begin_agent_session().expect("shared agent session");
        let pending = admitted
            .begin_provider_round(None)
            .expect("shared provider round");
        assert_eq!(pending.request().messages.len(), 1);
        pending
            .complete(response("shared reply"))
            .expect("complete shared turn");
        drop(admitted);
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
            startup.begin_agent_session(),
            Err(ActorRegistryError::Initializing(_))
        ));
        let mut admitted = startup
            .begin_startup_agent_session(&starting)
            .expect("startup capability admits the agent session");
        let pending = admitted
            .begin_provider_round(None)
            .expect("begin startup provider round");
        assert_eq!(pending.request().messages[0].content, "configure behavior");
        pending
            .complete(response("```haskell\ninitialPolicy\n```"))
            .expect("record startup response");

        drop(admitted);

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
            second_session.begin_startup_agent_session(&first),
            Err(ActorRegistryError::StartupSessionMismatch { .. })
        ));
    }
}
