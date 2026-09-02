use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_model::{
    Conversation, DynModelProvider, Message, ProviderError, Role, StreamSink, TurnRequest,
    TurnResponse, Usage,
};
use tidepool_model_output::extract_haskell_blocks;

use crate::mount::{install_actor_context, ActorRunTarget};
use crate::{ActorRef, ActorSessionContext};

/// One local actor's accumulating model transcript and boundary queue.
///
/// Ractor owns exclusive actor admission. This value owns only conversation
/// state; it is never an alternate scheduler or lifecycle registry.
#[derive(Clone)]
pub struct ActorAgentSession {
    actor: ActorRef,
    context: ActorSessionContext,
    state: Arc<Mutex<AgentSessionState>>,
}

struct AgentSessionState {
    conversation: Conversation,
    queued: Vec<Message>,
    next_turn: u64,
}

impl AgentSessionState {
    fn new() -> Self {
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
    Provider(#[from] ProviderError),
}

impl ActorAgentSession {
    #[must_use]
    pub fn local(context: ActorSessionContext) -> Self {
        Self {
            actor: context.actor,
            context,
            state: Arc::new(Mutex::new(AgentSessionState::new())),
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

    #[must_use]
    pub fn begin_agent_session(&self) -> AdmittedAgentSession {
        AdmittedAgentSession {
            session: self.clone(),
            context: self.context.clone(),
        }
    }

    #[must_use]
    pub fn transcript(&self) -> Vec<Message> {
        self.state.lock().conversation.messages().to_vec()
    }
}

/// A complete agent interaction admitted by its owning Ractor message turn.
pub struct AdmittedAgentSession {
    session: ActorAgentSession,
    context: ActorSessionContext,
}

impl AdmittedAgentSession {
    #[must_use]
    pub fn actor(&self) -> ActorRef {
        self.session.actor
    }

    #[must_use]
    pub fn session_context(&self) -> ActorSessionContext {
        self.context.clone()
    }

    pub(crate) fn queue_developer(&self, content: impl Into<String>) {
        self.session.queue_developer(content);
    }

    pub(crate) fn queue_user(&self, content: impl Into<String>) {
        self.session.queue_user(content);
    }

    pub fn install_execution<Target>(&self, target: &mut Target) -> Result<(), Target::Error>
    where
        Target: ActorRunTarget,
    {
        install_actor_context(target, &self.context)
    }

    /// Assemble one request at a legal provider boundary. Queued messages are
    /// transferred exactly once even when the provider call is later retried.
    #[must_use]
    pub fn begin_provider_round(&mut self, max_tokens: Option<u32>) -> PendingProviderRound<'_> {
        let (turn, request) = {
            let mut state = self.session.state.lock();
            let queued = std::mem::take(&mut state.queued);
            for message in queued {
                state.conversation.append(message);
            }
            (state.next_turn, state.conversation.request(max_tokens))
        };
        PendingProviderRound {
            admitted: self,
            request,
            turn,
        }
    }

    pub async fn run_provider_round(
        &mut self,
        provider: &dyn DynModelProvider,
        max_tokens: Option<u32>,
        sink: Option<StreamSink>,
    ) -> Result<AssistantTurn, AgentSessionError> {
        let pending = self.begin_provider_round(max_tokens);
        let response = provider
            .complete_boxed(pending.request().clone(), sink)
            .await?;
        Ok(pending.complete(response))
    }
}

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

    /// Commit one provider response while the enclosing actor turn remains
    /// exclusive.
    #[must_use]
    pub fn complete(self, response: TurnResponse) -> AssistantTurn {
        let reply = response.text;
        let usage = response.usage;
        let reasoning = response.reasoning;
        {
            let mut state = self.admitted.session.state.lock();
            state.conversation.append(Message {
                role: Role::Assistant,
                content: reply.clone(),
                reasoning_items: response.reasoning_items,
            });
            state.next_turn += 1;
        }
        AssistantTurn {
            turn: self.turn,
            blocks: extract_haskell_blocks(&reply),
            reply,
            usage,
            reasoning,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_codegen::{scope::ScopeId, suspension::RealmId};
    use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
    use tidepool_model::{ModelProvider, StreamSink};

    fn context() -> ActorSessionContext {
        ActorSessionContext {
            actor: ActorRef::first(crate::ActorId(41)),
            placement: crate::ActorPlacement {
                session: tidepool_repr::SessionId(7),
                resource_scope: RealmId::fresh(),
                lexical_scope: ScopeId(9),
            },
            effect_policy: EffectRunPolicy::HandleOrSuspend,
            live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            source_imports: crate::ActorSourceImports::default(),
        }
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
    fn queued_lifecycle_fact_enters_only_after_the_pending_round() {
        let session = ActorAgentSession::local(context());
        session.queue_user("review this");
        let mut admitted = session.begin_agent_session();
        let pending = admitted.begin_provider_round(None);
        session.queue_developer("child exited");
        let assistant = pending.complete(response("```haskell\npure ()\n```"));
        assert_eq!(assistant.blocks, ["pure ()"]);

        let next = admitted.begin_provider_round(None);
        let roles: Vec<_> = next
            .request()
            .messages
            .iter()
            .map(|message| message.role)
            .collect();
        assert_eq!(roles, [Role::User, Role::Assistant, Role::Developer]);
    }

    struct FailsOnce(std::sync::atomic::AtomicUsize);

    impl ModelProvider for FailsOnce {
        async fn complete(
            &self,
            _request: TurnRequest,
            _sink: Option<StreamSink>,
        ) -> Result<TurnResponse, ProviderError> {
            if self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                Err(ProviderError::Api("temporary".into()))
            } else {
                Ok(response("done"))
            }
        }
    }

    #[tokio::test]
    async fn provider_failure_does_not_duplicate_injected_input() {
        let session = ActorAgentSession::local(context());
        session.queue_user("start");
        let mut admitted = session.begin_agent_session();
        let provider = FailsOnce(std::sync::atomic::AtomicUsize::new(0));
        assert!(admitted
            .run_provider_round(&provider, None, None)
            .await
            .is_err());
        admitted
            .run_provider_round(&provider, None, None)
            .await
            .expect("retry");
        let transcript = session.transcript();
        assert_eq!(
            transcript
                .iter()
                .filter(|message| message.role == Role::User)
                .count(),
            1
        );
    }
}
