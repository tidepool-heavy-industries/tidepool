//! Canonical sequential Ractor wrapper for Tidepool actor behavior.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::stream::{FuturesUnordered, StreamExt};
use ractor::{Actor, ActorProcessingErr, ActorRef as RactorRef, SupervisionEvent};

use crate::{
    ActorExitKind, ActorRef, ActorTerminal, ExternalApplicationFailure, ExternalFailureDisposition,
    KernelCallFailure, KernelInvocationFailure, KernelMessage, LocalActorRef, MailboxValue,
    RetainedActorExit,
};
use tidepool_runtime::session::{WorkbenchRequest, WorkbenchResponse};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct KernelBehaviorError {
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct ChildExitNotice {
    pub owner: ActorRef,
    pub child: LocalActorRef,
    pub terminal: ActorTerminal,
}

/// Result of one complete logical behavior operation.
///
/// `Stop` carries the ordinary operation result as well as the immutable
/// actor terminal record. The wrapper can therefore settle a caller or MCP
/// transport before stopping without turning successful actor completion into
/// an error or scheduling a private self-message.
#[derive(Debug)]
pub enum KernelStep<T> {
    Continue(T),
    Stop { output: T, terminal: ActorTerminal },
}

impl<T> KernelStep<T> {
    fn into_parts(self) -> (T, Option<ActorTerminal>) {
        match self {
            Self::Continue(output) => (output, None),
            Self::Stop { output, terminal } => (output, Some(terminal)),
        }
    }
}

#[derive(Clone)]
pub struct KernelContext {
    identity: ActorRef,
    myself: RactorRef<KernelMessage>,
    children: std::sync::Arc<parking_lot::Mutex<HashMap<ractor::ActorId, LocalActorRef>>>,
    directory: LocalActorDirectory,
}

/// Process-local exact-incarnation routing and terminal-observation index.
///
/// This is deliberately not a scheduler or lifecycle state machine. Ractor
/// owns runnable actors and mailboxes; each actor owns its terminal cell. The
/// directory only resolves the identity carried by a live Haskell `ActorRef`
/// to that pair of owners. Entries intentionally live for the root ownership
/// tree's lifetime: an exited exact reference must remain resolvable so any
/// number of late `wait` operations can observe its retained result.
#[derive(Clone, Default)]
pub struct LocalActorDirectory {
    actors: std::sync::Arc<parking_lot::RwLock<HashMap<ActorRef, LocalActorRef>>>,
    sessions: std::sync::Arc<parking_lot::RwLock<HashMap<ActorRef, crate::ActorSessionContext>>>,
}

impl LocalActorDirectory {
    #[must_use]
    pub fn resolve(&self, actor: ActorRef) -> Option<LocalActorRef> {
        self.actors.read().get(&actor).cloned()
    }

    #[must_use]
    pub fn session_context(&self, actor: ActorRef) -> Option<crate::ActorSessionContext> {
        self.sessions.read().get(&actor).cloned()
    }

    fn insert(&self, actor: LocalActorRef) {
        self.actors.write().insert(actor.identity(), actor);
    }
}

impl KernelContext {
    #[must_use]
    pub fn identity(&self) -> ActorRef {
        self.identity
    }

    #[must_use]
    pub fn resolve(&self, actor: ActorRef) -> Option<LocalActorRef> {
        self.directory.resolve(actor)
    }

    pub fn install_session_context(
        &self,
        context: crate::ActorSessionContext,
    ) -> Result<(), KernelBehaviorError> {
        if context.actor != self.identity {
            return Err(KernelBehaviorError {
                detail: format!(
                    "actor {:?} cannot install session context for {:?}",
                    self.identity, context.actor
                ),
            });
        }
        self.directory
            .sessions
            .write()
            .insert(context.actor, context);
        Ok(())
    }

    #[must_use]
    pub fn session_context(&self, actor: ActorRef) -> Option<crate::ActorSessionContext> {
        self.directory.session_context(actor)
    }

    #[must_use]
    pub fn owns_child(&self, actor: ActorRef) -> bool {
        self.children
            .lock()
            .values()
            .any(|child| child.identity() == actor)
    }

    /// Start a linked child and return its exact handle only after startup.
    pub async fn spawn_child<C>(
        &self,
        name: Option<String>,
        behavior: C,
    ) -> Result<LocalActorRef, ractor::SpawnErr>
    where
        C: KernelBehavior,
    {
        let terminal = RetainedActorExit::new();
        let (address, task) = self
            .myself
            .spawn_linked(
                name,
                LocalActor::<C>(PhantomData),
                LocalActorArguments {
                    behavior,
                    terminal: terminal.clone(),
                    directory: self.directory.clone(),
                },
            )
            .await?;
        drop(task);
        let child = LocalActorRef::new(address, terminal);
        self.children
            .lock()
            .insert(child.address().get_id(), child.clone());
        Ok(child)
    }
}

/// Resident execution owned by one sequential local actor.
///
/// The wrapper owns lifecycle, publication, and reply settlement. A behavior
/// owns Haskell/provider/tool execution and returns domain results without
/// gaining access to Ractor's scheduler internals.
pub trait KernelBehavior: Send + 'static {
    fn start<'a>(
        &'a mut self,
        context: &'a KernelContext,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>>;

    fn cast<'a>(
        &'a mut self,
        context: &'a KernelContext,
        sender: ActorRef,
        request: MailboxValue,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>>;

    fn call<'a>(
        &'a mut self,
        context: &'a KernelContext,
        caller: ActorRef,
        ancestry: crate::CallAncestry,
        request: MailboxValue,
    ) -> BoxFuture<'a, Result<KernelStep<MailboxValue>, KernelBehaviorError>>;

    fn mcp<'a>(
        &'a mut self,
        context: &'a KernelContext,
        name: String,
        arguments: serde_json::Value,
    ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>>;

    fn workbench<'a>(
        &'a mut self,
        context: &'a KernelContext,
        request: WorkbenchRequest,
    ) -> BoxFuture<'a, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>>;

    fn external_application_failed<'a>(
        &'a mut self,
        context: &'a KernelContext,
        failure: ExternalApplicationFailure,
    ) -> BoxFuture<'a, ExternalFailureDisposition>;

    fn shutdown<'a>(
        &'a mut self,
        context: &'a KernelContext,
        terminal: &'a ActorTerminal,
    ) -> BoxFuture<'a, Result<(), KernelBehaviorError>>;

    /// Observe the immutable terminal result after cleanup and publication.
    ///
    /// Lifecycle adapters belong here rather than in `shutdown`: consumers
    /// must never observe retirement before `wait` can retrieve the retained
    /// terminal result.
    fn stopped<'a>(
        &'a mut self,
        context: &'a KernelContext,
        terminal: &'a ActorTerminal,
    ) -> BoxFuture<'a, ()>;

    fn child_exited(&mut self, notice: ChildExitNotice) -> BoxFuture<'_, ()>;
}

