//! Canonical sequential Ractor wrapper for Tidepool actor behavior.

use std::collections::{HashMap, VecDeque};
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
/// actor terminal record. The wrapper can therefore settle a caller or tool
/// host before stopping. `ContinueLater` is the one explicit scheduler handoff:
/// settle first, then resume actor-owned work through the same mailbox.
#[derive(Debug)]
pub enum KernelStep<T> {
    Continue(T),
    /// Settle the current caller, then resume actor-owned work from a fresh
    /// mailbox turn. Interactive sessions use this when the model returns a
    /// live Haskell action.
    ContinueLater(T),
    Stop {
        output: T,
        terminal: ActorTerminal,
    },
}

impl<T> KernelStep<T> {
    fn into_parts(self) -> (T, Option<ActorTerminal>, bool) {
        match self {
            Self::Continue(output) => (output, None, false),
            Self::ContinueLater(output) => (output, None, true),
            Self::Stop { output, terminal } => (output, Some(terminal), false),
        }
    }
}

#[derive(Clone)]
pub struct KernelContext {
    identity: ActorRef,
    myself: RactorRef<KernelMessage>,
    children: std::sync::Arc<parking_lot::Mutex<HashMap<ractor::ActorId, LocalActorRef>>>,
    directory: LocalActorDirectory,
    forgotten_children: std::sync::Arc<parking_lot::Mutex<crate::CleanupComponentOutcome>>,
    child_admission_closed: std::sync::Arc<tokio::sync::RwLock<bool>>,
}

/// Process-local exact-incarnation routing and terminal-observation index.
///
/// This is deliberately not a scheduler or lifecycle state machine. Ractor
/// owns runnable actors and mailboxes; each actor owns its terminal cell. The
/// directory only resolves the identity carried by a live Haskell `ActorRef`
/// to that pair of owners. Entries intentionally live for the routing
/// domain's lifetime: an exited exact reference must remain resolvable so any
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

    fn forget_terminal(&self, actor: ActorRef) -> bool {
        let mut actors = self.actors.write();
        let terminal = actors
            .get(&actor)
            .is_some_and(|actor| actor.terminal().get().is_some());
        if terminal {
            actors.remove(&actor);
            self.sessions.write().remove(&actor);
        }
        terminal
    }
}

impl KernelContext {
    pub(crate) fn requested_shutdown(&self) -> Option<ActorTerminal> {
        self.directory
            .resolve(self.identity)?
            .terminal()
            .requested_shutdown()
    }

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

    /// Release routing and terminal metadata for an exact, already-terminal
    /// actor. Live Haskell handles become explicitly unavailable afterward.
    pub fn forget_terminal_actor(&self, actor: ActorRef) -> bool {
        let mut children = self.children.lock();
        let child = children
            .values()
            .find(|child| child.identity() == actor)
            .cloned();
        let forgotten = self.directory.forget_terminal(actor);
        if forgotten {
            if let Some(child) = child {
                if !child
                    .terminal()
                    .cleanup()
                    .is_some_and(|outcome| outcome.actor() == actor && outcome.is_confirmed())
                {
                    let mut retained = self.forgotten_children.lock();
                    *retained = combine_cleanup(
                        retained.clone(),
                        crate::CleanupComponentOutcome::Unconfirmed(format!(
                            "forgotten child {actor:?} lacked confirmed cleanup"
                        )),
                    );
                }
            }
            children.retain(|_, child| child.identity() != actor);
        }
        forgotten
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
        // Hold admission through registration so retirement cannot miss a
        // child whose startup is already in flight.
        let admission = self.child_admission_closed.read().await;
        if *admission {
            return Err(ractor::SpawnErr::StartupFailed(
                std::io::Error::other("actor child admission is closed").into(),
            ));
        }
        let mut custody = StartupCustody {
            retained: self.forgotten_children.clone(),
            accounted: false,
        };
        let terminal = RetainedActorExit::new();
        let spawned = self
            .myself
            .spawn_linked(
                name,
                LocalActor::<C>(PhantomData),
                LocalActorArguments {
                    behavior,
                    terminal: terminal.clone(),
                    directory: self.directory.clone(),
                    incarnation: self.identity.incarnation,
                },
            )
            .await;
        let (address, task) = match spawned {
            Ok(spawned) => spawned,
            Err(error) => {
                let mut retained = self.forgotten_children.lock();
                *retained = combine_cleanup(
                    retained.clone(),
                    crate::CleanupComponentOutcome::Unconfirmed(format!(
                        "child startup failed without retained cleanup: {error}"
                    )),
                );
                custody.accounted = true;
                return Err(error);
            }
        };
        drop(task);
        let child = LocalActorRef::new_in_incarnation(address, terminal, self.identity.incarnation);
        self.children
            .lock()
            .insert(child.address().get_id(), child.clone());
        custody.accounted = true;
        Ok(child)
    }
}

