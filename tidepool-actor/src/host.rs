//! Structured owner for resident actor work.
//!
//! The host polls actor futures directly. Registry admission and resident
//! machine checkout remain the two execution gates; this layer owns runnable
//! work, cancellation, and boundary custody without adding another scheduler.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use futures_util::stream::{FuturesUnordered, StreamExt};
use futures_util::FutureExt;
use tidepool_effect::dispatch::DispatchEffect;
use tidepool_model::{DynModelProvider, StreamSink};
use tidepool_runtime::session::{OutputSink, ResidentOutcome, ResidentSession};
use tokio::sync::watch;

use crate::resident_workbench::ResidentActorBoundary;
use crate::start::UnpublishedResidentActor;
use crate::{
    ActorAgentSession, ActorDescriptor, ActorExitKind, ActorMachineRegistry, ActorRef,
    ActorRegistry, ActorRegistryError, ActorRuntimeWake, ActorRuntimeWakes, ActorTerminal,
    ActorTurnKind, ActorWorkbenchSource, CallId, ExitObservation, OutboundSettlement,
    ResidentActorLifecycle, ResidentActorMailbox, ResidentActorRunner, ResidentActorStartError,
    ResidentActorStarter, ResidentActorWorkbenchError, ResidentCall, ResidentCallPoll,
    ResidentCompletionError, ResidentCompletionExecutor, ResidentLifecycleError,
    ResidentLifecyclePolicy, ResidentMailboxError, ResidentWait, ResidentWaitPoll, StartInitiator,
    TurnLease, WaitId,
};

/// A compiled root at the point where Rust transfers its machine and first
/// resident outcome into actor scheduling.
pub struct ResidentActorRoot<H, O> {
    descriptor: ActorDescriptor,
    machine: ResidentSession<H, O>,
    outcome: ResidentOutcome,
}