pub struct LocalActor<B>(PhantomData<fn() -> B>);

pub struct LocalActorArguments<B> {
    pub behavior: B,
    pub terminal: RetainedActorExit,
    pub directory: LocalActorDirectory,
}

pub struct LocalActorState<B> {
    context: KernelContext,
    behavior: B,
    terminal: RetainedActorExit,
}

impl<B> Actor for LocalActor<B>
where
    B: KernelBehavior,
{
    type Msg = KernelMessage;
    type State = LocalActorState<B>;
    type Arguments = LocalActorArguments<B>;

    async fn pre_start(
        &self,
        myself: RactorRef<Self::Msg>,
        arguments: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let identity = ActorRef::first(crate::ActorId(myself.get_id().pid()));
        let context = KernelContext {
            identity,
            myself,
            children: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
            directory: arguments.directory,
        };
        let mut state = LocalActorState {
            context,
            behavior: arguments.behavior,
            terminal: arguments.terminal,
        };
        state.context.directory.insert(LocalActorRef::new(
            state.context.myself.clone(),
            state.terminal.clone(),
        ));
        match state.behavior.start(&state.context).await {
            Ok(KernelStep::Continue(())) => {}
            Ok(KernelStep::Stop { terminal, .. }) => {
                finish_actor(&state.context.myself.clone(), &mut state, terminal).await;
            }
            Err(error) => {
                let terminal = failed_terminal(format!("actor startup failed: {error}"));
                finish_actor(&state.context.myself.clone(), &mut state, terminal).await;
                return Err(Box::new(error));
            }
        }
        Ok(state)
    }

    async fn handle(
        &self,
        myself: RactorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            KernelMessage::Cast { sender, request } => {
                match state.behavior.cast(&state.context, sender, request).await {
                    Ok(step) => finish_after_step(&myself, state, step).await,
                    Err(error) => {
                        fail_actor(&myself, state, format!("actor cast failed: {error}")).await
                    }
                }
            }
            KernelMessage::Call {
                caller,
                ancestry,
                request,
                reply,
            } => match ancestry.enter(state.context.identity) {
                Err(error) => {
                    let _ = reply.send(Err(error));
                }
                Ok(ancestry) => match state
                    .behavior
                    .call(&state.context, caller, ancestry, request)
                    .await
                {
                    Ok(step) => {
                        let (value, terminal) = step.into_parts();
                        let _ = reply.send(Ok(value));
                        if let Some(terminal) = terminal {
                            finish_actor(&myself, state, terminal).await;
                        }
                    }
                    Err(error) => {
                        let detail = error.to_string();
                        let _ = reply.send(Err(KernelCallFailure::Handler {
                            actor: state.context.identity,
                            detail: detail.clone(),
                        }));
                        fail_actor(&myself, state, format!("actor call failed: {detail}")).await;
                    }
                },
            },
            KernelMessage::Mcp {
                name,
                arguments,
                reply,
            } => match state.behavior.mcp(&state.context, name, arguments).await {
                Ok(step) => {
                    let (output, terminal) = step.into_parts();
                    let _ = reply.send(Ok(output));
                    if let Some(terminal) = terminal {
                        finish_actor(&myself, state, terminal).await;
                    }
                }
                Err(error) => {
                    let _ = reply.send(Err(error));
                }
            },
            KernelMessage::Workbench { request, reply } => {
                match state.behavior.workbench(&state.context, request).await {
                    Ok(step) => {
                        let (output, terminal) = step.into_parts();
                        let _ = reply.send(Ok(output));
                        if let Some(terminal) = terminal {
                            finish_actor(&myself, state, terminal).await;
                        }
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            KernelMessage::ExternalApplicationFailed { failure, reply } => {
                let detail = format!("native actor application failed: {}", failure.detail);
                let disposition = state
                    .behavior
                    .external_application_failed(&state.context, failure)
                    .await;
                let _ = reply.send(disposition);
                if disposition == ExternalFailureDisposition::Applied {
                    fail_actor(&myself, state, detail).await;
                }
            }
            KernelMessage::Shutdown { terminal, reply } => {
                let terminal = finish_actor(&myself, state, terminal).await;
                let _ = reply.send(terminal);
            }
        }
        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: RactorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let (cell, observed) = match event {
            SupervisionEvent::ActorStarted(_) | SupervisionEvent::ProcessGroupChanged(_) => {
                return Ok(())
            }
            SupervisionEvent::ActorTerminated(cell, _, reason) => {
                let summary = reason.unwrap_or_else(|| {
                    "linked child stopped without publishing a terminal result".into()
                });
                (cell, failed_terminal(summary))
            }
            SupervisionEvent::ActorFailed(cell, error) => (
                cell,
                failed_terminal(format!("linked child actor failed: {error}")),
            ),
        };
        let child = state.context.children.lock().get(&cell.get_id()).cloned();
        let Some(child) = child else {
            tracing::warn!(child = %cell.get_id(), "received lifecycle event for an unregistered linked child");
            return Ok(());
        };
        if child.terminal().get().is_none() {
            publish_terminal(child.terminal(), &observed);
        }
        let Some(terminal) = child.terminal().get() else {
            unreachable!("supervision publishes or observes the child terminal result");
        };
        state
            .behavior
            .child_exited(ChildExitNotice {
                owner: state.context.identity,
                child,
                terminal,
            })
            .await;
        Ok(())
    }
}

/// Spawn one root through the same actor implementation used for children.
pub async fn spawn_local_actor<B>(
    name: Option<String>,
    behavior: B,
) -> Result<(LocalActorRef, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr>
where
    B: KernelBehavior,
{
    let terminal = RetainedActorExit::new();
    let directory = LocalActorDirectory::default();
    let (address, task) = Actor::spawn(
        name,
        LocalActor::<B>(PhantomData),
        LocalActorArguments {
            behavior,
            terminal: terminal.clone(),
            directory,
        },
    )
    .await?;
    Ok((LocalActorRef::new(address, terminal), task))
}

async fn fail_actor<B>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    detail: String,
) where
    B: KernelBehavior,
{
    finish_actor(myself, state, failed_terminal(detail)).await;
}

async fn finish_after_step<B>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    step: KernelStep<()>,
) where
    B: KernelBehavior,
{
    if let KernelStep::Stop { terminal, .. } = step {
        finish_actor(myself, state, terminal).await;
    }
}

async fn finish_actor<B>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    requested: ActorTerminal,
) -> ActorTerminal
where
    B: KernelBehavior,
{
    shutdown_children(&state.context, Duration::from_secs(15)).await;
    let terminal = match state.behavior.shutdown(&state.context, &requested).await {
        Ok(()) => requested,
        Err(error) => failed_terminal(format!("actor shutdown failed: {error}")),
    };
    publish_terminal(&state.terminal, &terminal);
    state.behavior.stopped(&state.context, &terminal).await;
    myself.stop(Some(terminal.summary.clone()));
    terminal
}

