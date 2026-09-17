//! Join logical jobs to the existing native process owner and resource authority.
use futures_util::future::BoxFuture;
use std::sync::Arc;
use tidepool_actor::command_jobs::{CommandBackend, CommandControl};
use tidepool_agent::{
    InteractiveAgentBackend, NativeCommandOperation as Op, NativeCommandReply as Reply,
    QueueReadyThread,
};
use tidepool_bridge_effects::{
    CommandCleanup, CommandError, CommandInput, CommandOutcome, CommandOutput, CommandPage,
    CommandPosition, CommandResult, CommandSpec, CommandStatus, CommandStream,
};
use tidepool_node::command_resources::{CommandResourceClient, CommandResourceStatus as Resource};
use tidepool_node::host_command::{
    HostCommand, HostCommandSpec, HostExit, HostPage, HostStdin, HostStream,
};
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

// ---------------------------------------------------------------------------
// Commands raised by an actor that has no process of its own.
// ---------------------------------------------------------------------------

/// How much of a stream one `read` returns when the caller did not name both
/// ends of the window.
const PAGE_BYTES: u64 = 64 * 1024;

/// A resident actor — one started from a notebook with `R.start`, or an
/// operator workbench — has no agent process and therefore no sandbox of its
/// own, so its commands run in this host process instead.
///
/// Routing them to the nearest interactive ancestor instead, which is what
/// this did before, put them inside *that* actor's sandbox, whose only
/// writable root is that actor's own worktree. A resident actor holding its
/// own worktree could then read it and merge into it (merging is host-side)
/// but could not run `git reset --hard` inside it, which is how a rolled-back
/// check left an integration worktree stranded on a red head in dogfood run 7.
/// Running here matches where every other custody-following operation already
/// runs: `tidepool_worktree::git::GitCli` shells out from this same process.
pub(super) struct HostCommandBackend {
    resources: Arc<CommandResourceClient>,
    actor: String,
    roots: super::ResidentCommandRoots,
    running: parking_lot::Mutex<Option<Arc<HostCommand>>>,
    cancelled: watch::Sender<bool>,
    ready: watch::Sender<OutputReadiness>,
}

impl HostCommandBackend {
    pub(super) fn new(
        resources: Arc<CommandResourceClient>,
        actor: tidepool_actor::ActorRef,
        roots: super::ResidentCommandRoots,
    ) -> Self {
        Self {
            resources,
            actor: format!("{}-{}", actor.id.0, actor.incarnation.0),
            roots,
            running: parking_lot::Mutex::new(None),
            cancelled: watch::channel(false).0,
            ready: watch::channel(OutputReadiness::Pending).0,
        }
    }

    fn live(&self) -> Result<Arc<HostCommand>, CommandError> {
        self.ready.borrow().check()?;
        self.running
            .lock()
            .clone()
            .ok_or_else(|| CommandError::CommandOutputPending)
    }

    async fn execute_inner(
        &self,
        id: &str,
        spec: CommandSpec,
        phase: watch::Sender<CommandStatus>,
    ) -> Result<CommandResult, String> {
        let stdin = match spec.input {
            CommandInput::ClosedInput => HostStdin::Closed,
            CommandInput::PipeInput => HostStdin::Piped,
            // A resident actor has no pane and no terminal to attach one to.
            CommandInput::TerminalInput => {
                return Err(
                    "this actor has no terminal; run the command with piped or closed input"
                        .into(),
                );
            }
        };
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
        let Resource::Admitted { cgroup } = &resource else {
            return Err(format!("command admission: {resource:?}"));
        };
        phase.send_replace(CommandStatus::CommandStarting);
        // An explicitly requested directory wins; otherwise the command runs
        // in whatever custody this actor holds. The boundary is rebuilt around
        // that directory because the wrapper carries its own `--chdir`: a
        // boundary rooted elsewhere would silently run the command elsewhere.
        // Naming a directory never widens what may be written there, and one
        // outside the actor's reach is refused rather than quietly redirected.
        let directory = match &spec.directory {
            Some(requested) => std::path::PathBuf::from(requested),
            None => self.roots.directory.clone(),
        };
        let boundary = tidepool_node::ProcessMountBoundary::new(
            &directory,
            self.roots.protected.clone(),
            self.roots.writable.clone(),
        )
        .map_err(|error| {
            format!(
                "this actor cannot run a command in {}: {error}; it may run in {}{}",
                directory.display(),
                self.roots.directory.display(),
                if self.roots.custody {
                    " and write there"
                } else {
                    ", and holds no worktree to write in"
                }
            )
        })?;
        tracing::debug!(actor = %self.actor, ?directory, custody = self.roots.custody,
            argv = ?spec.argv, "resident actor command");
        let command = Arc::new(
            HostCommand::spawn(HostCommandSpec {
                argv: &spec.argv,
                directory: &directory,
                environment: &spec.environment,
                stdin,
                cgroup: Some(cgroup),
                boundary: Some(&boundary),
            })
            .map_err(detail)?,
        );
        *self.running.lock() = Some(Arc::clone(&command));
        self.resources
            .started(&self.actor, id)
            .await
            .map_err(detail)?;
        self.ready.send_replace(OutputReadiness::Ready);
        phase.send_replace(CommandStatus::CommandRunning);

        let mut stop_sent = false;
        let exit = loop {
            tokio::select! {
                biased;
                _ = wait_until_set(&mut cancelled), if !stop_sent => {
                    stop_sent = true;
                    phase.send_replace(CommandStatus::CommandStopping);
                    // Fence a not-yet-started cgroup join as well, exactly as
                    // the native path does, then stop the whole group.
                    self.resources.cancel(&self.actor, id).await.map_err(detail)?;
                    command.terminate();
                }
                exit = command.wait() => break exit.map_err(detail)?,
            }
        };
        let resource = self.resources.status(&self.actor, id).await.map_err(detail)?;
        let cleanup = match &resource {
            Resource::Completed | Resource::ResourceExhausted | Resource::CancelledBeforeStart => {
                CommandCleanup::CommandClean
            }
            Resource::CleanupUnconfirmed { detail } => {
                CommandCleanup::CommandCleanupUnknown(detail.clone())
            }
            _ => CommandCleanup::CommandRetained,
        };
        let outcome = if matches!(resource, Resource::ResourceExhausted) {
            CommandOutcome::CommandOutOfMemory
        } else if stop_sent {
            CommandOutcome::CommandCancelled
        } else {
            match exit {
                HostExit::Exited(code) => CommandOutcome::CommandExited(i64::from(code)),
                HostExit::Signalled(signal) => CommandOutcome::CommandSignalled(i64::from(signal)),
            }
        };
        Ok(CommandResult { outcome, cleanup })
    }
}