impl<H, O> ResidentActorRoot<H, O> {
    #[must_use]
    pub fn new(
        descriptor: ActorDescriptor,
        machine: ResidentSession<H, O>,
        outcome: ResidentOutcome,
    ) -> Self {
        Self {
            descriptor,
            machine,
            outcome,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentActorHostError {
    #[error("the actor host is closing")]
    Closing,
    #[error("resident session {0:?} is already installed in this host")]
    DuplicateSession(tidepool_repr::SessionId),
    #[error("actor {0:?} already has owned host work")]
    DuplicateTask(ActorRef),
    #[error(transparent)]
    Registry(#[from] ActorRegistryError),
    #[error(transparent)]
    Lifecycle(#[from] ResidentLifecycleError),
    #[error(transparent)]
    Start(#[from] ResidentActorStartError),
    #[error(
        "actor host did not quiesce: {tasks} tasks, {calls} calls, {waits} waits, {live_actors} live actors"
    )]
    NotQuiescent {
        tasks: usize,
        calls: usize,
        waits: usize,
        live_actors: usize,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ResidentHostTaskError {
    #[error(transparent)]
    Registry(#[from] ActorRegistryError),
    #[error(transparent)]
    Workbench(#[from] ResidentActorWorkbenchError),
    #[error(transparent)]
    Completion(#[from] ResidentCompletionError),
    #[error(transparent)]
    Start(#[from] ResidentActorStartError),
    #[error(transparent)]
    Lifecycle(#[from] ResidentLifecycleError),
    #[error(transparent)]
    Mailbox(#[from] ResidentMailboxError),
    #[error("actor task panicked")]
    Panicked,
    #[error("actor task was cancelled")]
    Cancelled,
}

impl ResidentHostTaskError {
    fn is_shutdown_cancellation(&self) -> bool {
        matches!(
            self,
            Self::Cancelled | Self::Start(ResidentActorStartError::Cancelled)
        )
    }
}

#[derive(Debug, Default)]
pub struct ResidentHostRunReport {
    pub roots: usize,
    pub parked: BTreeMap<ResidentHostParkedKind, usize>,
    pub idle: usize,
    pub exited: usize,
    pub failures: Vec<(ActorRef, ResidentHostTaskError)>,
    pub cleanup_failures: Vec<(ActorRef, ResidentLifecycleError)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResidentHostParkedKind {
    Call,
    Wait,
    McpPolicy,
}

#[derive(Debug)]
pub struct ResidentHostShutdownReport {
    pub run: ResidentHostRunReport,
    pub terminal_roots: Vec<(ActorRef, ActorTerminal)>,
    pub removed_sessions: usize,
}

struct ResidentHostRuntime<H, O> {
    registry: ActorRegistry,
    runner: ResidentActorRunner<H, O>,
    completions: ResidentCompletionExecutor<H, O>,
    starter: ResidentActorStarter<H, O>,
    mailbox: ResidentActorMailbox<H, O>,
    lifecycle: Arc<ResidentActorLifecycle<H, O>>,
}

enum HostedActorState {
    Running,
    Idle,
    ParkedCall,
    ParkedWait,
    McpPolicy,
    Exited,
}

struct HostedActor {
    cancel: watch::Sender<bool>,
    state: HostedActorState,
}

enum HostTaskResult<H, O> {
    Idle {
        actor: ActorRef,
        children: Vec<ActorRef>,
    },
    ParkedCall {
        actor: ActorRef,
        pending: ResidentCall,
        children: Vec<ActorRef>,
    },
    ParkedWait {
        actor: ActorRef,
        pending: ResidentWait,
        children: Vec<ActorRef>,
    },
    McpPolicy {
        actor: ActorRef,
        policy: Arc<crate::ResidentMcpPolicy<H, O>>,
        children: Vec<ActorRef>,
    },
    Exited {
        actor: ActorRef,
        children: Vec<ActorRef>,
        cleanup_failure: Option<ResidentLifecycleError>,
    },
    Failed {
        actor: ActorRef,
        error: ResidentHostTaskError,
        cleanup_failure: Option<ResidentLifecycleError>,
    },
}

enum HostWork {
    Outcome {
        turn: TurnLease,
        outcome: Box<ResidentOutcome>,
    },
    Call(ResidentCall),
    Wait(ResidentWait),
    Mailbox,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HostLifecycle {
    Open,
    Closing,
}

/// The sole owner of resident actor scheduling state for one runtime.
#[must_use = "a live actor host requires shutdown or run_until_shutdown for async cleanup"]
pub struct ResidentActorHost<H, O> {
    runtime: Arc<ResidentHostRuntime<H, O>>,
    machines: Arc<ActorMachineRegistry<H, O>>,
    provider: Arc<dyn DynModelProvider>,
    sink: Option<StreamSink>,
    wakes: ActorRuntimeWakes,
    tasks: FuturesUnordered<BoxFuture<'static, HostTaskResult<H, O>>>,
    actors: HashMap<ActorRef, HostedActor>,
    calls: HashMap<(ActorRef, CallId), ResidentCall>,
    waits: HashMap<(ActorRef, WaitId), ResidentWait>,
    mcp_policies: HashMap<ActorRef, Arc<crate::ResidentMcpPolicy<H, O>>>,
    roots: BTreeSet<ActorRef>,
    failures: Vec<(ActorRef, ResidentHostTaskError)>,
    cleanup_failures: Vec<(ActorRef, ResidentLifecycleError)>,
    lifecycle: HostLifecycle,
}

impl<H, O> ResidentActorHost<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    /// Return the installed resident MCP policy for an exact actor
    /// incarnation, once that actor has reached `serveTools`.
    #[must_use]
    pub fn mcp_policy(&self, actor: ActorRef) -> Option<Arc<crate::ResidentMcpPolicy<H, O>>> {
        self.mcp_policies.get(&actor).cloned()
    }

    pub fn new(
        registry: ActorRegistry,
        source: ActorWorkbenchSource,
        provider: Arc<dyn DynModelProvider>,
        sink: Option<StreamSink>,
        lifecycle_policy: ResidentLifecyclePolicy,
    ) -> Result<Self, ResidentActorHostError> {
        let wakes = registry.take_runtime_wakes()?;
        let machines = Arc::new(ActorMachineRegistry::new());
        let runner = ResidentActorRunner::new(Arc::clone(&machines), source.clone());
        let lifecycle = Arc::new(ResidentActorLifecycle::with_policy(
            registry.clone(),
            runner.clone(),
            lifecycle_policy,
        ));
        let completions = ResidentCompletionExecutor::new(Arc::clone(&machines), source);
        let starter = ResidentActorStarter::new(Arc::clone(&lifecycle), completions.clone());
        let mailbox = ResidentActorMailbox::new(Arc::clone(&lifecycle));
        Ok(Self {
            runtime: Arc::new(ResidentHostRuntime {
                registry,
                runner,
                completions,
                starter,
                mailbox,
                lifecycle,
            }),
            machines,
            provider,
            sink,
            wakes,
            tasks: FuturesUnordered::new(),
            actors: HashMap::new(),
            calls: HashMap::new(),
            waits: HashMap::new(),
            mcp_policies: HashMap::new(),
            roots: BTreeSet::new(),
            failures: Vec::new(),
            cleanup_failures: Vec::new(),
            lifecycle: HostLifecycle::Open,
        })
    }

    pub async fn launch_root(
        &mut self,
        root: ResidentActorRoot<H, O>,
    ) -> Result<ActorRef, ResidentActorHostError> {
        if self.lifecycle == HostLifecycle::Closing {
            return Err(ResidentActorHostError::Closing);
        }
        let session = root.descriptor.placement().session;
        if self.machines.label(session).is_some() {
            return Err(ResidentActorHostError::DuplicateSession(session));
        }
        let previous = self.machines.insert_idle(session, root.machine);
        debug_assert!(previous.is_none());

        let unpublished = match UnpublishedResidentActor::begin(
            &self.runtime.registry,
            None,
            root.descriptor,
            StartInitiator::Runtime,
        ) {
            Ok(unpublished) => unpublished,
            Err(error) => {
                self.machines.remove(session);
                return Err(error.into());
            }
        };
        let actor = match unpublished
            .publish(&self.runtime.registry, &self.runtime.lifecycle)
            .await
        {
            Ok(actor) => actor,
            Err(error) => {
                self.machines.remove(session);
                return Err(error.into());
            }
        };
        let turn = match self
            .runtime
            .registry
            .begin_turn(actor, ActorTurnKind::Haskell)
        {
            Ok(turn) => turn,
            Err(error) => {
                let _ = self
                    .runtime
                    .lifecycle
                    .force_terminate(actor, failed_terminal(error.to_string()))
                    .await;
                return Err(error.into());
            }
        };
        self.roots.insert(actor);
        self.schedule(
            actor,
            HostWork::Outcome {
                turn,
                outcome: Box::new(root.outcome),
            },
        )?;
        Ok(actor)
    }

    pub async fn run_until_idle(
        &mut self,
    ) -> Result<ResidentHostRunReport, ResidentActorHostError> {
        let mut wakes_open = true;
        loop {
            self.wakes.drain_available();
            self.service_wakes()?;
            if self.tasks.is_empty() {
                self.wakes.drain_available();
                self.service_wakes()?;
                if self.tasks.is_empty() {
                    break;
                }
            }
            if wakes_open {
                tokio::select! {
                    result = self.tasks.next() => {
                        if let Some(result) = result {
                            self.install_task_result(result);
                        }
                    }
                    open = self.wakes.wait() => {
                        wakes_open = open;
                    }
                }
            } else if let Some(result) = self.tasks.next().await {
                self.install_task_result(result);
            }
        }
        let mut parked = BTreeMap::new();
        let mut exited = 0;
        let mut idle = 0;
        for hosted in self.actors.values() {
            match &hosted.state {
                HostedActorState::Exited => exited += 1,
                HostedActorState::Running => {}
                HostedActorState::Idle => idle += 1,
                HostedActorState::ParkedCall => {
                    *parked.entry(ResidentHostParkedKind::Call).or_default() += 1;
                }
                HostedActorState::ParkedWait => {
                    *parked.entry(ResidentHostParkedKind::Wait).or_default() += 1;
                }
                HostedActorState::McpPolicy => {
                    *parked.entry(ResidentHostParkedKind::McpPolicy).or_default() += 1;
                }
            }
        }
        Ok(ResidentHostRunReport {
            roots: self.roots.len(),
            parked,
            idle,
            exited,
            failures: std::mem::take(&mut self.failures),
            cleanup_failures: std::mem::take(&mut self.cleanup_failures),
        })
    }

    /// Cooperatively terminate every owned root and keep polling task
    /// epilogues until no actor work or live-value obligation remains.
    pub async fn shutdown(mut self) -> Result<ResidentHostShutdownReport, ResidentActorHostError> {
        self.finish_shutdown().await
    }

    /// Run until an external shutdown signal resolves, then cooperatively
    /// terminate and drain the host. The signal participates in the host event
    /// loop, so it can interrupt provider, machine, or mailbox waits without
    /// aborting their owning actor task.
    pub async fn run_until_shutdown<F>(
        mut self,
        requested: F,
    ) -> Result<ResidentHostShutdownReport, ResidentActorHostError>
    where
        F: std::future::Future<Output = ()>,
    {
        tokio::pin!(requested);
        let mut wakes_open = true;
        loop {
            self.wakes.drain_available();
            self.service_wakes()?;
            tokio::select! {
                biased;
                () = &mut requested => break,
                result = self.tasks.next(), if !self.tasks.is_empty() => {
                    if let Some(result) = result {
                        self.install_task_result(result);
                    }
                }
                open = self.wakes.wait(), if wakes_open => {
                    wakes_open = open;
                }
            }
        }
        self.finish_shutdown().await
    }

    async fn finish_shutdown(
        &mut self,
    ) -> Result<ResidentHostShutdownReport, ResidentActorHostError> {
        self.lifecycle = HostLifecycle::Closing;
        for hosted in self.actors.values_mut() {
            hosted.cancel.send_replace(true);
        }

        let roots: Vec<_> = self.roots.iter().copied().collect();
        let sessions: HashSet<_> = roots
            .iter()
            .filter_map(|root| {
                self.runtime
                    .registry
                    .session_context(*root)
                    .ok()
                    .map(|context| context.placement.session)
            })
            .collect();
        let mut shutdown_cleanup_failures = Vec::new();
        for root in &roots {
            match self
                .runtime
                .lifecycle
                .force_terminate(
                    *root,
                    ActorTerminal {
                        kind: ActorExitKind::Cancelled,
                        summary: "actor host shutdown".into(),
                    },
                )
                .await
            {
                Ok(()) | Err(ResidentLifecycleError::Registry(ActorRegistryError::Exited(_))) => {}
                Err(error) => shutdown_cleanup_failures.push((*root, error)),
            }
        }

        let mut run = self.run_until_idle().await?;
        run.cleanup_failures.extend(shutdown_cleanup_failures);
        self.wakes.discard_all();

        let live_actors = self
            .actors
            .values()
            .filter(|hosted| !matches!(hosted.state, HostedActorState::Exited))
            .count();
        if !self.tasks.is_empty()
            || !self.calls.is_empty()
            || !self.waits.is_empty()
            || live_actors != 0
            || !self.wakes.is_empty()
        {
            return Err(ResidentActorHostError::NotQuiescent {
                tasks: self.tasks.len(),
                calls: self.calls.len(),
                waits: self.waits.len(),
                live_actors,
            });
        }

        let terminal_roots = roots
            .into_iter()
            .filter_map(|root| match self.runtime.registry.observe_exit(root) {
                Ok(ExitObservation::Exited(terminal)) => Some((root, terminal)),
                Ok(ExitObservation::Pending) | Err(_) => None,
            })
            .collect();
        let removed_sessions = sessions
            .into_iter()
            .filter(|session| self.machines.remove(*session).is_some())
            .count();
        Ok(ResidentHostShutdownReport {
            run,
            terminal_roots,
            removed_sessions,
        })
    }

    fn schedule(&mut self, actor: ActorRef, work: HostWork) -> Result<(), ResidentActorHostError> {
        let receiver = match self.actors.entry(actor) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                let (cancel, receiver) = watch::channel(false);
                entry.insert(HostedActor {
                    cancel,
                    state: HostedActorState::Running,
                });
                receiver
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if matches!(entry.get().state, HostedActorState::Running) {
                    return Err(ResidentActorHostError::DuplicateTask(actor));
                }
                entry.get_mut().state = HostedActorState::Running;
                entry.get().cancel.subscribe()
            }
        };
        let runtime = Arc::clone(&self.runtime);
        let provider = Arc::clone(&self.provider);
        let sink = self.sink.clone();
        self.tasks.push(
            async move {
                let cancelled = receiver.clone();
                let task = drive_actor(Arc::clone(&runtime), provider, sink, actor, work, receiver);
                match AssertUnwindSafe(task).catch_unwind().await {
                    Ok(Ok(result)) => result,
                    Ok(Err(error)) if *cancelled.borrow() || error.is_shutdown_cancellation() => {
                        HostTaskResult::Exited {
                            actor,
                            children: Vec::new(),
                            cleanup_failure: None,
                        }
                    }
                    Ok(Err(error)) => {
                        let cleanup_failure =
                            terminate_failed_actor(&runtime.lifecycle, actor, error.to_string())
                                .await;
                        HostTaskResult::Failed {
                            actor,
                            error,
                            cleanup_failure,
                        }
                    }
                    Err(_) => {
                        let error = ResidentHostTaskError::Panicked;
                        let cleanup_failure =
                            terminate_failed_actor(&runtime.lifecycle, actor, error.to_string())
                                .await;
                        HostTaskResult::Failed {
                            actor,
                            error,
                            cleanup_failure,
                        }
                    }
                }
            }
            .boxed(),
        );
        Ok(())
    }

    fn install_task_result(&mut self, result: HostTaskResult<H, O>) {
        match result {
            HostTaskResult::Idle { actor, children } => {
                self.register_children(children);
                if let Some(hosted) = self.actors.get_mut(&actor) {
                    hosted.state = HostedActorState::Idle;
                }
            }
            HostTaskResult::ParkedCall {
                actor,
                pending,
                children,
            } => {
                self.register_children(children);
                let key = pending.key();
                self.calls.insert(key, pending);
                if let Some(hosted) = self.actors.get_mut(&actor) {
                    hosted.state = HostedActorState::ParkedCall;
                }
            }
            HostTaskResult::ParkedWait {
                actor,
                pending,
                children,
            } => {
                self.register_children(children);
                let key = pending.key();
                self.waits.insert(key, pending);
                if let Some(hosted) = self.actors.get_mut(&actor) {
                    hosted.state = HostedActorState::ParkedWait;
                }
            }
            HostTaskResult::McpPolicy {
                actor,
                policy,
                children,
            } => {
                self.register_children(children);
                self.mcp_policies.insert(actor, policy);
                if let Some(hosted) = self.actors.get_mut(&actor) {
                    hosted.state = HostedActorState::McpPolicy;
                }
            }
            HostTaskResult::Exited {
                actor,
                children,
                cleanup_failure,
            } => {
                self.register_children(children);
                if let Some(hosted) = self.actors.get_mut(&actor) {
                    hosted.state = HostedActorState::Exited;
                }
                if let Some(error) = cleanup_failure {
                    self.cleanup_failures.push((actor, error));
                }
            }
            HostTaskResult::Failed {
                actor,
                error,
                cleanup_failure,
            } => {
                if let Some(hosted) = self.actors.get_mut(&actor) {
                    hosted.state = HostedActorState::Exited;
                }
                self.failures.push((actor, error));
                if let Some(error) = cleanup_failure {
                    self.cleanup_failures.push((actor, error));
                }
            }
        }
    }

    fn service_wakes(&mut self) -> Result<(), ResidentActorHostError> {
        let actors: Vec<_> = self.actors.keys().copied().collect();
        for actor in actors {
            self.wakes.take(ActorRuntimeWake::ActorReady { actor });
            if !self.wakes.take(ActorRuntimeWake::ActorExited { actor }) {
                continue;
            }
            if let Some(hosted) = self.actors.get_mut(&actor) {
                // An exit wake reports the registry's terminal transition; it
                // is not permission to cancel the task that may still own the
                // actor's mandatory cleanup epilogue. Only host closing sends
                // cancellation. A running task reports `Exited`/`Failed` after
                // its epilogue settles; actors with no task can be marked now.
                if !matches!(hosted.state, HostedActorState::Running) {
                    hosted.state = HostedActorState::Exited;
                }
            }
            self.wakes.take(ActorRuntimeWake::MailboxReady { actor });
            self.calls.retain(|(caller, call), _| {
                if *caller == actor {
                    self.wakes.take(ActorRuntimeWake::CallReady {
                        caller: *caller,
                        call: *call,
                    });
                    false
                } else {
                    true
                }
            });
            self.waits.retain(|(waiter, wait), _| {
                if *waiter == actor {
                    self.wakes.take(ActorRuntimeWake::WaitReady {
                        waiter: *waiter,
                        wait: *wait,
                    });
                    false
                } else {
                    true
                }
            });
        }

        let calls: Vec<_> = self.calls.keys().copied().collect();
        for (caller, call) in calls {
            if self
                .wakes
                .take(ActorRuntimeWake::CallReady { caller, call })
            {
                if let Some(pending) = self.calls.remove(&(caller, call)) {
                    self.schedule(caller, HostWork::Call(pending))?;
                }
            }
        }

        let waits: Vec<_> = self.waits.keys().copied().collect();
        for (waiter, wait) in waits {
            if self
                .wakes
                .take(ActorRuntimeWake::WaitReady { waiter, wait })
            {
                if let Some(pending) = self.waits.remove(&(waiter, wait)) {
                    self.schedule(waiter, HostWork::Wait(pending))?;
                }
            }
        }

        let idle: Vec<_> = self
            .actors
            .iter()
            .filter_map(|(actor, hosted)| {
                matches!(hosted.state, HostedActorState::Idle).then_some(*actor)
            })
            .collect();
        for actor in idle {
            if self.wakes.take(ActorRuntimeWake::MailboxReady { actor }) {
                self.schedule(actor, HostWork::Mailbox)?;
            }
        }

        Ok(())
    }

    fn register_children(&mut self, children: Vec<ActorRef>) {
        for child in children {
            self.actors.entry(child).or_insert_with(|| {
                let (cancel, _) = watch::channel(false);
                let state = if self.runtime.registry.lifecycle(child)
                    == Ok(crate::ActorLifecycle::Exited)
                {
                    HostedActorState::Exited
                } else {
                    HostedActorState::Idle
                };
                HostedActor { cancel, state }
            });
        }
    }
}

async fn drive_actor<H, O>(
    runtime: Arc<ResidentHostRuntime<H, O>>,
    provider: Arc<dyn DynModelProvider>,
    sink: Option<StreamSink>,
    actor: ActorRef,
    work: HostWork,
    cancel: watch::Receiver<bool>,
) -> Result<HostTaskResult<H, O>, ResidentHostTaskError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    let mut children = Vec::new();
    let (mut turn, mut outcome) = match work {
        HostWork::Outcome { turn, outcome } => (turn, *outcome),
        HostWork::Call(pending) => {
            match await_host_operation(cancel.clone(), runtime.mailbox.poll_call(pending))
                .await
                .ok_or(ResidentHostTaskError::Cancelled)??
            {
                ResidentCallPoll::Pending(pending) => {
                    return Ok(HostTaskResult::ParkedCall {
                        actor,
                        pending,
                        children,
                    });
                }
                ResidentCallPoll::Continued { turn, outcome } => (turn, *outcome),
            }
        }
        HostWork::Wait(pending) => {
            match await_host_operation(cancel.clone(), runtime.mailbox.poll_wait(pending))
                .await
                .ok_or(ResidentHostTaskError::Cancelled)??
            {
                ResidentWaitPoll::Pending(pending) => {
                    return Ok(HostTaskResult::ParkedWait {
                        actor,
                        pending,
                        children,
                    });
                }
                ResidentWaitPoll::Continued { turn, outcome } => (turn, *outcome),
            }
        }
        HostWork::Mailbox => {
            await_host_operation(cancel.clone(), runtime.mailbox.dispatch_one(actor))
                .await
                .ok_or(ResidentHostTaskError::Cancelled)??;
            return Ok(
                if runtime.registry.lifecycle(actor) == Ok(crate::ActorLifecycle::Exited) {
                    HostTaskResult::Exited {
                        actor,
                        children,
                        cleanup_failure: None,
                    }
                } else {
                    HostTaskResult::Idle { actor, children }
                },
            );
        }
    };
    loop {
        let context = turn.session_context();
        let boundary = await_host_operation(
            cancel.clone(),
            runtime.runner.capture_boundary(
                context.clone(),
                outcome,
                context.placement.resource_scope,
            ),
        )
        .await
        .ok_or(ResidentHostTaskError::Cancelled)??;
        match boundary {
            ResidentActorBoundary::Completed => {
                drop(turn);
                let cleanup_failure = runtime
                    .lifecycle
                    .force_terminate(
                        actor,
                        ActorTerminal {
                            kind: ActorExitKind::Completed,
                            summary: "completed".into(),
                        },
                    )
                    .await
                    .err();
                return Ok(HostTaskResult::Exited {
                    actor,
                    children,
                    cleanup_failure,
                });
            }
            ResidentActorBoundary::Deliberate(completion) => {
                let agent = ActorAgentSession::attach(runtime.registry.clone(), actor)?;
                (turn, outcome) = await_host_operation(
                    cancel.clone(),
                    runtime.completions.resolve(
                        &agent,
                        turn,
                        provider.as_ref(),
                        completion,
                        sink.clone(),
                    ),
                )
                .await
                .ok_or(ResidentHostTaskError::Cancelled)??;
            }
            ResidentActorBoundary::Start(start) => {
                let cancellation = Box::pin(cancellation_requested(cancel.clone()));
                let (next_turn, child, next_outcome) = runtime
                    .starter
                    .start_until_cancelled(
                        turn,
                        provider.as_ref(),
                        start,
                        sink.clone(),
                        cancellation,
                    )
                    .await?;
                children.push(child);
                turn = next_turn;
                outcome = next_outcome;
            }
            ResidentActorBoundary::Outbound(outbound) => {
                match await_host_operation(
                    cancel.clone(),
                    runtime.mailbox.submit_captured_outbound(turn, outbound),
                )
                .await
                .ok_or(ResidentHostTaskError::Cancelled)??
                {
                    OutboundSettlement::Continued {
                        turn: next_turn,
                        outcome: next_outcome,
                    } => {
                        turn = next_turn;
                        outcome = *next_outcome;
                    }
                    OutboundSettlement::Pending(pending) => {
                        return Ok(HostTaskResult::ParkedCall {
                            actor,
                            pending,
                            children,
                        });
                    }
                }
            }
            ResidentActorBoundary::Wait(wait) => {
                let pending = await_host_operation(
                    cancel.clone(),
                    runtime.mailbox.submit_captured_wait(turn, wait),
                )
                .await
                .ok_or(ResidentHostTaskError::Cancelled)??;
                return Ok(HostTaskResult::ParkedWait {
                    actor,
                    pending,
                    children,
                });
            }
            ResidentActorBoundary::Receive(receiver) => {
                runtime.registry.install_resident_receiver(turn, receiver)?;
                return Ok(HostTaskResult::Idle { actor, children });
            }
            ResidentActorBoundary::McpAwait(awaiting) => {
                drop(turn);
                let policy = Arc::new(crate::resident_mcp::install_resident_mcp(
                    actor,
                    runtime.registry.clone(),
                    runtime.runner.clone(),
                    Arc::clone(&runtime.lifecycle),
                    awaiting,
                ));
                return Ok(HostTaskResult::McpPolicy {
                    actor,
                    policy,
                    children,
                });
            }
            ResidentActorBoundary::McpReply(_) => {
                return Err(ResidentHostTaskError::Workbench(
                    ResidentActorWorkbenchError::ActorProtocol(
                        "actor MCP reply reached the host without an invocation".into(),
                    ),
                ));
            }
        }
    }
}

async fn cancellation_requested(mut receiver: watch::Receiver<bool>) {
    if *receiver.borrow() {
        return;
    }
    while receiver.changed().await.is_ok() {
        if *receiver.borrow() {
            return;
        }
    }
}

async fn await_host_operation<T>(
    cancel: watch::Receiver<bool>,
    operation: impl std::future::Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        () = cancellation_requested(cancel) => None,
        result = operation => Some(result),
    }
}

async fn terminate_failed_actor<H, O>(
    lifecycle: &ResidentActorLifecycle<H, O>,
    actor: ActorRef,
    summary: String,
) -> Option<ResidentLifecycleError>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    match lifecycle
        .force_terminate(actor, failed_terminal(summary))
        .await
    {
        Ok(()) | Err(ResidentLifecycleError::Registry(ActorRegistryError::Exited(_))) => None,
        Err(error) => Some(error),
    }
}

fn failed_terminal(summary: String) -> ActorTerminal {
    ActorTerminal {
        kind: ActorExitKind::Failed,
        summary,
    }
}