/// Resident execution owned by one sequential local actor.
///
/// The wrapper owns lifecycle, publication, and reply settlement. A behavior
/// owns Haskell/provider/tool execution and returns domain results without
/// gaining access to Ractor's scheduler internals.
pub trait KernelBehavior: Send + 'static {
    /// Execute one ready watch continuation on this actor's ordinary turn queue.
    fn route<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _watch: crate::WatchId,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async {
            Err(KernelBehaviorError {
                detail: "actor does not support watch continuations".into(),
            })
        })
    }

    /// Whether the installed behavior is currently parked on its authored
    /// mailbox receiver. The wrapper retains cast/call custody while an
    /// external interaction temporarily occupies that continuation.
    fn accepts_mailbox(&self) -> bool {
        true
    }

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

    fn tool<'a>(
        &'a mut self,
        context: &'a KernelContext,
        invocation: tidepool_tool::ToolInvocation,
    ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>>;

    fn workbench<'a>(
        &'a mut self,
        context: &'a KernelContext,
        request: WorkbenchRequest,
    ) -> BoxFuture<'a, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>>;

    /// Continue work deliberately yielded after its initiating caller was
    /// settled. Behaviors which never return 'ContinueLater' own no pending
    /// continuation and keep this rejecting default.
    /// Release effects waiting for a durably recorded enclosing tool result.
    fn abort_pending_forks<'a>(
        &'a mut self,
        _context: &'a KernelContext,
    ) -> BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async { Ok(()) })
    }

    fn tool_completed<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async { Ok(()) })
    }

    /// Start an admitted child with its final inherited scope.
    fn release_fork<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _scope: tidepool_codegen::scope::ScopeId,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async {
            Err(KernelBehaviorError {
                detail: "actor has no deferred fork".into(),
            })
        })
    }

    fn resume<'a>(
        &'a mut self,
        _context: &'a KernelContext,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async {
            Err(KernelBehaviorError {
                detail: "actor received an internal resume without pending work".into(),
            })
        })
    }

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

    /// Explicit component evidence. Generic behavior success cannot attest a
    /// resident realm it does not own; resident behavior overrides this method.
    fn shutdown_components<'a>(
        &'a mut self,
        context: &'a KernelContext,
        terminal: &'a ActorTerminal,
    ) -> BoxFuture<
        'a,
        (
            crate::CleanupComponentOutcome,
            crate::CleanupComponentOutcome,
        ),
    > {
        Box::pin(async move {
            let hook = match self.shutdown(context, terminal).await {
                Ok(()) => crate::CleanupComponentOutcome::Confirmed,
                Err(error) => crate::CleanupComponentOutcome::Unconfirmed(error.to_string()),
            };
            (hook, crate::CleanupComponentOutcome::Unsupported)
        })
    }

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
    /// Durable runtime epoch shared by every actor in one local host.
    ///
    /// Ractor process IDs may be reused after a host restart. Pairing them
    /// with the host epoch prevents an old exact handle from addressing a new
    /// actor that happens to receive the same process ID.
    pub incarnation: crate::Incarnation,
}

#[derive(Default)]
enum HostedAdmission {
    #[default]
    Open,
    Sealed,
    Closing,
}