/// A host-side capture retains a bounded tail, so a long stream's oldest bytes
/// are reported as lost ahead of `retained_start` rather than silently absent.
fn command_page(page: HostPage) -> CommandPage {
    CommandPage {
        text: page.text,
        start: page.start as i64,
        end: page.end as i64,
        available_end: page.available_end as i64,
        retained_start: page.retained_start as i64,
        lost_bytes: page.retained_start as i64,
        finished: page.finished,
        lossy: page.lossy,
        leading_fragment: page.leading_fragment,
        trailing_fragment: page.trailing_fragment,
    }
}

/// Byte window for one `read`. Positions are original-stream offsets, so a
/// caller paging forward passes the previous page's `end` back as an offset.
fn window(position: CommandPosition, available: u64) -> (u64, u64) {
    match position {
        CommandPosition::OutputBeginning => (0, PAGE_BYTES),
        CommandPosition::OutputTail => (available.saturating_sub(PAGE_BYTES), available),
        CommandPosition::OutputOffset(start) => {
            let start = start.max(0) as u64;
            (start, start.saturating_add(PAGE_BYTES))
        }
        CommandPosition::OutputSlice(start, end) => (start.max(0) as u64, end.max(0) as u64),
    }
}

impl CommandBackend for HostCommandBackend {
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
            let command = self.live()?;
            let write = |text: String| async move {
                command
                    .write_stdin(&text)
                    .await
                    .map_err(|error| CommandError::CommandInputRejected(error.to_string()))
            };
            match operation {
                CommandControl::Input(text) => write(text).await,
                CommandControl::InputAndClose(text) => {
                    let command = self.live()?;
                    write(text).await?;
                    command.close_stdin().await;
                    Ok(())
                }
                CommandControl::CloseInput => {
                    self.live()?.close_stdin().await;
                    Ok(())
                }
                CommandControl::Resize { .. } => Err(CommandError::CommandInvalid(
                    "this actor's commands have no terminal to resize".into(),
                )),
                CommandControl::Cancel => unreachable!("cancellation accepted above"),
            }
        })
    }

    fn output<'a>(
        &'a self,
        _id: &'a str,
        bytes: usize,
    ) -> BoxFuture<'a, Result<CommandOutput, CommandError>> {
        Box::pin(async move {
            let command = self.live()?;
            // The head of each stream: a command whose whole output fits is
            // then complete, and one that does not reports `end` short of
            // `available_end` so the caller knows to page.
            let bytes = bytes as u64;
            Ok(CommandOutput {
                stdout: command_page(command.page(HostStream::Stdout, 0, bytes)),
                stderr: command_page(command.page(HostStream::Stderr, 0, bytes)),
            })
        })
    }

    fn read<'a>(
        &'a self,
        _id: &'a str,
        stream: CommandStream,
        position: CommandPosition,
    ) -> BoxFuture<'a, Result<CommandPage, CommandError>> {
        Box::pin(async move {
            let command = self.live()?;
            let stream = match stream {
                CommandStream::Stdout => HostStream::Stdout,
                CommandStream::Stderr => HostStream::Stderr,
            };
            let (start, end) = window(position, command.available(stream));
            Ok(command_page(command.page(stream, start, end)))
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

    /// Positions are original-stream byte offsets, so paging forward is
    /// "pass the previous page's `end` back as an offset".
    #[test]
    fn a_read_window_follows_the_position_it_was_asked_for() {
        assert_eq!(window(CommandPosition::OutputBeginning, 10), (0, PAGE_BYTES));
        assert_eq!(window(CommandPosition::OutputSlice(2, 5), 10), (2, 5));
        assert_eq!(
            window(CommandPosition::OutputOffset(4), 10),
            (4, 4 + PAGE_BYTES)
        );
        // A tail shorter than one page starts at the beginning, not below it.
        assert_eq!(window(CommandPosition::OutputTail, 10), (0, 10));
        assert_eq!(
            window(CommandPosition::OutputTail, PAGE_BYTES + 6),
            (6, PAGE_BYTES + 6)
        );
        // Negative offsets are a caller error, not a panic or a wrap.
        assert_eq!(
            window(CommandPosition::OutputOffset(-3), 10),
            (0, PAGE_BYTES)
        );
    }

    #[test]
    fn a_host_page_reports_no_lost_history() {
        let page = command_page(HostPage {
            text: "out".into(),
            start: 1,
            end: 4,
            available_end: 9,
            retained_start: 0,
            finished: false,
            lossy: false,
            leading_fragment: true,
            trailing_fragment: false,
        });
        assert_eq!((page.start, page.end, page.available_end), (1, 4, 9));
        assert_eq!((page.retained_start, page.lost_bytes), (0, 0));
        assert!(page.leading_fragment && !page.finished);
    }
}
