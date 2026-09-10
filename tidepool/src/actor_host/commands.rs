//! Join logical jobs to the existing native process owner and resource authority.
use futures_util::future::BoxFuture;
use std::sync::Arc;
use tidepool_actor::command_jobs::{CommandBackend, CommandControl};
use tidepool_agent::{
    InteractiveAgentBackend, NativeCommandOperation as Op, NativeCommandReply as Reply,
    QueueReadyThread,
};
use tidepool_bridge_effects::{
    CommandCleanup, CommandError, CommandOutcome, CommandOutput, CommandResult, CommandSpec,
    CommandStatus,
};
use tidepool_node::command_resources::{CommandResourceClient, CommandResourceStatus as Resource};
use tokio::sync::watch;

pub(super) struct NativeCommandBackend {
    native: Arc<dyn InteractiveAgentBackend>,
    thread: QueueReadyThread,
    resources: Arc<CommandResourceClient>,
    actor: String,
    cancelled: watch::Sender<bool>,
    ready: watch::Sender<bool>,
}

impl NativeCommandBackend {
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
            ready: watch::channel(false).0,
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
        self.ready.send_replace(true);
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
            self.execute_inner(id, spec, phase)
                .await
                .unwrap_or_else(|detail| CommandResult {
                    outcome: CommandOutcome::CommandUnconfirmed(detail.clone()),
                    cleanup: CommandCleanup::CommandCleanupUnknown(detail),
                })
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
            let mut ready = self.ready.subscribe();
            wait_until_set(&mut ready).await;
            let operation = match operation {
                CommandControl::Input(text) => Op::Input(text),
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
            if !*self.ready.borrow() {
                return Ok(CommandOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    truncated: false,
                });
            }
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