fn publish_terminal(retained: &RetainedActorExit, terminal: &ActorTerminal) {
    if let Err(error) = retained.publish(terminal.clone()) {
        tracing::error!(?terminal, existing = ?error.existing, "actor terminal result published twice");
    }
}

fn failed_terminal(summary: String) -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Failed,
        summary,
    }
}

async fn shutdown_children(context: &KernelContext, timeout: Duration) {
    let children: Vec<_> = context.children.lock().values().cloned().collect();
    let mut shutdowns = FuturesUnordered::new();
    for child in children {
        shutdowns.push(async move {
            if child.terminal().get().is_some() {
                return;
            }
            let requested = ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "owner actor stopped".into(),
            };
            let result = child
                .address()
                .call(
                    |reply| KernelMessage::Shutdown {
                        terminal: requested.clone(),
                        reply,
                    },
                    Some(timeout),
                )
                .await;
            if !matches!(result, Ok(ractor::rpc::CallResult::Success(_))) {
                if child.terminal().get().is_none() {
                    publish_terminal(child.terminal(), &requested);
                }
                child.address().kill();
            }
        });
    }
    while shutdowns.next().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use parking_lot::Mutex;
    use tidepool_repr::SessionId;
    use tidepool_runtime::session::{WorkbenchResponse, WorkbenchRunStatus};
    use tokio::sync::{oneshot, Notify};

    use super::*;

    struct ProbeBehavior {
        calls: Arc<Mutex<Vec<&'static str>>>,
        release_first: Arc<Notify>,
        fail_cast: bool,
        spawned_child: Arc<Mutex<Option<LocalActorRef>>>,
        child_exits: Arc<Mutex<Vec<ActorTerminal>>>,
    }

    impl KernelBehavior for ProbeBehavior {
        fn start(
            &mut self,
            _context: &KernelContext,
        ) -> BoxFuture<'_, Result<KernelStep<()>, KernelBehaviorError>> {
            Box::pin(async { Ok(KernelStep::Continue(())) })
        }

        fn cast(
            &mut self,
            _context: &KernelContext,
            _sender: ActorRef,
            _request: MailboxValue,
        ) -> BoxFuture<'_, Result<KernelStep<()>, KernelBehaviorError>> {
            let fail = self.fail_cast;
            Box::pin(async move {
                if fail {
                    Err(KernelBehaviorError {
                        detail: "cast probe".into(),
                    })
                } else {
                    Ok(KernelStep::Continue(()))
                }
            })
        }

        fn call(
            &mut self,
            _context: &KernelContext,
            _caller: ActorRef,
            _ancestry: crate::CallAncestry,
            request: MailboxValue,
        ) -> BoxFuture<'_, Result<KernelStep<MailboxValue>, KernelBehaviorError>> {
            Box::pin(async move { Ok(KernelStep::Continue(request)) })
        }

        fn mcp<'a>(
            &'a mut self,
            context: &'a KernelContext,
            name: String,
            _arguments: serde_json::Value,
        ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>> {
            Box::pin(async move {
                if name == "spawn" {
                    let child = context
                        .spawn_child(None, FailingChild)
                        .await
                        .map_err(|error| KernelInvocationFailure::Failed {
                            actor: context.identity(),
                            detail: error.to_string(),
                        })?;
                    *self.spawned_child.lock() = Some(child);
                } else if name == "finish" {
                    return Ok(KernelStep::Stop {
                        output: serde_json::Value::String(name),
                        terminal: ActorTerminal {
                            kind: ActorExitKind::Completed,
                            summary: "finished through MCP".into(),
                        },
                    });
                } else if name == "first" {
                    self.calls.lock().push("first-start");
                    self.release_first.notified().await;
                    self.calls.lock().push("first-end");
                } else {
                    self.calls.lock().push("second");
                }
                Ok(KernelStep::Continue(serde_json::Value::String(name)))
            })
        }

        fn workbench(
            &mut self,
            _context: &KernelContext,
            _request: WorkbenchRequest,
        ) -> BoxFuture<'_, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>> {
            Box::pin(async {
                Ok(KernelStep::Continue(WorkbenchResponse {
                    status: WorkbenchRunStatus::Committed,
                    items: Vec::new(),
                    next_index: 0,
                    total: 0,
                }))
            })
        }

        fn external_application_failed(
            &mut self,
            _context: &KernelContext,
            _failure: ExternalApplicationFailure,
        ) -> BoxFuture<'_, ExternalFailureDisposition> {
            Box::pin(async { ExternalFailureDisposition::Applied })
        }

        fn shutdown(
            &mut self,
            _context: &KernelContext,
            _terminal: &ActorTerminal,
        ) -> BoxFuture<'_, Result<(), KernelBehaviorError>> {
            Box::pin(async move {
                self.calls.lock().push("shutdown");
                Ok(())
            })
        }

        fn stopped(
            &mut self,
            _context: &KernelContext,
            _terminal: &ActorTerminal,
        ) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }

        fn child_exited(&mut self, notice: ChildExitNotice) -> BoxFuture<'_, ()> {
            Box::pin(async move { self.child_exits.lock().push(notice.terminal) })
        }
    }

    struct FailingChild;

    impl KernelBehavior for FailingChild {
        fn start(
            &mut self,
            _context: &KernelContext,
        ) -> BoxFuture<'_, Result<KernelStep<()>, KernelBehaviorError>> {
            Box::pin(async { Ok(KernelStep::Continue(())) })
        }

        fn cast(
            &mut self,
            _context: &KernelContext,
            _sender: ActorRef,
            _request: MailboxValue,
        ) -> BoxFuture<'_, Result<KernelStep<()>, KernelBehaviorError>> {
            Box::pin(async {
                Err(KernelBehaviorError {
                    detail: "child failed".into(),
                })
            })
        }

        fn call(
            &mut self,
            _context: &KernelContext,
            _caller: ActorRef,
            _ancestry: crate::CallAncestry,
            request: MailboxValue,
        ) -> BoxFuture<'_, Result<KernelStep<MailboxValue>, KernelBehaviorError>> {
            Box::pin(async move { Ok(KernelStep::Continue(request)) })
        }

        fn mcp<'a>(
            &'a mut self,
            context: &'a KernelContext,
            _name: String,
            _arguments: serde_json::Value,
        ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>> {
            Box::pin(async move {
                Err(KernelInvocationFailure::Rejected {
                    actor: context.identity(),
                    detail: "child has no MCP policy".into(),
                })
            })
        }

        fn workbench<'a>(
            &'a mut self,
            context: &'a KernelContext,
            _request: WorkbenchRequest,
        ) -> BoxFuture<'a, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>> {
            Box::pin(async move {
                Err(KernelInvocationFailure::Rejected {
                    actor: context.identity(),
                    detail: "child has no workbench".into(),
                })
            })
        }

        fn external_application_failed(
            &mut self,
            _context: &KernelContext,
            _failure: ExternalApplicationFailure,
        ) -> BoxFuture<'_, ExternalFailureDisposition> {
            Box::pin(async { ExternalFailureDisposition::Applied })
        }

        fn shutdown(
            &mut self,
            _context: &KernelContext,
            _terminal: &ActorTerminal,
        ) -> BoxFuture<'_, Result<(), KernelBehaviorError>> {
            Box::pin(async { Ok(()) })
        }

        fn stopped(
            &mut self,
            _context: &KernelContext,
            _terminal: &ActorTerminal,
        ) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }

        fn child_exited(&mut self, _notice: ChildExitNotice) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    struct ProbeFixture {
        behavior: ProbeBehavior,
        calls: Arc<Mutex<Vec<&'static str>>>,
        release: Arc<Notify>,
        spawned_child: Arc<Mutex<Option<LocalActorRef>>>,
        child_exits: Arc<Mutex<Vec<ActorTerminal>>>,
    }

    fn behavior(fail_cast: bool) -> ProbeFixture {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let release = Arc::new(Notify::new());
        let spawned_child = Arc::new(Mutex::new(None));
        let child_exits = Arc::new(Mutex::new(Vec::new()));
        ProbeFixture {
            behavior: ProbeBehavior {
                calls: Arc::clone(&calls),
                release_first: Arc::clone(&release),
                fail_cast,
                spawned_child: Arc::clone(&spawned_child),
                child_exits: Arc::clone(&child_exits),
            },
            calls,
            release,
            spawned_child,
            child_exits,
        }
    }

    #[tokio::test]
    async fn one_actor_never_reenters_while_an_operation_is_pending() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let (first_tx, first_rx) = oneshot::channel();
        let (second_tx, second_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Mcp {
                name: "first".into(),
                arguments: serde_json::Value::Null,
                reply: first_tx.into(),
            })
            .expect("queue first");
        actor
            .address()
            .send_message(KernelMessage::Mcp {
                name: "second".into(),
                arguments: serde_json::Value::Null,
                reply: second_tx.into(),
            })
            .expect("queue second");

        tokio::task::yield_now().await;
        assert_eq!(&*fixture.calls.lock(), &["first-start"]);
        fixture.release.notify_one();
        assert_eq!(first_rx.await.expect("first reply").unwrap(), "first");
        assert_eq!(second_rx.await.expect("second reply").unwrap(), "second");
        assert_eq!(
            &*fixture.calls.lock(),
            &["first-start", "first-end", "second"]
        );

        let terminal = ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "done".into(),
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Shutdown {
                terminal: terminal.clone(),
                reply: shutdown_tx.into(),
            })
            .expect("queue shutdown");
        assert_eq!(shutdown_rx.await.expect("shutdown reply"), terminal);
        task.await.expect("actor task");
        assert_eq!(actor.terminal().wait().await, terminal);
    }

    #[tokio::test]
    async fn behavior_failure_publishes_once_and_releases_message_custody() {
        let fixture = behavior(true);
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let dropped = Arc::new(AtomicUsize::new(0));
        actor
            .address()
            .send_message(KernelMessage::Cast {
                sender: ActorRef::first(crate::ActorId(99)),
                request: MailboxValue::probe(SessionId(1), Arc::clone(&dropped)),
            })
            .expect("queue cast");

        let terminal = actor.terminal().wait().await;
        task.await.expect("actor task");
        assert_eq!(terminal.kind, ActorExitKind::Failed);
        assert!(terminal.summary.contains("cast probe"));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn successful_operation_can_reply_and_publish_terminal_in_one_step() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let reply = actor
            .address()
            .call(
                |reply| KernelMessage::Mcp {
                    name: "finish".into(),
                    arguments: serde_json::Value::Null,
                    reply,
                },
                None,
            )
            .await
            .expect("RPC transport")
            .expect("actor replied")
            .expect("successful actor reply");
        assert_eq!(reply, serde_json::Value::String("finish".into()));
        task.await.expect("actor task");
        assert_eq!(
            actor.terminal().get(),
            Some(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "finished through MCP".into(),
            })
        );
        assert_eq!(&*fixture.calls.lock(), &["shutdown"]);
    }

    #[tokio::test]
    async fn child_failure_is_retained_and_notifies_without_killing_owner() {
        let fixture = behavior(false);
        let (owner, owner_task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn owner");
        let (spawn_tx, spawn_rx) = oneshot::channel();
        owner
            .address()
            .send_message(KernelMessage::Mcp {
                name: "spawn".into(),
                arguments: serde_json::Value::Null,
                reply: spawn_tx.into(),
            })
            .expect("request child");
        spawn_rx.await.expect("spawn reply").expect("spawn result");
        let child = fixture.spawned_child.lock().clone().expect("child handle");
        child
            .address()
            .send_message(KernelMessage::Cast {
                sender: owner.identity(),
                request: MailboxValue::probe(SessionId(1), Arc::new(AtomicUsize::new(0))),
            })
            .expect("fail child");

        assert_eq!(child.terminal().wait().await.kind, ActorExitKind::Failed);
        for _ in 0..20 {
            if !fixture.child_exits.lock().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(fixture.child_exits.lock().len(), 1);

        let (ping_tx, ping_rx) = oneshot::channel();
        owner
            .address()
            .send_message(KernelMessage::Mcp {
                name: "second".into(),
                arguments: serde_json::Value::Null,
                reply: ping_tx.into(),
            })
            .expect("owner remains callable");
        assert_eq!(ping_rx.await.expect("owner reply").unwrap(), "second");

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        owner
            .address()
            .send_message(KernelMessage::Shutdown {
                terminal: ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "owner done".into(),
                },
                reply: shutdown_tx.into(),
            })
            .expect("shutdown owner");
        shutdown_rx.await.expect("shutdown reply");
        owner_task.await.expect("owner task");
    }
}
