//! Backgrounded command observation precedes the actor's display-cursor fence.

use crate::command_jobs::CommandJobs;
use crate::resident_workbench::CommandObservationStop;
use crate::ActorRef;
use tidepool_bridge_effects::{CommandError, CommandPage, CommandStream};

pub(super) struct BackgroundCommandRequest {
    pub job: String,
    pub binding: String,
    pub reason: CommandObservationStop,
    pub named_tool: bool,
    pub command_prefix: String,
    pub display_remaining: usize,
}

pub(super) struct PreparedBackgroundCommand {
    actor: ActorRef,
    job: String,
    output: String,
    displayed_pages: Option<Vec<(CommandStream, CommandPage)>>,
}

impl PreparedBackgroundCommand {
    /// The actor applies this only after its original owned-step fence.
    pub(super) fn apply(mut self, jobs: &CommandJobs, actor: ActorRef) -> String {
        if let Some(pages) = self.displayed_pages {
            let marked = if actor == self.actor {
                jobs.mark_displayed(actor, &self.job, &pages)
            } else {
                Err(CommandError::CommandUnauthorized)
            };
            if let Err(error) = marked {
                self.output.push_str(&format!(
                    "\nOutput cursor unavailable: {error:?}; explicit reads remain non-consuming."
                ));
            }
        }
        self.output
    }
}

pub(super) async fn prepare(
    jobs: &CommandJobs,
    actor: ActorRef,
    request: BackgroundCommandRequest,
) -> PreparedBackgroundCommand {
    let BackgroundCommandRequest {
        job,
        binding,
        reason,
        named_tool,
        command_prefix,
        display_remaining,
    } = request;
    let reason = crate::workbench_display::bounded_output(&reason.to_string(), 1024);
    let mut output = if named_tool {
        format!(
            "session_id: {job}\n{reason}. Observation ended; the command remains retained. Poll/send input with write_stdin; read_output navigates output. Later handler effects did not run.\nHaskell binding: {binding} :: Cmd.Job"
        )
    } else {
        format!(
            "Retained command · session_id: {job}\n{reason}. Available binding:\n\n{binding} :: Cmd.Job\n\nThe enclosing result was not bound; subsequent statements did not run.\nContinue with: result <- Cmd.await {binding}"
        )
    };
    if !command_prefix.is_empty() {
        output.push_str(&format!("\n{command_prefix}"));
    }
    let mut displayed_pages = None;
    match jobs.observation(actor, &job).await {
        Ok(pages) => {
            let rendered = crate::workbench_display::bounded_output(
                &crate::workbench_display::command_pages(&pages),
                display_remaining.saturating_sub(output.len()),
            );
            if !rendered.is_empty() {
                output.push_str(&rendered);
                // Explicit pages remain available when presentation is shortened.
                displayed_pages = Some(pages);
            }
        }
        Err(CommandError::CommandOutputPending) => {
            output.push_str("\nNo output yet; streams are starting.");
        }
        Err(error) => output.push_str(&format!(
            "\nOutput unavailable: {error:?}; the same job remains retained."
        )),
    }
    PreparedBackgroundCommand {
        actor,
        job,
        output,
        displayed_pages,
    }
}

#[cfg(test)]
#[path = "background_command_wait_tests.rs"]
mod tests;
