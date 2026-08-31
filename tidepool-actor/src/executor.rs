//! The one provider/fenced-Haskell loop used by typed actor deliberations.
//!
//! Provider transcript mechanics remain in [`crate::ActorAgentSession`], and
//! resident compilation/execution remains behind [`AgentWorkbench`]. This
//! module owns only their actor-level protocol: inject one typed goal, execute
//! every tagged Haskell fence in order, report receipts at a Developer
//! boundary, and stop exactly once when the workbench produces the required
//! live value.

use std::fmt;
use std::future::Future;

use tidepool_model::{DynModelProvider, ProviderError, StreamSink};
use tidepool_runtime::session::{BlockExecution, BlockSequenceOutcome, ParsedBlock};

use crate::{AdmittedAgentSession, AgentSessionError};

const PROVIDER_API_ATTEMPTS: usize = 3;

/// Model-facing description of one statically typed completion obligation.
/// The authoritative input and completion type remain mounted in Haskell;
/// these strings orient the model and render `:goal`-equivalent context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedGoal {
    pub task: String,
    pub expected_type: String,
}

impl TypedGoal {
    #[must_use]
    pub fn new(task: impl Into<String>, expected_type: impl Into<String>) -> Self {
        Self {
            task: task.into(),
            expected_type: expected_type.into(),
        }
    }
}

/// A fenced fragment either made persistent progress or settled the current
/// typed obligation. A rejected fragment is deliberately different from a
/// Rust error: its continuation was abandoned safely, and its diagnostic is
/// useful context for the next model round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentBlockStop<Completion> {
    Completed(Completion),
    Rejected(String),
}

/// Actor-neutral resident workbench seam. The concrete implementation owns
/// GHC compilation, live bindings, effect settlement, and typed completion;
/// the agent executor owns neither machine checkout nor Haskell policy.
pub trait AgentWorkbench {
    type Completion;
    type Error;

    fn execute(
        &mut self,
        admitted: &AdmittedAgentSession,
        block: ParsedBlock,
    ) -> impl Future<
        Output = Result<BlockExecution<String, AgentBlockStop<Self::Completion>>, Self::Error>,
    > + Send;
}

#[derive(Debug, thiserror::Error)]
pub enum AgentExecutionError<WorkbenchError> {
    #[error(transparent)]
    Session(#[from] AgentSessionError),
    #[error("resident Haskell workbench failed: {0}")]
    Workbench(WorkbenchError),
}

/// Run one typed completion obligation inside an already-admitted actor
/// session. The admission guard lives in `admitted` across provider waits and
/// every workbench segment; a concrete workbench acquires machine ownership
/// only inside `execute`.
pub async fn run_typed_deliberation<Workbench>(
    admitted: &mut AdmittedAgentSession,
    provider: &dyn DynModelProvider,
    workbench: &mut Workbench,
    goal: TypedGoal,
    max_tokens: Option<u32>,
    sink: Option<StreamSink>,
) -> Result<Workbench::Completion, AgentExecutionError<Workbench::Error>>
where
    Workbench: AgentWorkbench,
{
    admitted.queue_developer(format!(
        "Current typed Haskell goal: produce `{}`. The authoritative input is mounted as `goalInput` in the resident workbench; complete the goal through its typed completion action.",
        goal.expected_type
    ));
    admitted.queue_user(goal.task);

    loop {
        let assistant =
            run_provider_with_retry(admitted, provider, max_tokens, sink.clone()).await?;
        if assistant.blocks.is_empty() {
            admitted.queue_developer(format!(
                "No tagged Haskell block was executed in assistant turn {}. Continue the current typed goal with a fenced `haskell` or `hs` block.",
                assistant.turn
            ));
            continue;
        }

        let total = assistant.blocks.len();
        let mut sequence = tidepool_runtime::session::WorkSequence::new(
            assistant
                .blocks
                .into_iter()
                .enumerate()
                .map(|(index, source)| ParsedBlock {
                    ordinal: index + 1,
                    total,
                    source,
                })
                .collect(),
        );
        let outcome = loop {
            let Some(block) = sequence.current().cloned() else {
                break BlockSequenceOutcome::Completed {
                    committed: sequence.into_committed(),
                };
            };
            match workbench.execute(admitted, block.clone()).await {
                Ok(BlockExecution::Committed(output)) => {
                    sequence
                        .commit_next(tidepool_runtime::session::CommittedBlock { block, output });
                }
                Ok(BlockExecution::Stopped(outcome)) => {
                    break BlockSequenceOutcome::Stopped {
                        committed: sequence.into_committed(),
                        block,
                        outcome,
                    };
                }
                Err(error) => {
                    break BlockSequenceOutcome::Failed {
                        committed: sequence.into_committed(),
                        block,
                        error,
                    };
                }
            }
        };
        match outcome {
            BlockSequenceOutcome::Completed { committed } => {
                admitted.queue_developer(render_receipts(assistant.turn, &committed));
            }
            BlockSequenceOutcome::Stopped {
                committed,
                block,
                outcome: AgentBlockStop::Rejected(diagnostic),
            } => {
                admitted.queue_developer(render_rejection(
                    assistant.turn,
                    &committed,
                    &block,
                    &diagnostic,
                ));
            }
            BlockSequenceOutcome::Stopped {
                outcome: AgentBlockStop::Completed(value),
                ..
            } => return Ok(value),
            BlockSequenceOutcome::Failed { error, .. } => {
                return Err(AgentExecutionError::Workbench(error));
            }
        }
    }
}

async fn run_provider_with_retry(
    admitted: &mut AdmittedAgentSession,
    provider: &dyn DynModelProvider,
    max_tokens: Option<u32>,
    sink: Option<StreamSink>,
) -> Result<crate::AssistantTurn, AgentSessionError> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        match admitted
            .run_provider_round(provider, max_tokens, sink.clone())
            .await
        {
            Ok(turn) => return Ok(turn),
            Err(AgentSessionError::Provider(ProviderError::Api(_)))
                if attempts < PROVIDER_API_ATTEMPTS => {}
            Err(error) => return Err(error),
        }
    }
}

