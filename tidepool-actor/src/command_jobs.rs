//! Lightweight native-command actors. The backend retains OS-process custody.
use crate::{ActorRef, KernelContext};
use futures_util::future::BoxFuture;
use parking_lot::Mutex;
use ractor::{Actor, ActorProcessingErr, ActorRef as RactorRef};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tidepool_bridge_effects::{
    CommandCleanup, CommandError, CommandOutcome, CommandOutput, CommandPage, CommandPosition,
    CommandResult, CommandSpec, CommandStatus, CommandStream,
};
use tokio::sync::{oneshot, watch};

type CompletionSink = dyn Fn(CommandResult) -> bool + Send + Sync;

/// Native execution remains in the owning TUI; these operations carry no shell launcher.
pub trait CommandBackend: Send + Sync + 'static {
    fn execute<'a>(
        &'a self,
        id: &'a str,
        spec: CommandSpec,
        phase: watch::Sender<CommandStatus>,
    ) -> BoxFuture<'a, CommandResult>;
    fn control<'a>(
        &'a self,
        id: &'a str,
        operation: CommandControl,
    ) -> BoxFuture<'a, Result<(), CommandError>>;
    fn output<'a>(
        &'a self,
        id: &'a str,
        bytes: usize,
    ) -> BoxFuture<'a, Result<CommandOutput, CommandError>>;
    fn read<'a>(
        &'a self,
        id: &'a str,
        stream: CommandStream,
        position: CommandPosition,
    ) -> BoxFuture<'a, Result<CommandPage, CommandError>>;
    fn cleanup<'a>(&'a self, id: &'a str) -> BoxFuture<'a, CommandCleanup>;
}
#[derive(Clone, Debug)]
pub enum CommandControl {
    Input(String),
    CloseInput,
    Resize { rows: u16, columns: u16 },
    Cancel,
}

type BackendResult = Result<Arc<dyn CommandBackend>, CommandError>;

/// The deployment owner supplies a backend for this exact actor, never an ambient executor.
pub struct CommandBackendRequest {
    pub owner: ActorRef,
    reply: Mutex<Option<oneshot::Sender<BackendResult>>>,
}
impl CommandBackendRequest {
    pub fn supply(&self, backend: BackendResult) {
        if let Some(reply) = self.reply.lock().take() {
            let _ = reply.send(backend);
        }
    }
}

struct Shared {
    owner: ActorRef,
    phase: watch::Sender<CommandStatus>,
    backend: Mutex<Option<Arc<dyn CommandBackend>>>,
    sinks: Mutex<Vec<std::sync::Weak<CompletionSink>>>,
    observers: Mutex<HashMap<ActorRef, usize>>,
    displayed: Mutex<HashMap<ActorRef, [i64; 2]>>,
}
impl Shared {
    /// A settled command with no supplied backend never opened output streams.
    fn unstarted_output(&self) -> Option<CommandPage> {
        let phase = self.phase.borrow();
        if !matches!(
            &*phase,
            CommandStatus::CommandFinished(CommandResult {
                outcome: CommandOutcome::CommandCancelled | CommandOutcome::CommandFailed(_),
                cleanup: CommandCleanup::CommandClean,
            })
        ) {
            return None;
        }
        Some(CommandPage {
            text: String::new(),
            start: 0,
            end: 0,
            available_end: 0,
            retained_start: 0,
            lost_bytes: 0,
            finished: true,
            lossy: false,
            leading_fragment: false,
            trailing_fragment: false,
        })
    }

    async fn cleanup(&self, id: &str) -> CommandCleanup {
        let current = self.phase.borrow().clone();
        let CommandStatus::CommandFinished(result) = current else {
            return CommandCleanup::CommandRetained;
        };
        if result.cleanup == CommandCleanup::CommandClean {
            return result.cleanup;
        }
        let backend = self.backend.lock().clone();
        let Some(backend) = backend else {
            return result.cleanup;
        };
        let cleanup = backend.cleanup(id).await;
        self.phase.send_modify(|phase| {
            if let CommandStatus::CommandFinished(result) = phase {
                result.cleanup = cleanup.clone();
            }
        });
        cleanup
    }
    fn complete(&self, result: CommandResult) {
        let mut sinks = self.sinks.lock();
        if matches!(*self.phase.borrow(), CommandStatus::CommandFinished(_)) {
            return;
        }
        self.phase
            .send_replace(CommandStatus::CommandFinished(result.clone()));
        sinks.retain(|sink| sink.upgrade().is_some_and(|sink| sink(result.clone())));
        sinks.clear();
    }
}

