//! The one provider/fenced-Haskell loop used by result-bearing agent sessions.
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
pub struct CompletionExpectation {
    expected_type: String,
}

impl CompletionExpectation {
    #[must_use]
    pub fn new(expected_type: impl Into<String>) -> Self {
        Self {
            expected_type: expected_type.into(),
        }
    }

    #[must_use]
    pub(crate) fn goal_text(&self) -> String {
        format!(
            "Current typed Haskell goal: produce `{}`. The authoritative input is mounted as `goalInput` in the resident workbench; complete the goal through its typed completion action.",
            self.expected_type
        )
    }

    #[must_use]
    pub fn expected_type(&self) -> &str {
        &self.expected_type
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
pub async fn run_result_session<Workbench>(
    admitted: &mut AdmittedAgentSession,
    provider: &dyn DynModelProvider,
    workbench: &mut Workbench,
    task: String,
    completion: CompletionExpectation,
    max_tokens: Option<u32>,
    sink: Option<StreamSink>,
) -> Result<Workbench::Completion, AgentExecutionError<Workbench::Error>>
where
    Workbench: AgentWorkbench,
{
    admitted.queue_developer(completion.goal_text());
    admitted.queue_user(task);

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

        // Keep the mutable workbench borrow inside this loop. The runtime's
        // closure-based helper intentionally cannot lend one `&mut` owner
        // across successive async calls without extra synchronization.
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
