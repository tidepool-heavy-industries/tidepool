//! Join logical jobs to the existing native process owner and resource authority.
use futures_util::future::BoxFuture;
use std::sync::Arc;
use tidepool_actor::command_jobs::{CommandBackend, CommandControl};
use tidepool_agent::{
    InteractiveAgentBackend, NativeCommandOperation as Op, NativeCommandReply as Reply,
    QueueReadyThread,
};
use tidepool_bridge_effects::{
    CommandCleanup, CommandError, CommandOutcome, CommandOutput, CommandPage, CommandPosition,
    CommandResult, CommandSpec, CommandStatus, CommandStream,
};
use tidepool_node::command_resources::{CommandResourceClient, CommandResourceStatus as Resource};
use tokio::sync::watch;

pub(super) struct NativeCommandBackend {
    native: Arc<dyn InteractiveAgentBackend>,
    thread: QueueReadyThread,
    resources: Arc<CommandResourceClient>,
    actor: String,
    cancelled: watch::Sender<bool>,
    ready: watch::Sender<OutputReadiness>,
}

#[derive(Clone)]
enum OutputReadiness {
    Pending,
    Ready,
    Finished(CommandResult),
}

impl OutputReadiness {
    fn check(&self) -> Result<(), CommandError> {
        match self {
            Self::Pending => Err(CommandError::CommandOutputPending),
            Self::Ready => Ok(()),
            Self::Finished(result) => Err(CommandError::CommandUnavailable(format!(
                "command terminated before output streams became available: {:?}",
                result.outcome
            ))),
        }
    }
}

impl NativeCommandBackend {
    fn output_readiness(&self) -> Result<(), CommandError> {
        self.ready.borrow().check()
    }

    async fn wait_ready(&self) -> Result<(), CommandError> {
        let mut ready = self.ready.subscribe();
        loop {
            if !matches!(*ready.borrow_and_update(), OutputReadiness::Pending) {
                return self.output_readiness();
            }
            ready.changed().await.map_err(|_| {
                CommandError::CommandUnavailable("command readiness channel closed".into())
            })?;
        }
    }

    pub(super) fn new(
        native: Arc<dyn InteractiveAgentBackend>,
        thread: QueueReadyThread,
        resources: Arc<CommandResourceClient>,
        actor: tidepool_actor::ActorRef,
    ) -> Self {
        Self {
            native,
            thread,
            resources,
            actor: format!("{}-{}", actor.id.0, actor.incarnation.0),
            cancelled: watch::channel(false).0,
            ready: watch::channel(OutputReadiness::Pending).0,
        }
    }

    async fn execute_inner(
        &self,
        id: &str,
        spec: CommandSpec,
        phase: watch::Sender<CommandStatus>,
    ) -> Result<CommandResult, String> {
        let mut cancelled = self.cancelled.subscribe();
        self.resources
            .submit(&self.actor, id, spec.memory as u64)
            .await
            .map_err(detail)?;
        let resource = tokio::select! {
            biased;
            _ = wait_until_set(&mut cancelled) => self.resources.cancel(&self.actor, id).await.map_err(detail)?,
            result = self.resources.wait(&self.actor, id) => result.map_err(detail)?,
        };
        if matches!(resource, Resource::CancelledBeforeStart) {
            return Ok(CommandResult {
                outcome: CommandOutcome::CommandCancelled,
                cleanup: CommandCleanup::CommandClean,
            });
        }
        if !matches!(resource, Resource::Admitted { .. }) {
            return Err(format!("command admission: {resource:?}"));
        }
        phase.send_replace(CommandStatus::CommandStarting);
        // Resolve this single submission before acting on cancellation. Abandoning
        // it would lose whether the native owner registered the command.
        let mut reply = self
            .native
            .command(&self.thread, id, Op::Start(spec))
            .await
            .map_err(detail)?;
        self.ready.send_replace(OutputReadiness::Ready);
        let mut stop_sent = false;
        loop {
            match reply {
                Reply::Finished {
                    exit_code,
                    cancelled,
                } => {
                    let resource = self
                        .resources
                        .status(&self.actor, id)
                        .await
                        .map_err(detail)?;
                    let cleanup = match &resource {
                        Resource::Completed
                        | Resource::ResourceExhausted
                        | Resource::CancelledBeforeStart => CommandCleanup::CommandClean,
                        Resource::CleanupUnconfirmed { detail } => {
                            CommandCleanup::CommandCleanupUnknown(detail.clone())
                        }
                        _ => CommandCleanup::CommandRetained,
                    };
                    let outcome = if matches!(resource, Resource::ResourceExhausted) {
                        CommandOutcome::CommandOutOfMemory
                    } else if cancelled || stop_sent {
                        CommandOutcome::CommandCancelled
                    } else {
                        CommandOutcome::CommandExited(i64::from(exit_code))
                    };
                    return Ok(CommandResult { outcome, cleanup });
                }
                Reply::Pending => {}
                Reply::Unconfirmed(detail) => {
                    if matches!(
                        self.resources.status(&self.actor, id).await,
                        Ok(Resource::CancelledBeforeStart)
                    ) {
                        return Ok(CommandResult {
                            outcome: CommandOutcome::CommandCancelled,
                            cleanup: CommandCleanup::CommandClean,
                        });
                    }
                    return Err(detail);
                }
                _ => return Err("unexpected native command state".into()),
            }
            if !stop_sent {
                phase.send_replace(CommandStatus::CommandRunning);
            }
            reply = tokio::select! {
                biased;
                _ = wait_until_set(&mut cancelled), if !stop_sent => {
                    stop_sent = true;
                    phase.send_replace(CommandStatus::CommandStopping);
                    // The resource owner also fences a not-yet-started cgroup join
                    // and terminates descendants outside the native root process.
                    self.resources.cancel(&self.actor, id).await.map_err(detail)?;
                    self.native.command(&self.thread, id, Op::Cancel).await.map_err(detail)?;
                    Reply::Pending
                }
                result = self.native.command(&self.thread, id, Op::Wait) => result.map_err(detail)?,
            };
        }
    }
}