struct Entry {
    actor: RactorRef<JobMessage>,
    shared: Arc<Shared>,
}
#[derive(Clone, Default)]
pub struct CommandJobs {
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

pub struct CommandConnection {
    _sink: Arc<CompletionSink>,
    shared: Arc<Shared>,
    observer: ActorRef,
}
impl CommandConnection {
    pub(crate) fn handoff(&mut self, successor: ActorRef) {
        let mut observers = self.shared.observers.lock();
        Self::release(&mut observers, self.observer);
        *observers.entry(successor).or_default() += 1;
        self.observer = successor;
    }

    fn release(observers: &mut HashMap<ActorRef, usize>, observer: ActorRef) {
        if let Some(count) = observers.get_mut(&observer) {
            *count -= 1;
            if *count == 0 {
                observers.remove(&observer);
            }
        }
    }
}
impl Drop for CommandConnection {
    fn drop(&mut self) {
        Self::release(&mut self.shared.observers.lock(), self.observer);
    }
}

impl CommandJobs {
    pub(crate) async fn start(
        &self,
        parent: &KernelContext,
        spec: CommandSpec,
    ) -> Result<(String, Arc<CommandBackendRequest>), CommandError> {
        if spec.argv.is_empty()
            || spec.argv[0].is_empty()
            || spec.memory <= 0
            || spec.argv.iter().any(|arg| arg.contains('\0'))
            || spec
                .directory
                .as_ref()
                .is_some_and(|path| path.contains('\0'))
            || spec.environment.iter().any(|(key, value)| {
                key.is_empty() || key.contains(['\0', '=']) || value.contains('\0')
            })
        {
            return Err(CommandError::CommandInvalid(
                "invalid argv, environment, directory or memory limit".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (reply, receive) = oneshot::channel();
        let request = Arc::new(CommandBackendRequest {
            owner: parent.identity(),
            reply: Mutex::new(Some(reply)),
        });
        let shared = Arc::new(Shared {
            owner: parent.identity(),
            phase: watch::channel(CommandStatus::CommandQueued).0,
            backend: Mutex::new(None),
            sinks: Mutex::new(Vec::new()),
            observers: Mutex::new(Default::default()),
            displayed: Mutex::new(Default::default()),
        });
        let cleanup_shared = shared.clone();
        let cleanup_id = id.clone();
        let actor = parent
            .spawn_resource(
                Some(format!("command-{id}")),
                JobActor,
                JobArguments {
                    id: id.clone(),
                    spec,
                    shared: shared.clone(),
                    backend: receive,
                },
                move |mode| {
                    let shared = cleanup_shared.clone();
                    let id = cleanup_id.clone();
                    Box::pin(async move {
                        if matches!(mode, crate::local_actor::ResourceCleanup::Retire) {
                            let backend = shared.backend.lock().clone();
                            if let Some(backend) = backend {
                                let _ = backend.control(&id, CommandControl::Cancel).await;
                            }
                        }
                        match shared.cleanup(&id).await {
                            CommandCleanup::CommandClean => {
                                crate::CleanupComponentOutcome::Confirmed
                            }
                            CommandCleanup::CommandRetained => {
                                crate::CleanupComponentOutcome::Unconfirmed(format!(
                                    "command {id} retains descendants"
                                ))
                            }
                            CommandCleanup::CommandCleanupUnknown(detail) => {
                                crate::CleanupComponentOutcome::Unconfirmed(detail)
                            }
                        }
                    })
                },
            )
            .await
            .map_err(|error| CommandError::CommandUnavailable(error.to_string()))?;
        self.entries
            .lock()
            .insert(id.clone(), Entry { actor, shared });
        Ok((id, request))
    }

    fn shared(&self, owner: ActorRef, id: &str) -> Result<Arc<Shared>, CommandError> {
        let entries = self.entries.lock();
        let entry = entries
            .get(id)
            .ok_or_else(|| CommandError::CommandUnavailable("unknown command job".into()))?;
        if entry.shared.owner != owner && !entry.shared.observers.lock().contains_key(&owner) {
            return Err(CommandError::CommandUnauthorized);
        }
        Ok(entry.shared.clone())
    }

    pub async fn status(&self, owner: ActorRef, id: &str) -> Result<CommandStatus, CommandError> {
        let shared = self.shared(owner, id)?;
        let _ = shared.cleanup(id).await;
        let phase = shared.phase.borrow().clone();
        Ok(phase)
    }

    pub async fn wait(
        &self,
        owner: ActorRef,
        id: &str,
        milliseconds: i64,
    ) -> Result<CommandStatus, CommandError> {
        if milliseconds < -1 {
            return Err(CommandError::CommandInvalid(
                "wait must be -1 or nonnegative milliseconds".into(),
            ));
        }
        let shared = self.shared(owner, id)?;
        let mut phase = shared.phase.subscribe();
        let wait = async {
            loop {
                let value = phase.borrow_and_update().clone();
                if matches!(value, CommandStatus::CommandFinished(_)) {
                    return value;
                }
                if phase.changed().await.is_err() {
                    return shared.phase.borrow().clone();
                }
            }
        };
        if milliseconds == -1 {
            Ok(wait.await)
        } else {
            Ok(
                tokio::time::timeout(Duration::from_millis(milliseconds as u64), wait)
                    .await
                    .unwrap_or_else(|_| shared.phase.borrow().clone()),
            )
        }
    }

    pub async fn control(
        &self,
        owner: ActorRef,
        id: &str,
        operation: CommandControl,
    ) -> Result<(), CommandError> {
        let shared = self.shared(owner, id)?;
        if shared.owner != owner {
            return Err(CommandError::CommandUnauthorized);
        }
        let finished = matches!(*shared.phase.borrow(), CommandStatus::CommandFinished(_));
        if finished {
            if matches!(operation, CommandControl::Cancel) {
                let backend = shared.backend.lock().clone();
                if let Some(backend) = backend {
                    backend.control(id, operation).await?;
                }
                let _ = shared.cleanup(id).await;
                return Ok(());
            }
            return Err(CommandError::CommandUnavailable(
                "command has finished".into(),
            ));
        }
        let actor = self
            .entries
            .lock()
            .get(id)
            .map(|entry| entry.actor.clone())
            .ok_or_else(|| CommandError::CommandUnavailable("job retired".into()))?;
        let (reply, receive) = oneshot::channel();
        actor
            .cast(JobMessage::Control(operation, reply))
            .map_err(|error| CommandError::CommandUnavailable(error.to_string()))?;
        receive.await.map_err(|_| {
            CommandError::CommandUnavailable(
                "command control interrupted; inspect the existing job".into(),
            )
        })?
    }

    pub async fn output(
        &self,
        owner: ActorRef,
        id: &str,
        bytes: usize,
    ) -> Result<CommandOutput, CommandError> {
        if bytes > 1024 * 1024 {
            return Err(CommandError::CommandInvalid(
                "read at most 1048576 output bytes per stream".into(),
            ));
        }
        let shared = self.shared(owner, id)?;
        let Some(backend) = shared.backend.lock().clone() else {
            if let Some(page) = shared.unstarted_output() {
                return Ok(CommandOutput {
                    stdout: page.clone(),
                    stderr: page,
                });
            }
            return Err(CommandError::CommandUnavailable(
                "command backend not ready; retain the job".into(),
            ));
        };
        backend.output(id, bytes).await
    }

    pub async fn read(
        &self,
        owner: ActorRef,
        id: &str,
        stream: CommandStream,
        position: CommandPosition,
    ) -> Result<CommandPage, CommandError> {
        if matches!(position, CommandPosition::OutputOffset(n) if n < 0) {
            return Err(CommandError::CommandInvalid(
                "output position must be nonnegative".into(),
            ));
        }
        let shared = self.shared(owner, id)?;
        let Some(backend) = shared.backend.lock().clone() else {
            return shared.unstarted_output().ok_or_else(|| {
                CommandError::CommandUnavailable("command backend not ready; retain the job".into())
            });
        };
        backend.read(id, stream, position).await
    }

    /// Read an observation without consuming explicit pages. The caller advances
    /// the display cursor only after attaching these pages to its response.
    pub(crate) async fn observation(
        &self,
        owner: ActorRef,
        id: &str,
    ) -> Result<Vec<(CommandStream, CommandPage)>, CommandError> {
        let shared = self.shared(owner, id)?;
        let cursors = shared
            .displayed
            .lock()
            .get(&owner)
            .copied()
            .unwrap_or_default();
        let mut pages = Vec::new();
        for (stream, cursor) in [CommandStream::Stdout, CommandStream::Stderr]
            .into_iter()
            .zip(cursors)
        {
            let page = self
                .read(
                    owner,
                    id,
                    stream.clone(),
                    CommandPosition::OutputOffset(cursor),
                )
                .await?;
            let end = page.end;
            let available = page.available_end;
            if page.end > cursor || page.lost_bytes > 0 {
                pages.push((stream.clone(), page));
            }
            if end < available {
                let tail = self
                    .read(owner, id, stream.clone(), CommandPosition::OutputTail)
                    .await?;
                let tail = if tail.start < end {
                    self.read(
                        owner,
                        id,
                        stream.clone(),
                        CommandPosition::OutputOffset(end),
                    )
                    .await?
                } else {
                    tail
                };
                if tail.end > end {
                    pages.push((stream, tail));
                }
            }
        }
        Ok(pages)
    }

    pub(crate) fn mark_displayed(
        &self,
        owner: ActorRef,
        id: &str,
        pages: &[(CommandStream, CommandPage)],
    ) -> Result<(), CommandError> {
        let shared = self.shared(owner, id)?;
        let mut displayed = shared.displayed.lock();
        let cursors = displayed.entry(owner).or_default();
        for (stream, page) in pages {
            let index = match stream {
                CommandStream::Stdout => 0,
                CommandStream::Stderr => 1,
            };
            cursors[index] = cursors[index].max(page.end);
        }
        Ok(())
    }

    pub(crate) fn connect(
        &self,
        owner: ActorRef,
        observer: ActorRef,
        id: &str,
        sink: impl Fn(CommandResult) -> bool + Send + Sync + 'static,
    ) -> Result<CommandConnection, CommandError> {
        let shared = self.shared(owner, id)?;
        *shared.observers.lock().entry(observer).or_default() += 1;
        let sink: Arc<CompletionSink> = Arc::new(sink);
        let mut sinks = shared.sinks.lock();
        if let CommandStatus::CommandFinished(result) = &*shared.phase.borrow() {
            sink(result.clone());
        } else {
            sinks.retain(|sink| sink.strong_count() > 0);
            sinks.push(Arc::downgrade(&sink));
        }
        drop(sinks);
        Ok(CommandConnection {
            _sink: sink,
            shared,
            observer,
        })
    }
}

struct JobActor;
struct JobArguments {
    id: String,
    spec: CommandSpec,
    shared: Arc<Shared>,
    backend: oneshot::Receiver<BackendResult>,
}
enum JobMessage {
    BackendReady(BackendResult),
    Finished,
    Control(CommandControl, oneshot::Sender<Result<(), CommandError>>),
    ControlFinished(
        oneshot::Sender<Result<(), CommandError>>,
        Result<(), CommandError>,
    ),
}
struct JobState {
    id: String,
    spec: Option<CommandSpec>,
    shared: Arc<Shared>,
    execution: tokio::task::JoinHandle<()>,
    controls: VecDeque<(CommandControl, oneshot::Sender<Result<(), CommandError>>)>,
    control: Option<tokio::task::JoinHandle<()>>,
}

fn unconfirmed(detail: String) -> CommandResult {
    CommandResult {
        outcome: CommandOutcome::CommandUnconfirmed(detail.clone()),
        cleanup: CommandCleanup::CommandCleanupUnknown(detail),
    }
}
impl JobState {
    fn next_control(&mut self, myself: &RactorRef<JobMessage>) {
        if self.control.is_some() {
            return;
        }
        let Some(backend) = self.shared.backend.lock().clone() else {
            return;
        };
        let Some((operation, reply)) = self.controls.pop_front() else {
            return;
        };
        let id = self.id.clone();
        let myself = myself.clone();
        self.control = Some(tokio::spawn(async move {
            let result = backend.control(&id, operation).await;
            let _ = myself.cast(JobMessage::ControlFinished(reply, result));
        }));
    }
}
impl Actor for JobActor {
    type Msg = JobMessage;
    type State = JobState;
    type Arguments = JobArguments;

    async fn pre_start(
        &self,
        myself: RactorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let JobArguments {
            id,
            spec,
            shared,
            backend,
        } = args;
        let execution = tokio::spawn(async move {
            let backend = backend.await.unwrap_or_else(|_| {
                Err(CommandError::CommandUnavailable(
                    "native deployment owner unavailable".into(),
                ))
            });
            let _ = myself.cast(JobMessage::BackendReady(backend));
        });
        Ok(JobState {
            id,
            spec: Some(spec),
            shared,
            execution,
            controls: VecDeque::new(),
            control: None,
        })
    }

    async fn handle(
        &self,
        myself: RactorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            JobMessage::BackendReady(Ok(backend)) => {
                *state.shared.backend.lock() = Some(backend.clone());
                #[expect(
                    clippy::expect_used,
                    reason = "BackendReady is sent once by the deployment receiver"
                )]
                let spec = state.spec.take().expect("backend supplied once");
                let id = state.id.clone();
                let shared = state.shared.clone();
                let execution_actor = myself.clone();
                state.execution = tokio::spawn(async move {
                    let result = backend.execute(&id, spec, shared.phase.clone()).await;
                    shared.complete(result);
                    let _ = execution_actor.cast(JobMessage::Finished);
                });
                state.next_control(&myself);
            }
            JobMessage::BackendReady(Err(error)) => {
                state.shared.complete(CommandResult {
                    outcome: CommandOutcome::CommandFailed(format!("{error:?}")),
                    cleanup: CommandCleanup::CommandClean,
                });
                myself.stop(None);
            }
            JobMessage::Finished => myself.stop(None),
            JobMessage::ControlFinished(reply, result) => {
                state.control.take();
                let _ = reply.send(result);
                state.next_control(&myself);
            }
            JobMessage::Control(operation, reply) => {
                let backend = state.shared.backend.lock().clone();
                if matches!(operation, CommandControl::Cancel) {
                    state.shared.phase.send_if_modified(|phase| {
                        if matches!(phase, CommandStatus::CommandFinished(_)) {
                            return false;
                        }
                        *phase = CommandStatus::CommandStopping;
                        true
                    });
                    if let Some(backend) = backend {
                        // Cancellation must bypass stdin backpressure. The backend
                        // accepts intent here; execution observes and settles it.
                        let result = backend.control(&state.id, CommandControl::Cancel).await;
                        let _ = reply.send(result);
                    } else {
                        state.execution.abort();
                        state.shared.complete(CommandResult {
                            outcome: CommandOutcome::CommandCancelled,
                            cleanup: CommandCleanup::CommandClean,
                        });
                        let _ = reply.send(Ok(()));
                        myself.stop(None);
                    }
                } else {
                    state.controls.push_back((operation, reply));
                    state.next_control(&myself);
                }
            }
        }
        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: RactorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if let Some(control) = state.control.take() {
            control.abort();
        }
        if matches!(
            *state.shared.phase.borrow(),
            CommandStatus::CommandFinished(_)
        ) {
            return Ok(());
        }
        let backend = state.shared.backend.lock().clone();
        if let Some(backend) = backend {
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                backend.control(&state.id, CommandControl::Cancel).await?;
                (&mut state.execution)
                    .await
                    .map_err(|error| CommandError::CommandUnavailable(error.to_string()))
            })
            .await;
            state.execution.abort();
            state.shared.complete(unconfirmed(
                "job owner retired before native cleanup was confirmed".into(),
            ));
        } else {
            state.execution.abort();
            state.shared.complete(CommandResult {
                outcome: CommandOutcome::CommandCancelled,
                cleanup: CommandCleanup::CommandClean,
            });
        }
        Ok(())
    }
}