fn render_receipts<Receipt: fmt::Display>(
    turn: u64,
    committed: &[tidepool_runtime::session::CommittedBlock<Receipt>],
) -> String {
    let mut rendered = render_committed(turn, committed);
    rendered.push_str("\n\nThe typed goal remains open.");
    rendered
}

fn render_committed<Receipt: fmt::Display>(
    turn: u64,
    committed: &[tidepool_runtime::session::CommittedBlock<Receipt>],
) -> String {
    let mut rendered = format!("Haskell workbench receipts for assistant turn {turn}:");
    for receipt in committed {
        rendered.push_str(&format!(
            "\n\n[block {}/{}]\n{}",
            receipt.block.ordinal, receipt.block.total, receipt.output
        ));
    }
    rendered
}

fn render_rejection<Receipt: fmt::Display>(
    turn: u64,
    committed: &[tidepool_runtime::session::CommittedBlock<Receipt>],
    rejected: &ParsedBlock,
    diagnostic: &str,
) -> String {
    let mut rendered = render_committed(turn, committed);
    rendered.push_str(&format!(
        "\n\n[block {}/{} rejected]\n{}\n\nThe rejected fragment was abandoned; its suffix did not run. The typed goal remains open.",
        rejected.ordinal, rejected.total, diagnostic
    ));
    rendered
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;

    use parking_lot::Mutex;
    use tidepool_codegen::{scope::ScopeId, suspension::RealmId};
    use tidepool_model::{Message, ModelProvider, Role, TurnRequest, TurnResponse, Usage};
    use tidepool_repr::SessionId;

    use super::*;
    use crate::{
        ActorDescriptor, ActorPlacement, ActorRef, ActorRegistry, ActorRegistryError,
        ActorTurnKind, StartInitiator,
    };

    struct ScriptedProvider {
        responses: Mutex<VecDeque<Result<TurnResponse, ProviderError>>>,
        requests: Mutex<Vec<TurnRequest>>,
    }

    impl ScriptedProvider {
        fn new(responses: impl IntoIterator<Item = Result<TurnResponse, ProviderError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl ModelProvider for ScriptedProvider {
        async fn complete(
            &self,
            request: TurnRequest,
            _sink: Option<StreamSink>,
        ) -> Result<TurnResponse, ProviderError> {
            self.requests.lock().push(request);
            self.responses
                .lock()
                .pop_front()
                .unwrap_or_else(|| Err(ProviderError::Api("script exhausted".into())))
        }
    }

    struct ScriptedWorkbench {
        registry: ActorRegistry,
        actor: ActorRef,
        seen: Vec<String>,
    }

    impl AgentWorkbench for ScriptedWorkbench {
        type Completion = usize;
        type Error = Infallible;

        async fn execute(
            &mut self,
            _admitted: &AdmittedAgentSession,
            block: ParsedBlock,
        ) -> Result<BlockExecution<String, AgentBlockStop<usize>>, Infallible> {
            assert!(matches!(
                self.registry.begin_turn(self.actor, ActorTurnKind::Mailbox),
                Err(ActorRegistryError::Busy {
                    active: ActorTurnKind::AgentSession,
                    ..
                })
            ));
            self.seen.push(block.source.clone());
            Ok(match block.source.as_str() {
                "complete" => BlockExecution::Stopped(AgentBlockStop::Completed(42)),
                "reject" => BlockExecution::Stopped(AgentBlockStop::Rejected(
                    "expected Review, got Text".into(),
                )),
                source => BlockExecution::Committed(format!("committed {source}")),
            })
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
                        session: SessionId(1),
                        resource_scope: RealmId::ROOT,
                        lexical_scope: ScopeId::ROOT,
                    },
                ),
                StartInitiator::Runtime,
            )
            .expect("begin actor startup");
        registry.publish_ready(starting).expect("publish actor")
    }

    fn response(text: &str) -> Result<TurnResponse, ProviderError> {
        Ok(TurnResponse {
            text: text.into(),
            usage: Usage::default(),
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }

    #[tokio::test]
    async fn completion_settles_once_and_never_executes_the_suffix() {
        let registry = ActorRegistry::new();
        let actor = ready_actor(&registry);
        let session = crate::ActorAgentSession::attach(registry.clone(), actor)
            .expect("attach actor session");
        let mut admitted = session.begin_agent_session().expect("admit session");
        let provider = ScriptedProvider::new([response(
            "```haskell\ndeclare\n```\n```hs\ncomplete\n```\n```haskell\nmust-not-run\n```",
        )]);
        let mut workbench = ScriptedWorkbench {
            registry,
            actor,
            seen: Vec::new(),
        };

        let value = run_typed_deliberation(
            &mut admitted,
            &provider,
            &mut workbench,
            TypedGoal::new("produce the answer", "Answer"),
            None,
            None,
        )
        .await
        .expect("typed completion");

        assert_eq!(value, 42);
        assert_eq!(workbench.seen, ["declare", "complete"]);
        let roles: Vec<_> = session
            .transcript()
            .into_iter()
            .map(|message| message.role)
            .collect();
        assert_eq!(roles, [Role::Developer, Role::User, Role::Assistant]);
    }

    #[tokio::test]
    async fn rejected_fragment_reports_context_and_continues_next_round() {
        let registry = ActorRegistry::new();
        let actor = ready_actor(&registry);
        let session = crate::ActorAgentSession::attach(registry.clone(), actor)
            .expect("attach actor session");
        let mut admitted = session.begin_agent_session().expect("admit session");
        let provider = ScriptedProvider::new([
            response("```haskell\nprogress\n```\n```haskell\nreject\n```\n```haskell\nskip\n```"),
            response("```haskell\ncomplete\n```"),
        ]);
        let mut workbench = ScriptedWorkbench {
            registry,
            actor,
            seen: Vec::new(),
        };

        let value = run_typed_deliberation(
            &mut admitted,
            &provider,
            &mut workbench,
            TypedGoal::new("review", "Review"),
            None,
            None,
        )
        .await
        .expect("corrected completion");

        assert_eq!(value, 42);
        assert_eq!(workbench.seen, ["progress", "reject", "complete"]);
        let transcript = session.transcript();
        let roles: Vec<_> = transcript.iter().map(|message| message.role).collect();
        assert_eq!(
            roles,
            [
                Role::Developer,
                Role::User,
                Role::Assistant,
                Role::Developer,
                Role::Assistant,
            ]
        );
        assert!(transcript[3].content.contains("block 2/3 rejected"));
        assert!(transcript[3].content.contains("suffix did not run"));
        assert_eq!(provider.requests.lock()[1].messages, transcript[..4]);
    }

    #[tokio::test]
    async fn transient_provider_failure_replays_the_exact_request() {
        let registry = ActorRegistry::new();
        let actor = ready_actor(&registry);
        let session = crate::ActorAgentSession::attach(registry.clone(), actor)
            .expect("attach actor session");
        let mut admitted = session.begin_agent_session().expect("admit session");
        let provider = ScriptedProvider::new([
            Err(ProviderError::Api("temporary".into())),
            response("```haskell\ncomplete\n```"),
        ]);
        let mut workbench = ScriptedWorkbench {
            registry,
            actor,
            seen: Vec::new(),
        };

        run_typed_deliberation(
            &mut admitted,
            &provider,
            &mut workbench,
            TypedGoal::new("answer", "Answer"),
            None,
            None,
        )
        .await
        .expect("retry succeeds");

        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], requests[1]);
        assert_eq!(
            requests[0].messages,
            vec![
                Message {
                    role: Role::Developer,
                    content: "Current typed Haskell goal: produce `Answer`. The authoritative input is mounted as `goalInput` in the resident workbench; complete the goal through its typed completion action.".into(),
                    reasoning_items: Vec::new(),
                },
                Message {
                    role: Role::User,
                    content: "answer".into(),
                    reasoning_items: Vec::new(),
                },
            ]
        );
    }
}