fn detail(error: impl std::fmt::Display) -> String {
    error.to_string()
}
async fn wait_until_set(cancelled: &mut watch::Receiver<bool>) {
    while !*cancelled.borrow_and_update() {
        if cancelled.changed().await.is_err() {
            return;
        }
    }
}

impl CommandBackend for NativeCommandBackend {
    fn execute<'a>(
        &'a self,
        id: &'a str,
        spec: CommandSpec,
        phase: watch::Sender<CommandStatus>,
    ) -> BoxFuture<'a, CommandResult> {
        Box::pin(async move {
            let result = self
                .execute_inner(id, spec, phase)
                .await
                .unwrap_or_else(|detail| CommandResult {
                    outcome: CommandOutcome::CommandUnconfirmed(detail.clone()),
                    cleanup: CommandCleanup::CommandCleanupUnknown(detail),
                });
            self.ready.send_if_modified(|ready| {
                if matches!(ready, OutputReadiness::Pending) {
                    *ready = OutputReadiness::Finished(result.clone());
                    true
                } else {
                    false
                }
            });
            result
        })
    }
    fn control<'a>(
        &'a self,
        id: &'a str,
        operation: CommandControl,
    ) -> BoxFuture<'a, Result<(), CommandError>> {
        Box::pin(async move {
            if matches!(operation, CommandControl::Cancel) {
                self.cancelled.send_replace(true);
                self.resources
                    .cancel(&self.actor, id)
                    .await
                    .map_err(|error| CommandError::CommandUnavailable(error.to_string()))?;
                return Ok(());
            }
            self.wait_ready().await?;
            let operation = match operation {
                CommandControl::Input(text) => Op::Input(text),
                CommandControl::InputAndClose(_) => {
                    return Err(CommandError::CommandInvalid(
                        "combined input must be serviced by the command owner".into(),
                    ));
                }
                CommandControl::CloseInput => Op::CloseInput,
                CommandControl::Resize { rows, columns } => Op::Resize { rows, columns },
                CommandControl::Cancel => unreachable!("cancellation accepted above"),
            };
            match self
                .native
                .command(&self.thread, id, operation)
                .await
                .map_err(|error| CommandError::CommandUnavailable(error.to_string()))?
            {
                Reply::Acknowledged => Ok(()),
                _ => Err(CommandError::CommandUnavailable(
                    "native command control unconfirmed".into(),
                )),
            }
        })
    }
    fn output<'a>(
        &'a self,
        id: &'a str,
        bytes: usize,
    ) -> BoxFuture<'a, Result<CommandOutput, CommandError>> {
        Box::pin(async move {
            self.output_readiness()?;
            match self
                .native
                .command(&self.thread, id, Op::Output(bytes))
                .await
                .map_err(|error| CommandError::CommandUnavailable(error.to_string()))?
            {
                Reply::Output(output) => Ok(output),
                _ => Err(CommandError::CommandUnavailable(
                    "native command output unavailable".into(),
                )),
            }
        })
    }
    fn read<'a>(
        &'a self,
        id: &'a str,
        stream: CommandStream,
        position: CommandPosition,
    ) -> BoxFuture<'a, Result<CommandPage, CommandError>> {
        Box::pin(async move {
            self.output_readiness()?;
            match self
                .native
                .command(&self.thread, id, Op::Read { stream, position })
                .await
                .map_err(|error| CommandError::CommandUnavailable(error.to_string()))?
            {
                Reply::Page(page) => Ok(page),
                _ => Err(CommandError::CommandUnavailable(
                    "native command page unavailable".into(),
                )),
            }
        })
    }
    fn cleanup<'a>(&'a self, id: &'a str) -> BoxFuture<'a, CommandCleanup> {
        Box::pin(async move {
            match self.resources.status(&self.actor, id).await {
                Ok(
                    Resource::Completed
                    | Resource::ResourceExhausted
                    | Resource::CancelledBeforeStart,
                ) => CommandCleanup::CommandClean,
                Ok(Resource::CleanupUnconfirmed { detail }) => {
                    CommandCleanup::CommandCleanupUnknown(detail)
                }
                Ok(_) => CommandCleanup::CommandRetained,
                Err(error) => CommandCleanup::CommandCleanupUnknown(error.to_string()),
            }
        })
    }
}

#[cfg(test)]
mod readiness_tests {
    use super::*;

    #[test]
    fn termination_before_stream_readiness_is_not_pending_or_fabricated_eof() {
        assert!(matches!(
            OutputReadiness::Pending.check(),
            Err(CommandError::CommandOutputPending)
        ));
        assert!(OutputReadiness::Ready.check().is_ok());
        for outcome in [
            CommandOutcome::CommandCancelled,
            CommandOutcome::CommandUnconfirmed("start acknowledgment lost".into()),
        ] {
            let state = OutputReadiness::Finished(CommandResult {
                outcome,
                cleanup: CommandCleanup::CommandRetained,
            });
            assert!(matches!(
                state.check(),
                Err(CommandError::CommandUnavailable(_))
            ));
        }
    }
}