pub struct LocalActorState<B> {
    hosted_admission: HostedAdmission,
    context: KernelContext,
    behavior: B,
    terminal: RetainedActorExit,
    deferred_mailbox: VecDeque<KernelMessage>,
    mailbox_drain_scheduled: bool,
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
        let identity = ActorRef {
            id: crate::ActorId(myself.get_id().pid()),
            incarnation: arguments.incarnation,
        };
        let context = KernelContext {
            identity,
            myself,
            children: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
            directory: arguments.directory,
            child_admission_closed: std::sync::Arc::new(tokio::sync::RwLock::new(false)),
            forgotten_children: std::sync::Arc::new(parking_lot::Mutex::new(
                crate::CleanupComponentOutcome::Confirmed,
            )),
        };
        let mut state = LocalActorState {
            context,
            behavior: arguments.behavior,
            terminal: arguments.terminal,
            deferred_mailbox: VecDeque::new(),
            mailbox_drain_scheduled: false,
            hosted_admission: HostedAdmission::Open,
        };
        state
            .context
            .directory
            .insert(LocalActorRef::new_in_incarnation(
                state.context.myself.clone(),
                state.terminal.clone(),
                state.context.identity.incarnation,
            ));
        match state.behavior.start(&state.context).await {
            Ok(KernelStep::Continue(())) => {}
            Ok(KernelStep::ContinueLater(())) => {
                state.context.myself.send_message(KernelMessage::Resume)?;
            }
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
        let message = match message {
            message @ (KernelMessage::Cast { .. } | KernelMessage::Call { .. })
                if !state.deferred_mailbox.is_empty() || !state.behavior.accepts_mailbox() =>
            {
                state.deferred_mailbox.push_back(message);
                schedule_deferred_mailbox(&myself, state)?;
                return Ok(());
            }
            KernelMessage::DrainMailbox => {
                state.mailbox_drain_scheduled = false;
                if !state.behavior.accepts_mailbox() {
                    return Ok(());
                }
                let Some(message) = state.deferred_mailbox.pop_front() else {
                    return Ok(());
                };
                message
            }
            message => message,
        };
        match message {
            KernelMessage::SealHostedWork { reply } => {
                if matches!(state.hosted_admission, HostedAdmission::Closing) {
                    drop(reply);
                    return Ok(());
                }
                state.hosted_admission = HostedAdmission::Sealed;
                let _ = reply.send(crate::HostedWorkSeal {
                    actor: state.context.identity,
                });
            }
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
                        settle_step(&myself, state, step, |value| {
                            let _ = reply.send(Ok(value));
                        })
                        .await;
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
            KernelMessage::Tool { invocation, reply } => {
                if !matches!(state.hosted_admission, HostedAdmission::Open) {
                    let _ = reply.send(Err(KernelInvocationFailure::Rejected {
                        actor: state.context.identity,
                        detail: "hosted work admission is sealed".into(),
                    }));
                    return Ok(());
                }

                match state.behavior.tool(&state.context, invocation).await {
                    Ok(step) => {
                        settle_step(&myself, state, step, |output| {
                            let _ = reply.send(Ok(output));
                        })
                        .await;
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            KernelMessage::AbortPendingForks { reply } => {
                if !matches!(state.hosted_admission, HostedAdmission::Open) {
                    let _ = reply.send(Err(KernelInvocationFailure::Rejected {
                        actor: state.context.identity,
                        detail: "hosted work admission is sealed".into(),
                    }));
                    return Ok(());
                }

                let result = state
                    .behavior
                    .abort_pending_forks(&state.context)
                    .await
                    .map(|()| serde_json::Value::Null)
                    .map_err(|error| KernelInvocationFailure::Rejected {
                        actor: state.context.identity,
                        detail: error.to_string(),
                    });
                let _ = reply.send(result);
            }
            KernelMessage::ToolCompleted { boundary, reply } => {
                if matches!(state.hosted_admission, HostedAdmission::Closing) {
                    let _ = reply.send(Err(KernelInvocationFailure::Rejected {
                        actor: state.context.identity,
                        detail: "hosted completion boundary is closed".into(),
                    }));
                    return Ok(());
                }
                let result = state
                    .behavior
                    .tool_completed(&state.context, boundary)
                    .await
                    .map(|()| serde_json::Value::Null)
                    .map_err(|error| KernelInvocationFailure::Rejected {
                        actor: state.context.identity,
                        detail: error.to_string(),
                    });
                let _ = reply.send(result);
            }
            KernelMessage::ReleaseFork { scope } => {
                if matches!(state.hosted_admission, HostedAdmission::Closing) {
                    return Ok(());
                }
                match state.behavior.release_fork(&state.context, scope).await {
                    Ok(step) => finish_after_step(&myself, state, step).await,
                    Err(error) => {
                        fail_actor(
                            &myself,
                            state,
                            format!("deferred fork startup failed: {error}"),
                        )
                        .await
                    }
                }
            }
            KernelMessage::Workbench { request, reply } => {
                if !matches!(state.hosted_admission, HostedAdmission::Open) {
                    let _ = reply.send(Err(KernelInvocationFailure::Rejected {
                        actor: state.context.identity,
                        detail: "hosted work admission is sealed".into(),
                    }));
                    return Ok(());
                }

                match state.behavior.workbench(&state.context, request).await {
                    Ok(step) => {
                        settle_step(&myself, state, step, |output| {
                            let _ = reply.send(Ok(output));
                        })
                        .await;
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            KernelMessage::DrainMailbox => {
                unreachable!("mailbox drain messages are normalized before dispatch")
            }
            KernelMessage::RouteReady { watch } => {
                match state.behavior.route(&state.context, watch).await {
                    Ok(step) => finish_after_step(&myself, state, step).await,
                    Err(error) => tracing::error!(?watch, %error, "watch continuation rejected"),
                }
            }
            KernelMessage::Resume => match state.behavior.resume(&state.context).await {
                Ok(step) => finish_after_step(&myself, state, step).await,
                Err(error) => {
                    fail_actor(
                        &myself,
                        state,
                        format!("actor continuation failed: {error}"),
                    )
                    .await
                }
            },
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
        schedule_deferred_mailbox(&myself, state)?;
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

fn schedule_deferred_mailbox<B>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
) -> Result<(), ActorProcessingErr>
where
    B: KernelBehavior,
{
    if state.terminal.get().is_none()
        && state.behavior.accepts_mailbox()
        && !state.deferred_mailbox.is_empty()
        && !state.mailbox_drain_scheduled
    {
        state.mailbox_drain_scheduled = true;
        myself
            .send_message(KernelMessage::DrainMailbox)
            .map_err(|error| Box::new(error) as ActorProcessingErr)?;
    }
    Ok(())
}

/// Spawn one root through the same actor implementation used for children.
pub async fn spawn_local_actor<B>(
    name: Option<String>,
    behavior: B,
) -> Result<(LocalActorRef, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr>
where
    B: KernelBehavior,
{
    spawn_local_actor_in_incarnation(name, behavior, crate::Incarnation::FIRST).await
}

/// Spawn one root in an explicitly claimed durable host incarnation.
///
/// Ordinary embedders and tests can use [`spawn_local_actor`]. A host whose
/// actor IDs must remain exact across process restarts claims an incarnation
/// durably and supplies it here. Children inherit this value from their
/// parent, so authority cannot accidentally cross a restarted host boundary.
pub async fn spawn_local_actor_in_incarnation<B>(
    name: Option<String>,
    behavior: B,
    incarnation: crate::Incarnation,
) -> Result<(LocalActorRef, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr>
where
    B: KernelBehavior,
{
    spawn_local_actor_in_directory(name, behavior, incarnation, LocalActorDirectory::default())
        .await
}

/// Admit an independent root to an existing routing domain. Ractor still owns
/// supervision; sharing a directory does not make one root another's child.
pub(crate) async fn spawn_local_actor_in_directory<B>(
    name: Option<String>,
    behavior: B,
    incarnation: crate::Incarnation,
    directory: LocalActorDirectory,
) -> Result<(LocalActorRef, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr>
where
    B: KernelBehavior,
{
    let terminal = RetainedActorExit::new();
    let (address, task) = Actor::spawn(
        name,
        LocalActor::<B>(PhantomData),
        LocalActorArguments {
            behavior,
            terminal: terminal.clone(),
            directory,
            incarnation,
        },
    )
    .await?;
    Ok((
        LocalActorRef::new_in_incarnation(address, terminal, incarnation),
        task,
    ))
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
    settle_step(myself, state, step, |_| {}).await;
}

/// Settle the initiating boundary before applying the actor-owned disposition.
/// Calls, hosted tools, workbench requests, and cast/resume all share this one
/// ordering rule; adding a new transport must not reimplement it.
async fn settle_step<B, T>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    step: KernelStep<T>,
    settle: impl FnOnce(T),
) where
    B: KernelBehavior,
{
    let (output, terminal, resume) = step.into_parts();
    settle(output);
    if let Some(terminal) = terminal {
        finish_actor(myself, state, terminal).await;
    } else if resume {
        if let Err(error) = myself.send_message(KernelMessage::Resume) {
            fail_actor(
                myself,
                state,
                format!("could not schedule actor continuation: {error}"),
            )
            .await;
        }
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
    if let Some(terminal) = state.terminal.get() {
        return terminal;
    }
    // Serialized mailbox execution closes completion admission here: queued
    // completion cannot run across this snapshot or after stopping the actor.
    state.hosted_admission = HostedAdmission::Closing;
    // Wait for admitted startup to register, then permanently reject creation,
    // including through cloned contexts and shutdown hooks.
    *state.context.child_admission_closed.write().await = true;
    let children = shutdown_children(&state.context, Duration::from_secs(15)).await;
    let (hook, realm) = state
        .behavior
        .shutdown_components(&state.context, &requested)
        .await;
    let terminal = match (&hook, &realm) {
        (crate::CleanupComponentOutcome::Unconfirmed(error), _)
        | (_, crate::CleanupComponentOutcome::Unconfirmed(error)) => {
            failed_terminal(format!("actor shutdown failed: {error}"))
        }
        _ => requested,
    };
    state
        .terminal
        .retain_cleanup(crate::ResidentCleanupOutcome {
            actor: state.context.identity,
            hook,
            realm,
            children,
        });
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

fn combine_cleanup(
    left: crate::CleanupComponentOutcome,
    right: crate::CleanupComponentOutcome,
) -> crate::CleanupComponentOutcome {
    use crate::CleanupComponentOutcome::*;
    match (left, right) {
        (Confirmed, value) | (value, Confirmed) => value,
        (Unconfirmed(a), Unconfirmed(b)) => Unconfirmed(format!("{a}; {b}")),
        (Unsupported, value) | (value, Unsupported) => match value {
            Unconfirmed(_) => value,
            _ => Unsupported,
        },
    }
}

// Declared after the admission lease: cancellation records uncertainty before
// releasing that lease, so retirement cannot overtake the evidence write.
struct StartupCustody {
    retained: std::sync::Arc<parking_lot::Mutex<crate::CleanupComponentOutcome>>,
    accounted: bool,
}
impl Drop for StartupCustody {
    fn drop(&mut self) {
        if !self.accounted {
            let mut retained = self.retained.lock();
            *retained = combine_cleanup(
                retained.clone(),
                crate::CleanupComponentOutcome::Unconfirmed(
                    "child startup waiter lost before registration; cleanup unavailable".into(),
                ),
            );
        }
    }
}

async fn shutdown_children(
    context: &KernelContext,
    timeout: Duration,
) -> crate::CleanupComponentOutcome {
    let (children, mut outcome) = {
        let children = context.children.lock();
        (
            children.values().cloned().collect::<Vec<_>>(),
            context.forgotten_children.lock().clone(),
        )
    };
    let mut shutdowns = FuturesUnordered::new();
    for child in children {
        shutdowns.push(async move {
            let requested = ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "owner actor stopped".into(),
            };
            let result =
                tokio::time::timeout(timeout, child.shutdown_with_cleanup(requested.clone())).await;
            match result {
                Ok(Ok(outcome))
                    if outcome.cleanup.actor() == child.identity()
                        && outcome.cleanup.is_confirmed() =>
                {
                    crate::CleanupComponentOutcome::Confirmed
                }
                Ok(Ok(_)) => crate::CleanupComponentOutcome::Unconfirmed(format!(
                    "child {:?} cleanup is unconfirmed",
                    child.identity()
                )),
                _ => {
                    if child.terminal().get().is_none() {
                        publish_terminal(child.terminal(), &requested);
                    }
                    child.address().kill();
                    crate::CleanupComponentOutcome::Unconfirmed(format!(
                        "child {:?} retirement failed or timed out; kill is not cleanup",
                        child.identity()
                    ))
                }
            }
        });
    }
    while let Some(child) = shutdowns.next().await {
        outcome = combine_cleanup(outcome, child);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use parking_lot::Mutex;
    use tidepool_repr::SessionId;
    use tidepool_runtime::session::{WorkbenchResponse, WorkbenchRunStatus};
    use tidepool_tool::{ToolArguments, ToolInvocation};
    use tokio::sync::{oneshot, Notify};

    use super::*;

    fn tool_invocation(name: &str) -> ToolInvocation {
        ToolInvocation {
            context: None,
            name: name.into(),
            arguments: ToolArguments::Structured(serde_json::Value::Object(serde_json::Map::new())),
        }
    }

    struct ProbeBehavior {
        startup_gate: Option<(Arc<Notify>, Arc<Notify>)>,
        calls: Arc<Mutex<Vec<&'static str>>>,
        release_first: Arc<Notify>,
        fail_cast: bool,
        mailbox_ready: bool,
        spawned_child: Arc<Mutex<Option<LocalActorRef>>>,
        child_exits: Arc<Mutex<Vec<ActorTerminal>>>,
    }

    impl KernelBehavior for ProbeBehavior {
        fn accepts_mailbox(&self) -> bool {
            self.mailbox_ready
        }

        fn start(
            &mut self,
            _context: &KernelContext,
        ) -> BoxFuture<'_, Result<KernelStep<()>, KernelBehaviorError>> {
            let gate = self.startup_gate.clone();
            Box::pin(async move {
                if let Some((entered, release)) = gate {
                    entered.notify_one();
                    release.notified().await;
                }
                Ok(KernelStep::Continue(()))
            })
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
            self.calls.lock().push("call");
            Box::pin(async move { Ok(KernelStep::Continue(request)) })
        }

        fn tool<'a>(
            &'a mut self,
            context: &'a KernelContext,
            invocation: ToolInvocation,
        ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>> {
            Box::pin(async move {
                let name = invocation.name;
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
                            summary: "finished through an agent tool".into(),
                        },
                    });
                } else if name == "first" {
                    self.calls.lock().push("first-start");
                    self.release_first.notified().await;
                    self.calls.lock().push("first-end");
                } else if name == "defer" {
                    self.calls.lock().push("reply-ready");
                    return Ok(KernelStep::ContinueLater(serde_json::Value::String(name)));
                } else if name == "park-mailbox" {
                    self.calls.lock().push("park-mailbox");
                    self.mailbox_ready = false;
                } else if name == "unpark-mailbox" {
                    self.calls.lock().push("unpark-mailbox");
                    self.mailbox_ready = true;
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

        fn resume(
            &mut self,
            _context: &KernelContext,
        ) -> BoxFuture<'_, Result<KernelStep<()>, KernelBehaviorError>> {
            Box::pin(async move {
                self.calls.lock().push("resume-start");
                self.release_first.notified().await;
                self.calls.lock().push("resume-end");
                Ok(KernelStep::Continue(()))
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

        fn tool<'a>(
            &'a mut self,
            context: &'a KernelContext,
            _invocation: ToolInvocation,
        ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>> {
            Box::pin(async move {
                Err(KernelInvocationFailure::Rejected {
                    actor: context.identity(),
                    detail: "child has no tool policy".into(),
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
                startup_gate: None,
                calls: Arc::clone(&calls),
                release_first: Arc::clone(&release),
                fail_cast,
                mailbox_ready: true,
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
            .send_message(KernelMessage::Tool {
                invocation: tool_invocation("first"),
                reply: first_tx.into(),
            })
            .expect("queue first");
        actor
            .address()
            .send_message(KernelMessage::Tool {
                invocation: tool_invocation("second"),
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
    async fn mailbox_calls_retain_fifo_custody_until_behavior_is_ready() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let parked = actor
            .address()
            .call(
                |reply| KernelMessage::Tool {
                    invocation: tool_invocation("park-mailbox"),
                    reply,
                },
                Some(Duration::from_secs(1)),
            )
            .await
            .expect("park transport")
            .expect("park reply")
            .expect("park succeeds");
        assert_eq!(parked, "park-mailbox");

        let dropped = Arc::new(AtomicUsize::new(0));
        let (call_tx, mut call_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Call {
                caller: ActorRef::first(crate::ActorId(99)),
                ancestry: crate::CallAncestry::begin(ActorRef::first(crate::ActorId(99))),
                request: MailboxValue::probe(SessionId(1), Arc::clone(&dropped)),
                reply: call_tx.into(),
            })
            .expect("queue call");
        tokio::task::yield_now().await;
        assert!(call_rx.try_recv().is_err());
        assert_eq!(&*fixture.calls.lock(), &["park-mailbox"]);

        let unparked = actor
            .address()
            .call(
                |reply| KernelMessage::Tool {
                    invocation: tool_invocation("unpark-mailbox"),
                    reply,
                },
                Some(Duration::from_secs(1)),
            )
            .await
            .expect("unpark transport")
            .expect("unpark reply")
            .expect("unpark succeeds");
        assert_eq!(unparked, "unpark-mailbox");
        let reply = call_rx
            .await
            .expect("retained call reply")
            .expect("call succeeds");
        drop(reply);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(
            &*fixture.calls.lock(),
            &["park-mailbox", "unpark-mailbox", "call"]
        );

        let terminal = ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "done".into(),
        };
        actor.shutdown(terminal).await.expect("shutdown");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn shutdown_releases_mailbox_custody_deferred_behind_external_work() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        actor
            .address()
            .call(
                |reply| KernelMessage::Tool {
                    invocation: tool_invocation("park-mailbox"),
                    reply,
                },
                Some(Duration::from_secs(1)),
            )
            .await
            .expect("park transport")
            .expect("park reply")
            .expect("park succeeds");

        let dropped = Arc::new(AtomicUsize::new(0));
        let (call_tx, call_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Call {
                caller: ActorRef::first(crate::ActorId(99)),
                ancestry: crate::CallAncestry::begin(ActorRef::first(crate::ActorId(99))),
                request: MailboxValue::probe(SessionId(1), Arc::clone(&dropped)),
                reply: call_tx.into(),
            })
            .expect("queue call");
        tokio::task::yield_now().await;

        let terminal = ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "cancel parked actor".into(),
        };
        let supervisor = ActorRef::first(crate::ActorId(99));
        actor
            .retire_by(supervisor, terminal)
            .await
            .expect("shutdown");
        assert!(actor.terminal().retirement_acknowledged_by(supervisor));
        assert!(!actor
            .terminal()
            .retirement_acknowledged_by(ActorRef::first(crate::ActorId(100))));
        task.await.expect("actor task");
        assert!(call_rx.await.is_err(), "deferred caller must be released");
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
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
        let supervisor = ActorRef::first(crate::ActorId(99));
        let observed = actor
            .retire_by(
                supervisor,
                ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "late retirement".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(observed.kind, ActorExitKind::Failed);
        assert!(!actor.terminal().retirement_acknowledged_by(supervisor));
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
                |reply| KernelMessage::Tool {
                    invocation: tool_invocation("finish"),
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
                summary: "finished through an agent tool".into(),
            })
        );
        assert_eq!(&*fixture.calls.lock(), &["shutdown"]);
    }

    #[tokio::test]
    async fn deferred_work_settles_the_caller_before_resuming() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let reply = actor
            .address()
            .call(
                |reply| KernelMessage::Tool {
                    invocation: tool_invocation("defer"),
                    reply,
                },
                Some(Duration::from_secs(1)),
            )
            .await
            .expect("RPC transport")
            .expect("actor replied")
            .expect("successful actor reply");
        assert_eq!(reply, serde_json::Value::String("defer".into()));
        assert_eq!(&*fixture.calls.lock(), &["reply-ready", "resume-start"]);

        fixture.release.notify_one();
        for _ in 0..20 {
            if fixture.calls.lock().last() == Some(&"resume-end") {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(fixture.calls.lock().last(), Some(&"resume-end"));

        let terminal = ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "done".into(),
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Shutdown {
                terminal,
                reply: shutdown_tx.into(),
            })
            .expect("queue shutdown");
        shutdown_rx.await.expect("shutdown reply");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn independent_roots_share_routing_but_not_supervision() {
        let directory = LocalActorDirectory::default();
        let first = behavior(false);
        let second = behavior(false);
        let (owner, owner_task) = spawn_local_actor_in_directory(
            None,
            first.behavior,
            crate::Incarnation(41),
            directory.clone(),
        )
        .await
        .expect("first root");
        let (sibling, sibling_task) = spawn_local_actor_in_directory(
            None,
            second.behavior,
            crate::Incarnation(41),
            directory.clone(),
        )
        .await
        .expect("second root");
        assert!(directory.resolve(owner.identity()).is_some());
        assert!(directory.resolve(sibling.identity()).is_some());
        assert!(directory
            .resolve(crate::ActorRef {
                incarnation: crate::Incarnation(42),
                ..owner.identity()
            })
            .is_none());
        let (spawn_tx, spawn_rx) = oneshot::channel();
        owner
            .address()
            .send_message(KernelMessage::Tool {
                invocation: tool_invocation("spawn"),
                reply: spawn_tx.into(),
            })
            .expect("spawn child");
        spawn_rx
            .await
            .expect("spawn response")
            .expect("spawn result");
        let child = first.spawned_child.lock().clone().expect("child");
        assert!(directory.resolve(child.identity()).is_some());
        owner
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "retire first tree".into(),
            })
            .await
            .expect("retire first");
        owner_task.await.expect("first task");
        assert!(child.terminal().get().is_some());
        assert!(sibling.terminal().get().is_none());
        // Exact retired addresses retain their exits for late observers.
        assert!(directory
            .resolve(owner.identity())
            .unwrap()
            .terminal()
            .get()
            .is_some());
        let (ping_tx, ping_rx) = oneshot::channel();
        sibling
            .address()
            .send_message(KernelMessage::Tool {
                invocation: tool_invocation("second"),
                reply: ping_tx.into(),
            })
            .expect("sibling still callable");
        assert_eq!(ping_rx.await.expect("sibling reply").unwrap(), "second");
        sibling
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "retire sibling".into(),
            })
            .await
            .expect("retire sibling");
        sibling_task.await.expect("sibling task");
    }

    #[tokio::test]
    async fn child_failure_is_retained_and_notifies_without_killing_owner() {
        let fixture = behavior(false);
        let incarnation = crate::Incarnation(41);
        let (owner, owner_task) =
            spawn_local_actor_in_incarnation(None, fixture.behavior, incarnation)
                .await
                .expect("spawn owner");
        assert_eq!(owner.identity().incarnation, incarnation);
        let (spawn_tx, spawn_rx) = oneshot::channel();
        owner
            .address()
            .send_message(KernelMessage::Tool {
                invocation: tool_invocation("spawn"),
                reply: spawn_tx.into(),
            })
            .expect("request child");
        spawn_rx.await.expect("spawn reply").expect("spawn result");
        let child = fixture.spawned_child.lock().clone().expect("child handle");
        assert_eq!(child.identity().incarnation, incarnation);
        assert_eq!(
            child
                .report_external_failure(ExternalApplicationFailure {
                    class: crate::ExternalApplicationFailureClass::ProcessLaunch,
                    detail: "codex did not start".into(),
                })
                .await
                .expect("report child application failure"),
            ExternalFailureDisposition::Applied
        );

        let child_terminal = child.terminal().wait().await;
        assert_eq!(child_terminal.kind, ActorExitKind::Failed);
        assert!(child_terminal.summary.contains("codex did not start"));
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
            .send_message(KernelMessage::Tool {
                invocation: tool_invocation("second"),
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
    #[tokio::test]
    async fn cleanup_child_force_and_terminal_only_never_confirm() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let (reply, receiver) = tokio::sync::oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Tool {
                invocation: tool_invocation("first"),
                reply: reply.into(),
            })
            .unwrap();
        for _ in 0..1000 {
            if fixture.calls.lock().contains(&"first-start") {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(fixture.calls.lock().contains(&"first-start"));
        let context = KernelContext {
            identity: actor.identity(),
            myself: actor.address().clone(),
            children: Arc::new(Mutex::new(HashMap::from([(
                actor.address().get_id(),
                actor.clone(),
            )]))),
            directory: LocalActorDirectory::default(),
            child_admission_closed: Arc::new(tokio::sync::RwLock::new(false)),
            forgotten_children: Arc::new(Mutex::new(crate::CleanupComponentOutcome::Confirmed)),
        };
        let result = shutdown_children(&context, Duration::from_millis(1)).await;
        assert!(matches!(
            result,
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
        task.await.unwrap();
        let _ = receiver.await;
        assert!(
            actor.terminal().cleanup().is_none(),
            "forced terminal must not manufacture component proof"
        );
        let observed = actor
            .shutdown_with_cleanup(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "observe only".into(),
            })
            .await
            .unwrap();
        assert!(!observed.cleanup.is_confirmed());
        assert_eq!(observed.cleanup.actor(), actor.identity());
        context.directory.insert(actor.clone());
        assert!(context.forget_terminal_actor(actor.identity()));
        assert!(context.children.lock().is_empty());
        assert!(
            matches!(
                shutdown_children(&context, Duration::from_millis(1)).await,
                crate::CleanupComponentOutcome::Unconfirmed(_)
            ),
            "forgetting routing must not erase cleanup uncertainty"
        );
    }
    #[tokio::test]
    async fn startup_admission_cancellation_is_retained_before_barrier() {
        for cancel in [false, true] {
            let (owner, task) = spawn_local_actor(None, behavior(false).behavior)
                .await
                .unwrap();
            let context = KernelContext {
                identity: owner.identity(),
                myself: owner.address().clone(),
                children: Arc::new(Mutex::new(HashMap::new())),
                directory: LocalActorDirectory::default(),
                child_admission_closed: Arc::new(tokio::sync::RwLock::new(false)),
                forgotten_children: Arc::new(Mutex::new(crate::CleanupComponentOutcome::Confirmed)),
            };
            let entered = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            let mut child = behavior(false).behavior;
            child.startup_gate = Some((entered.clone(), release.clone()));
            let spawning_context = context.clone();
            let spawning =
                tokio::spawn(async move { spawning_context.spawn_child(None, child).await });
            entered.notified().await;
            assert!(context.child_admission_closed.try_write().is_err());
            assert!(context.children.lock().is_empty());
            if cancel {
                spawning.abort();
                assert!(spawning.await.unwrap_err().is_cancelled());
            } else {
                release.notify_one();
                let child = spawning.await.unwrap().unwrap();
                assert!(context.owns_child(child.identity()));
            }
            *context.child_admission_closed.write().await = true;
            if cancel {
                assert!(context.children.lock().is_empty());
                assert!(matches!(
                    *context.forgotten_children.lock(),
                    crate::CleanupComponentOutcome::Unconfirmed(_)
                ));
            }
            let outcome = shutdown_children(&context, Duration::from_secs(1)).await;
            // Probe behavior cannot prove realm cleanup even on successful startup.
            assert!(!matches!(
                outcome,
                crate::CleanupComponentOutcome::Confirmed
            ));
            assert!(context
                .spawn_child(None, behavior(false).behavior)
                .await
                .is_err());
            owner
                .shutdown_with_cleanup(ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "test done".into(),
                })
                .await
                .unwrap();
            task.await.unwrap();
        }
    }
}
