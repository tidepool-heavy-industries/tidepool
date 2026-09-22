//! Canonical sequential Ractor wrapper for Tidepool actor behavior.

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::Arc;
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

/// One shutdown budget computed once by `finish_actor` and threaded to every
/// component that can otherwise wait unbounded for a busy or hung resource:
/// hook admission and resource-scope/placement checkout race this deadline, and
/// supervised children receive `deadline - SHUTDOWN_CHILD_MARGIN` so a
/// child's own budget always expires before the parent's, and it can report
/// its own `Unconfirmed` outcome instead of being killed by W3.
pub(crate) const SHUTDOWN_BUDGET: Duration = Duration::from_secs(30);
const SHUTDOWN_CHILD_MARGIN: Duration = Duration::from_secs(5);

/// Who is publishing an actor's terminal result.
///
/// [`publish_exit`] is the only caller of [`RetainedActorExit::publish`]
/// outside `termination`'s own tests, so every write to an actor's exit
/// declares which of the two writers it is.
pub(crate) enum ExitAuthority<'a> {
    /// The actor's own `finish_actor`: an ordinary stop (W1) or a
    /// replacement fence (W2). Carries the children snapshot taken after
    /// child admission closed; empty for a replaced predecessor, since
    /// ownership of every child already moved to the successor.
    Own { children: &'a [LocalActorRef] },
    /// The direct supervisor publishes on behalf of a child whose task is
    /// gone or was killed (W3, W4). No cleanup proof is retained; the
    /// child's `cleanup()` stays `None`.
    SupervisorForced,
}

/// What `finish_actor` is settling: an ordinary stop, or a replacement
/// fence handing this actor's identity and owned handles to a successor.
enum Disposition {
    Stop(ActorTerminal),
    Replaced {
        successor: crate::ActorRef,
        summary: String,
    },
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
    resources: Arc<Mutex<HashMap<ractor::ActorId, ResourceChild>>>,
    directory: LocalActorDirectory,
    forgotten_children: std::sync::Arc<parking_lot::Mutex<crate::CleanupComponentOutcome>>,
    child_admission_closed: std::sync::Arc<tokio::sync::RwLock<bool>>,
}

#[derive(Clone)]
struct ResourceChild {
    cell: ractor::ActorCell,
    cleanup: Arc<
        dyn Fn(ResourceCleanup) -> BoxFuture<'static, crate::CleanupComponentOutcome> + Send + Sync,
    >,
}

#[derive(Clone, Copy)]
pub(crate) enum ResourceCleanup {
    Observe,
    Retire,
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
    actors: std::sync::Arc<parking_lot::RwLock<HashMap<ActorRef, DirectoryEntry>>>,
    sessions: std::sync::Arc<parking_lot::RwLock<HashMap<ActorRef, crate::ActorSessionContext>>>,
    identities: std::sync::Arc<parking_lot::Mutex<DirectoryIdentities>>,
}

#[derive(Default)]
struct DirectoryIdentities {
    next_logical_id: u64,
    runtime: HashMap<ractor::ActorId, ActorRef>,
    fenced: std::collections::HashSet<crate::ActorId>,
    claimed: HashMap<crate::ActorId, ActorRef>,
}

struct DirectoryEntry {
    actor: LocalActorRef,
    // Retaining an exit must not keep its execution context (and thus this
    // directory) alive. Ractor's actor state owns the strong context reference.
    context: std::sync::Weak<KernelContext>,
}

impl LocalActorDirectory {
    fn reserve(&self, incarnation: crate::Incarnation) -> Result<ActorRef, String> {
        let mut identities = self.identities.lock();
        loop {
            identities.next_logical_id = identities
                .next_logical_id
                .checked_add(1)
                .ok_or_else(|| "logical actor identity space exhausted".to_owned())?;
            let id = crate::ActorId(identities.next_logical_id);
            if !identities.fenced.contains(&id) && !identities.claimed.contains_key(&id) {
                let actor = ActorRef { id, incarnation };
                identities.claimed.insert(id, actor);
                return Ok(actor);
            }
        }
    }

    pub(crate) fn fence_logical_ids(
        &self,
        actors: impl IntoIterator<Item = crate::ActorId>,
    ) -> Result<(), String> {
        let mut identities = self.identities.lock();
        for actor in actors {
            if identities.claimed.contains_key(&actor) {
                return Err(format!("logical actor {} is already active", actor.0));
            }
            identities.next_logical_id = identities.next_logical_id.max(actor.0);
            identities.fenced.insert(actor);
        }
        Ok(())
    }

    fn claim_exact(&self, actor: ActorRef) -> Result<(), String> {
        let mut identities = self.identities.lock();
        if let Some(previous) = identities.claimed.get(&actor.id).copied() {
            let predecessor_is_terminal = self
                .actors
                .read()
                .get(&previous)
                .is_some_and(|entry| entry.actor.terminal().get().is_some());
            if !predecessor_is_terminal || actor.incarnation <= previous.incarnation {
                return Err(format!(
                    "logical actor {} is already active as {previous}",
                    actor.id.0
                ));
            }
        }
        identities.fenced.remove(&actor.id);
        identities.claimed.insert(actor.id, actor);
        identities.next_logical_id = identities.next_logical_id.max(actor.id.0);
        Ok(())
    }

    fn runtime_identity(&self, actor: ractor::ActorId) -> Option<ActorRef> {
        self.identities.lock().runtime.get(&actor).copied()
    }

    #[must_use]
    pub fn resolve(&self, actor: ActorRef) -> Option<LocalActorRef> {
        self.actors
            .read()
            .get(&actor)
            .map(|entry| entry.actor.clone())
    }

    #[must_use]
    pub fn session_context(&self, actor: ActorRef) -> Option<crate::ActorSessionContext> {
        self.sessions.read().get(&actor).cloned()
    }

    fn insert(&self, actor: LocalActorRef, context: std::sync::Weak<KernelContext>) {
        self.identities
            .lock()
            .runtime
            .insert(actor.address().get_id(), actor.identity());
        self.actors
            .write()
            .insert(actor.identity(), DirectoryEntry { actor, context });
    }

    fn context(&self, actor: ActorRef) -> Option<std::sync::Arc<KernelContext>> {
        self.actors.read().get(&actor)?.context.upgrade()
    }

    fn forget_terminal(&self, actor: ActorRef) -> bool {
        let mut actors = self.actors.write();
        let terminal = actors
            .get(&actor)
            .is_some_and(|entry| entry.actor.terminal().get().is_some());
        if terminal {
            actors.remove(&actor);
            self.sessions.write().remove(&actor);
        }
        terminal
    }
}

impl KernelContext {
    /// Internal resources use Ractor supervision without a resident machine or model.
    pub(crate) async fn spawn_resource<A: ractor::Actor>(
        &self,
        name: Option<String>,
        actor: A,
        arguments: A::Arguments,
        cleanup: impl Fn(ResourceCleanup) -> BoxFuture<'static, crate::CleanupComponentOutcome>
            + Send
            + Sync
            + 'static,
    ) -> Result<RactorRef<A::Msg>, ractor::SpawnErr> {
        let admission = self.child_admission_closed.clone().read_owned().await;
        if *admission {
            return Err(ractor::SpawnErr::StartupFailed(
                std::io::Error::other("resource owner is retiring").into(),
            ));
        }
        let (child, task) = self.myself.spawn_linked(name, actor, arguments).await?;
        self.resources.lock().insert(
            child.get_id(),
            ResourceChild {
                cell: child.get_cell(),
                cleanup: Arc::new(cleanup),
            },
        );
        drop(task);
        Ok(child)
    }

    pub(crate) fn supervisor_identity(&self) -> Option<ActorRef> {
        self.myself
            .get_cell()
            .try_get_supervisor()
            .and_then(|supervisor| self.directory.runtime_identity(supervisor.get_id()))
    }
    pub(crate) async fn spawn_successor<B: KernelBehavior>(
        &self,
        behavior: B,
    ) -> Result<
        (
            LocalActorRef,
            Option<tokio::sync::OwnedRwLockReadGuard<bool>>,
        ),
        ractor::SpawnErr,
    > {
        if let Some(parent) = self.supervisor_identity() {
            let context = self.directory.context(parent).ok_or_else(|| {
                ractor::SpawnErr::StartupFailed(
                    std::io::Error::other("replacement supervisor is no longer running").into(),
                )
            })?;
            context
                .spawn_worker_retained(None, behavior, crate::WorkerLifetime::ParentOwned)
                .await
                .map(|(actor, admission)| (actor, Some(admission)))
        } else {
            let (actor, task) = spawn_local_actor_in_directory(
                None,
                behavior,
                self.identity.incarnation,
                self.directory.clone(),
            )
            .await?;
            drop(task);
            Ok((actor, None))
        }
    }

    pub(crate) fn requested_shutdown(&self) -> Option<ActorTerminal> {
        self.directory
            .resolve(self.identity)?
            .terminal()
            .requested_shutdown()
    }

    pub(crate) async fn wait_requested_shutdown(&self) -> ActorTerminal {
        #[allow(
            clippy::expect_used,
            reason = "called on this actor's own KernelContext while it is \
                      still running its turn loop; forget_terminal only ever \
                      removes an entry already observed terminal, so a live \
                      actor's own identity always resolves in its directory"
        )]
        self.directory
            .resolve(self.identity)
            .expect("running actor remains in its local directory")
            .terminal()
            .wait_requested_shutdown()
            .await
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
        self.spawn_worker(name, behavior, crate::WorkerLifetime::ParentOwned)
            .await
    }

    pub(crate) async fn spawn_worker<C>(
        &self,
        name: Option<String>,
        behavior: C,
        lifetime: crate::WorkerLifetime,
    ) -> Result<LocalActorRef, ractor::SpawnErr>
    where
        C: KernelBehavior,
    {
        self.spawn_worker_retained(name, behavior, lifetime)
            .await
            .map(|(actor, _)| actor)
    }

    async fn spawn_worker_retained<C: KernelBehavior>(
        &self,
        name: Option<String>,
        behavior: C,
        lifetime: crate::WorkerLifetime,
    ) -> Result<(LocalActorRef, tokio::sync::OwnedRwLockReadGuard<bool>), ractor::SpawnErr> {
        // Hold admission through registration so retirement cannot miss a
        // child whose startup is already in flight.
        let admission = self.child_admission_closed.clone().read_owned().await;
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
        let mailbox_admission = crate::kernel::MailboxAdmission::default();
        let identity = self
            .directory
            .reserve(self.identity.incarnation)
            .map_err(|error| {
                ractor::SpawnErr::StartupFailed(std::io::Error::other(error).into())
            })?;
        let arguments = LocalActorArguments {
            behavior,
            terminal: terminal.clone(),
            directory: self.directory.clone(),
            identity,
            mailbox_admission: mailbox_admission.clone(),
        };
        let spawned = match lifetime {
            crate::WorkerLifetime::ParentOwned => {
                self.myself
                    .spawn_linked(name, LocalActor::<C>(PhantomData), arguments)
                    .await
            }
            crate::WorkerLifetime::SwarmOwned => {
                LocalActor::<C>::spawn(name, LocalActor::<C>(PhantomData), arguments).await
            }
        };
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
        let child =
            LocalActorRef::with_identity_admission(address, terminal, identity, mailbox_admission);
        if lifetime == crate::WorkerLifetime::ParentOwned {
            self.children
                .lock()
                .insert(child.address().get_id(), child.clone());
        }
        custody.accounted = true;
        Ok((child, admission))
    }
}

/// Resident execution owned by one sequential local actor.
///
/// The wrapper owns lifecycle, publication, and reply settlement. A behavior
/// owns Haskell/provider/tool execution and returns domain results without
/// gaining access to Ractor's scheduler internals.
pub trait KernelBehavior: Send + 'static {
    fn replacement_staged(&self) -> bool {
        false
    }
    /// Release an unadmitted recipe through the owner of its captured scope.
    fn discard_replacement<'a>(
        &'a mut self,
        _definition: crate::ActorReplacementDefinition,
    ) -> BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async {
            Err(KernelBehaviorError {
                detail: "actor cannot confirm replacement recipe cleanup".into(),
            })
        })
    }
    fn replace<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _definition: crate::ActorReplacementDefinition,
    ) -> BoxFuture<'a, Result<LocalActorRef, KernelBehaviorError>> {
        Box::pin(async {
            Err(KernelBehaviorError {
                detail: "actor does not support stateful replacement".into(),
            })
        })
    }

    /// A prepared successor is mailbox-inert until its predecessor's fence
    /// transfers the accepted backlog and supervised children.
    fn activate_replacement<'a>(
        &'a mut self,
        _context: &'a KernelContext,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async {
            Err(KernelBehaviorError {
                detail: "actor has no prepared replacement".into(),
            })
        })
    }

    fn commit_replacement(
        &mut self,
        _predecessor: &KernelContext,
        _successor: &KernelContext,
    ) -> Result<(), KernelBehaviorError> {
        Err(KernelBehaviorError {
            detail: "actor has no replacement custody".into(),
        })
    }

    fn replacement_retired(&mut self, _context: &KernelContext, _terminal: &ActorTerminal) {}

    fn begin_drain(&mut self) -> Result<(), KernelBehaviorError> {
        Err(KernelBehaviorError {
            detail: "actor does not support draining".into(),
        })
    }

    fn close_sources(&mut self) {}

    fn drain<'a>(
        &'a mut self,
        _context: &'a KernelContext,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async {
            Err(KernelBehaviorError {
                detail: "actor does not support draining".into(),
            })
        })
    }
    /// Retain a failed input and its state instead of terminating this actor.
    /// Returning true requires closing execution while retaining mailbox admission.
    fn pause_failed_handler(&mut self, _context: &KernelContext, _detail: &str) -> bool {
        false
    }

    fn source<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _delivery: crate::SourceDelivery,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async {
            Err(KernelBehaviorError {
                detail: "actor has no installed source handler".into(),
            })
        })
    }

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
    /// mailbox receiver. The wrapper retains cast/call ownership while an
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
        invocation: exomonad_tool::ToolInvocation,
    ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>>;

    fn workbench<'a>(
        &'a mut self,
        context: &'a KernelContext,
        request: WorkbenchRequest,
        control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
    ) -> BoxFuture<'a, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>>;

    fn reconcile_workbench_cancellation(
        &self,
        execution: tidepool_runtime::session::WorkbenchExecutionId,
        _invocation: Option<exomonad_tool::ToolInvocationContext>,
    ) -> crate::WorkbenchCancellationOutcome {
        crate::WorkbenchCancellationOutcome::UnknownEvaluation { execution }
    }

    fn reconcile_workbench_boundary<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> BoxFuture<'a, Result<crate::WorkbenchBoundaryReconciliation, KernelBehaviorError>> {
        Box::pin(async move { Ok(crate::WorkbenchBoundaryReconciliation::Pending) })
    }

    /// Continue work deliberately yielded after its initiating caller was
    /// settled. Behaviors which never return 'ContinueLater' own no pending
    /// continuation and keep this rejecting default.
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
    /// resident resource scope it does not own; resident behavior overrides this
    /// method. `deadline` is the one shutdown deadline `finish_actor`
    /// computes for this retirement; a behavior that checks out a shared
    /// resource races that checkout against it instead of waiting unbounded.
    fn shutdown_components<'a>(
        &'a mut self,
        context: &'a KernelContext,
        terminal: &'a ActorTerminal,
        _deadline: tokio::time::Instant,
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
    /// Logical identity is allocated by the routing owner before scheduler
    /// admission. It deliberately does not reuse Ractor's process ID.
    pub identity: ActorRef,
    pub(crate) mailbox_admission: crate::kernel::MailboxAdmission,
}

#[derive(Default)]
enum HostedAdmission {
    #[default]
    Open,
    Sealed,
    Closing,
}

pub struct LocalActorState<B> {
    replacement: Option<PendingReplacement>,
    drain: DrainState,
    mailbox_admission: crate::kernel::MailboxAdmission,
    hosted_admission: HostedAdmission,
    context: std::sync::Arc<KernelContext>,
    behavior: B,
    terminal: RetainedActorExit,
    deferred_mailbox: VecDeque<KernelMessage>,
    mailbox_drain_scheduled: bool,
}

struct PendingReplacement {
    successor: LocalActorRef,
    reply: Option<ractor::RpcReplyPort<Result<LocalActorRef, crate::KernelInvocationFailure>>>,
    shutdown_waiters: Vec<ractor::RpcReplyPort<ActorTerminal>>,
}

#[derive(Default)]
enum DrainState {
    #[default]
    Open,
    Fencing,
    Draining,
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
        let identity = arguments.identity;
        let context = std::sync::Arc::new(KernelContext {
            identity,
            myself,
            children: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
            resources: Arc::new(Mutex::new(HashMap::new())),
            directory: arguments.directory,
            child_admission_closed: std::sync::Arc::new(tokio::sync::RwLock::new(false)),
            forgotten_children: std::sync::Arc::new(parking_lot::Mutex::new(
                crate::CleanupComponentOutcome::Confirmed,
            )),
        });
        let mut state = LocalActorState {
            replacement: None,
            drain: DrainState::Open,
            mailbox_admission: arguments.mailbox_admission,
            context,
            behavior: arguments.behavior,
            terminal: arguments.terminal,
            deferred_mailbox: VecDeque::new(),
            mailbox_drain_scheduled: false,
            hosted_admission: HostedAdmission::Open,
        };
        state.context.directory.insert(
            LocalActorRef::with_identity_admission(
                state.context.myself.clone(),
                state.terminal.clone(),
                state.context.identity,
                state.mailbox_admission.clone(),
            ),
            std::sync::Arc::downgrade(&state.context),
        );
        match state.behavior.start(&state.context).await {
            Ok(KernelStep::Continue(())) => {}
            Ok(KernelStep::ContinueLater(())) => {
                state.context.myself.send_message(KernelMessage::Resume)?;
            }
            Ok(KernelStep::Stop { terminal, .. }) => {
                finish_actor(
                    &state.context.myself.clone(),
                    &mut state,
                    Disposition::Stop(terminal),
                )
                .await;
            }
            Err(error) => {
                let terminal = failed_terminal(format!("actor startup failed: {error}"));
                finish_actor(
                    &state.context.myself.clone(),
                    &mut state,
                    Disposition::Stop(terminal),
                )
                .await;
                return Err(Box::new(error));
            }
        }
        Ok(state)
    }

    /// The actor level of the run's span tree: one span per inbound
    /// dispatch. An actor runs in its own task, so this span is a root of the
    /// trace rather than a child of the tool call that sent the message; the
    /// cell span's `execution` is what joins the two.
    #[tracing::instrument(
        name = "actor",
        skip_all,
        fields(
            actor = %state.context.identity,
            incarnation = state.context.identity.incarnation.0,
            message = message.kind(),
        )
    )]
    async fn handle(
        &self,
        myself: RactorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let message = match message {
            message @ KernelMessage::Shutdown { .. } if state.behavior.replacement_staged() => {
                state.deferred_mailbox.push_back(message);
                return Ok(());
            }
            message @ KernelMessage::RouteReady { .. }
                if state.replacement.is_some() || state.behavior.replacement_staged() =>
            {
                state.deferred_mailbox.push_back(message);
                return Ok(());
            }
            message @ (KernelMessage::Cast { .. }
            | KernelMessage::Call { .. }
            | KernelMessage::Source(_))
                if state.replacement.is_some()
                    || !state.deferred_mailbox.is_empty()
                    || !state.behavior.accepts_mailbox() =>
            {
                state.deferred_mailbox.push_back(message);
                schedule_deferred_mailbox(&myself, state)?;
                return Ok(());
            }
            KernelMessage::DrainMailbox => {
                state.mailbox_drain_scheduled = false;
                if state.replacement.is_some() || !state.behavior.accepts_mailbox() {
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
            KernelMessage::AbortReplacement { reply } => {
                if !state.behavior.replacement_staged() {
                    let _ = reply.send(Err(KernelBehaviorError {
                        detail: "actor is not a prepared replacement".into(),
                    }));
                    return Ok(());
                }
                let terminal = finish_actor(
                    &myself,
                    state,
                    Disposition::Stop(ActorTerminal {
                        kind: ActorExitKind::Cancelled,
                        summary: "replacement preparation aborted".into(),
                    }),
                )
                .await;
                let result = if state
                    .terminal
                    .cleanup()
                    .is_some_and(|cleanup| cleanup.is_confirmed())
                {
                    Ok(())
                } else {
                    Err(KernelBehaviorError {
                        detail: format!("prepared actor cleanup unconfirmed: {}", terminal.summary),
                    })
                };
                let _ = reply.send(result);
            }
            KernelMessage::Replace { definition, reply } => {
                if state.replacement.is_some() {
                    let failure = match state.behavior.discard_replacement(*definition).await {
                        Ok(()) => crate::KernelInvocationFailure::Rejected {
                            actor: state.context.identity,
                            detail: "actor replacement is already in progress".into(),
                        },
                        Err(error) => crate::KernelInvocationFailure::Failed {
                            actor: state.context.identity,
                            detail: format!(
                                "actor replacement is already in progress; rejected recipe cleanup unconfirmed: {error}"
                            ),
                        },
                    };
                    let _ = reply.send(Err(failure));
                } else {
                    match state.behavior.replace(&state.context, *definition).await {
                        Ok(successor) => {
                            state.replacement = Some(PendingReplacement {
                                successor,
                                reply: Some(reply),
                                shutdown_waiters: Vec::new(),
                            })
                        }
                        Err(error) => {
                            let _ = reply.send(Err(crate::KernelInvocationFailure::Rejected {
                                actor: state.context.identity,
                                detail: error.detail,
                            }));
                        }
                    }
                }
            }
            KernelMessage::ReplacementFence => {
                let Some(mut pending) = state.replacement.take() else {
                    return Err(std::io::Error::other(
                        "replacement fence has no prepared successor",
                    )
                    .into());
                };
                let successor_context = state
                    .context
                    .directory
                    .context(pending.successor.identity())
                    .ok_or_else(|| {
                        std::io::Error::other("prepared successor lost its execution context")
                    })?;
                *state.context.child_admission_closed.write().await = true;
                if let Err(error) = state
                    .behavior
                    .commit_replacement(&state.context, &successor_context)
                {
                    if let Some(reply) = pending.reply.take() {
                        let _ = reply.send(Err(crate::KernelInvocationFailure::Failed {
                            actor: state.context.identity,
                            detail: error.detail,
                        }));
                    }
                    state.replacement = Some(pending);
                    return Ok(());
                }
                {
                    let mut children = state.context.children.lock();
                    let mut inherited = successor_context.children.lock();
                    for (id, child) in children.drain() {
                        child
                            .address()
                            .get_cell()
                            .link(successor_context.myself.get_cell());
                        inherited.insert(id, child);
                    }
                }
                {
                    let mut resources = state.context.resources.lock();
                    for (id, child) in resources.drain() {
                        child.cell.link(successor_context.myself.get_cell());
                        successor_context.resources.lock().insert(id, child);
                    }
                }
                {
                    let mut inherited = successor_context.forgotten_children.lock();
                    *inherited = combine_cleanup(
                        inherited.clone(),
                        std::mem::replace(
                            &mut *state.context.forgotten_children.lock(),
                            crate::CleanupComponentOutcome::Confirmed,
                        ),
                    );
                }
                let summary = format!("replaced by {:?}", pending.successor.identity());
                pending
                    .successor
                    .address()
                    .send_message(KernelMessage::ActivateReplacement {
                        backlog: std::mem::take(&mut state.deferred_mailbox),
                        draining: !matches!(state.drain, DrainState::Open),
                    })?;
                if let Some(parent) = state
                    .context
                    .supervisor_identity()
                    .and_then(|parent| state.context.directory.context(parent))
                {
                    parent.children.lock().remove(&myself.get_id());
                    myself.get_cell().unlink(parent.myself.get_cell());
                }
                let terminal = finish_actor(
                    &myself,
                    state,
                    Disposition::Replaced {
                        successor: pending.successor.identity(),
                        summary,
                    },
                )
                .await;
                for reply in pending.shutdown_waiters {
                    let _ = reply.send(terminal.clone());
                }
                if let Some(reply) = pending.reply {
                    let _ = reply.send(Ok(pending.successor));
                }
                return Ok(());
            }
            KernelMessage::ActivateReplacement {
                mut backlog,
                draining,
            } => {
                backlog.append(&mut state.deferred_mailbox);
                state.deferred_mailbox = backlog;
                if draining {
                    state.mailbox_admission.close();
                    state.drain = DrainState::Draining;
                }
                match state.behavior.activate_replacement(&state.context).await {
                    Ok(step) => finish_after_step(&myself, state, step).await,
                    Err(error) => fail_actor(&myself, state, error.to_string()).await,
                }
            }
            KernelMessage::Drain { reply } => {
                if state.replacement.is_some() {
                    let _ = reply.send(Err(KernelBehaviorError {
                        detail: "actor replacement is in progress".into(),
                    }));
                    return Ok(());
                }
                let result = state.behavior.begin_drain().and_then(|()| {
                    if !matches!(state.drain, DrainState::Open) {
                        return Ok(());
                    }
                    state
                        .mailbox_admission
                        .close_with_fence(&myself)
                        .map_err(|error| KernelBehaviorError {
                            detail: error.to_string(),
                        })?;
                    state.behavior.close_sources();
                    state.drain = DrainState::Fencing;
                    Ok(())
                });
                let _ = reply.send(result);
            }
            KernelMessage::DrainFence => {
                state.drain = DrainState::Draining;
            }
            KernelMessage::Source(delivery) => {
                match state.behavior.source(&state.context, delivery).await {
                    Ok(step) => finish_after_step(&myself, state, step).await,
                    Err(error) => {
                        fail_handler(&myself, state, format!("source handler failed: {error}"))
                            .await
                    }
                }
            }
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
                        fail_handler(&myself, state, format!("actor cast failed: {error}")).await
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
                        fail_handler(&myself, state, format!("actor call failed: {detail}")).await;
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
            KernelMessage::ReconcileWorkbenchBoundary { boundary, reply } => {
                let outcome = state
                    .behavior
                    .reconcile_workbench_boundary(&state.context, boundary)
                    .await
                    .unwrap_or(crate::WorkbenchBoundaryReconciliation::Pending);
                let _ = reply.send(outcome);
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
            KernelMessage::Workbench {
                request,
                control,
                reply,
            } => {
                if !matches!(state.hosted_admission, HostedAdmission::Open) {
                    let _ = reply.send(Err(KernelInvocationFailure::Rejected {
                        actor: state.context.identity,
                        detail: "hosted work admission is sealed".into(),
                    }));
                    return Ok(());
                }

                match state
                    .behavior
                    .workbench(&state.context, request, control)
                    .await
                {
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
            KernelMessage::ReconcileWorkbenchCancellation {
                execution,
                invocation,
                reply,
            } => {
                let outcome = state
                    .behavior
                    .reconcile_workbench_cancellation(execution, invocation);
                let _ = reply.send(outcome);
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
                    fail_handler(
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
                if let Some(replacement) = &mut state.replacement {
                    replacement.shutdown_waiters.push(reply);
                    return Ok(());
                }
                let terminal = finish_actor(&myself, state, Disposition::Stop(terminal)).await;
                let _ = reply.send(terminal);
            }
        }
        if state.replacement.is_none()
            && matches!(state.drain, DrainState::Draining)
            && state.deferred_mailbox.is_empty()
            && state.behavior.accepts_mailbox()
            && state.terminal.get().is_none()
        {
            match state.behavior.drain(&state.context).await {
                Ok(step) => finish_after_step(&myself, state, step).await,
                Err(error) => {
                    fail_handler(&myself, state, format!("actor drain failed: {error}")).await
                }
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
        let resource = state.context.resources.lock().get(&cell.get_id()).cloned();
        if let Some(resource) = resource {
            if matches!(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    (resource.cleanup)(ResourceCleanup::Observe)
                )
                .await,
                Ok(crate::CleanupComponentOutcome::Confirmed)
            ) {
                state.context.resources.lock().remove(&cell.get_id());
            }
            return Ok(());
        }
        let child = state.context.children.lock().get(&cell.get_id()).cloned();
        let Some(child) = child else {
            // Cleanup forgets a child before its supervisor's final lifecycle
            // event can arrive; the late event carries nothing to act on.
            tracing::debug!(child = %cell.get_id(), "received lifecycle event for an already forgotten child");
            return Ok(());
        };
        if child.terminal().get().is_none() {
            publish_exit(child.terminal(), &observed, ExitAuthority::SupervisorForced);
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
    if state.replacement.is_none()
        && state.terminal.get().is_none()
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
    let identity = directory
        .reserve(incarnation)
        .map_err(|error| ractor::SpawnErr::StartupFailed(std::io::Error::other(error).into()))?;
    spawn_reserved_local_actor(name, behavior, identity, directory).await
}

pub(crate) async fn spawn_local_actor_in_directory_with_identity<B>(
    name: Option<String>,
    behavior: B,
    identity: ActorRef,
    directory: LocalActorDirectory,
) -> Result<(LocalActorRef, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr>
where
    B: KernelBehavior,
{
    directory
        .claim_exact(identity)
        .map_err(|error| ractor::SpawnErr::StartupFailed(std::io::Error::other(error).into()))?;
    spawn_reserved_local_actor(name, behavior, identity, directory).await
}

async fn spawn_reserved_local_actor<B>(
    name: Option<String>,
    behavior: B,
    identity: ActorRef,
    directory: LocalActorDirectory,
) -> Result<(LocalActorRef, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr>
where
    B: KernelBehavior,
{
    let terminal = RetainedActorExit::new();
    let mailbox_admission = crate::kernel::MailboxAdmission::default();
    let (address, task) = Actor::spawn(
        name,
        LocalActor::<B>(PhantomData),
        LocalActorArguments {
            behavior,
            terminal: terminal.clone(),
            directory,
            identity,
            mailbox_admission: mailbox_admission.clone(),
        },
    )
    .await?;
    Ok((
        LocalActorRef::with_identity_admission(address, terminal, identity, mailbox_admission),
        task,
    ))
}

/// A handler failure pauses the actor so replacement can recover it in
/// place — unless a drain was already requested. Once draining, the owner
/// asked this actor to finish accepted work and exit; parking on a failure
/// instead would leave `drainActor >> awaitExit` waiting with no exit and no
/// error. Q1-B: a failing queued message behaves like a failing drain
/// continuation, which already fails outright.
async fn fail_handler<B>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    detail: String,
) where
    B: KernelBehavior,
{
    if matches!(state.drain, DrainState::Open)
        && state.behavior.pause_failed_handler(&state.context, &detail)
    {
        return;
    }
    fail_actor(myself, state, detail).await;
}

async fn fail_actor<B>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    detail: String,
) where
    B: KernelBehavior,
{
    finish_actor(myself, state, Disposition::Stop(failed_terminal(detail))).await;
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
        finish_actor(myself, state, Disposition::Stop(terminal)).await;
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
    disposition: Disposition,
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
    state.mailbox_admission.close();
    // Wait for admitted startup to register, then permanently reject creation,
    // including through cloned contexts and shutdown hooks.
    *state.context.child_admission_closed.write().await = true;
    match disposition {
        Disposition::Stop(requested) => {
            // One shutdown deadline, computed once: hook admission and resource-scope
            // checkout race it directly, and children get the remainder minus
            // a margin so a child's own budget always expires first and it
            // reports its own `Unconfirmed` outcome instead of being killed.
            let deadline = tokio::time::Instant::now() + SHUTDOWN_BUDGET;
            let children_budget = SHUTDOWN_BUDGET.saturating_sub(SHUTDOWN_CHILD_MARGIN);
            let children = shutdown_children(&state.context, requested.kind, children_budget).await;
            let (hook, realm) = state
                .behavior
                .shutdown_components(&state.context, &requested, deadline)
                .await;
            // Q2-B: the exit kind is what was requested; cleanup uncertainty
            // is a separate, already-retained fact and never rewrites it.
            state
                .terminal
                .retain_cleanup(crate::ResidentCleanupOutcome {
                    actor: state.context.identity,
                    hook,
                    realm,
                    children,
                });
            let children_snapshot: Vec<LocalActorRef> =
                state.context.children.lock().values().cloned().collect();
            publish_exit(
                &state.terminal,
                &requested,
                ExitAuthority::Own {
                    children: &children_snapshot,
                },
            );
            state.behavior.stopped(&state.context, &requested).await;
            myself.stop(Some(requested.summary.clone()));
            requested
        }
        Disposition::Replaced { successor, summary } => {
            state.terminal.retain_successor(successor);
            let terminal = ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary,
            };
            // Custody of every child and resource already moved to the
            // successor before this call (the replacement fence transfers
            // them under `child_admission_closed`); cleanup is confirmed by
            // that transfer, not by a hook or resource scope this actor still owns.
            state
                .terminal
                .retain_cleanup(crate::ResidentCleanupOutcome {
                    actor: state.context.identity,
                    hook: crate::CleanupComponentOutcome::Confirmed,
                    realm: crate::CleanupComponentOutcome::Confirmed,
                    children: crate::CleanupComponentOutcome::Confirmed,
                });
            let children_snapshot: Vec<LocalActorRef> =
                state.context.children.lock().values().cloned().collect();
            publish_exit(
                &state.terminal,
                &terminal,
                ExitAuthority::Own {
                    children: &children_snapshot,
                },
            );
            state
                .behavior
                .replacement_retired(&state.context, &terminal);
            myself.stop(Some(terminal.summary.clone()));
            terminal
        }
    }
}

/// The only caller of [`RetainedActorExit::publish`] outside `termination`'s
/// own tests. `Own` asserts that every child already has an exit: the
/// snapshot is valid because `child_admission_closed` was set before it was
/// taken, and `shutdown_children`/the replacement fence's ownership transfer
/// both already guarantee the property. A violation does not withhold
/// publication — an owner with no exit at all is worse — it only records the
/// invariant break.
fn publish_exit(
    retained: &RetainedActorExit,
    terminal: &ActorTerminal,
    authority: ExitAuthority<'_>,
) {
    if let ExitAuthority::Own { children } = &authority {
        let all_children_exited = children
            .iter()
            .all(|child| child.terminal().get().is_some());
        debug_assert!(
            all_children_exited,
            "actor published its own exit before every child had one: {terminal:?}"
        );
        if !all_children_exited {
            tracing::error!(
                ?terminal,
                "actor published its own exit before every child had an exit"
            );
        }
    }
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
    owner_exit: ActorExitKind,
    timeout: Duration,
) -> crate::CleanupComponentOutcome {
    let resources: Vec<_> = context.resources.lock().values().cloned().collect();
    for resource in &resources {
        resource.cell.stop(None);
    }
    let mut resource_shutdowns = FuturesUnordered::new();
    for resource in resources {
        resource_shutdowns.push(async move {
            match tokio::time::timeout(timeout, async {
                resource.cell.wait(Some(timeout)).await?;
                Ok::<_, ractor::concurrency::Timeout>(
                    (resource.cleanup)(ResourceCleanup::Retire).await,
                )
            })
            .await
            {
                Ok(Ok(outcome)) => outcome,
                _ => crate::CleanupComponentOutcome::Unconfirmed(
                    "resource child cleanup is unconfirmed".into(),
                ),
            }
        });
    }
    while let Some(outcome) = resource_shutdowns.next().await {
        let mut retained = context.forgotten_children.lock();
        *retained = combine_cleanup(retained.clone(), outcome);
    }
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
                kind: match owner_exit {
                    ActorExitKind::Failed => ActorExitKind::Failed,
                    ActorExitKind::Completed | ActorExitKind::Cancelled => ActorExitKind::Cancelled,
                },
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
                        publish_exit(
                            child.terminal(),
                            &requested,
                            ExitAuthority::SupervisorForced,
                        );
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use crate::ActorLifecycle;
    use std::sync::Arc;

    use exomonad_tool::{ToolArguments, ToolInvocation};
    use parking_lot::Mutex;
    use tidepool_repr::SessionId;
    use tidepool_runtime::session::{WorkbenchResponse, WorkbenchRunStatus};
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
        replacement_staged: bool,
        startup_gate: Option<(Arc<Notify>, Arc<Notify>)>,
        calls: Arc<Mutex<Vec<&'static str>>>,
        mailbox_calls: Arc<Mutex<Vec<SessionId>>>,
        release_first: Arc<Notify>,
        fail_cast: bool,
        mailbox_ready: bool,
        spawned_child: Arc<Mutex<Option<LocalActorRef>>>,
        child_exits: Arc<Mutex<Vec<ActorTerminal>>>,
        /// Behaviors for children spawned by successive `"spawn_queued"` tool
        /// invocations, consumed in order.
        pending_children: VecDeque<ProbeBehavior>,
        spawned_children: Arc<Mutex<Vec<LocalActorRef>>>,
        /// Overrides `shutdown_components` for the one-shutdown-deadline and
        /// Q2-B tests; `None` keeps the trait's default (delegate to
        /// `shutdown`, realm `Unsupported`).
        shutdown_override: Option<ShutdownOverride>,
    }

    #[derive(Clone)]
    enum ShutdownOverride {
        Fixed(
            crate::CleanupComponentOutcome,
            crate::CleanupComponentOutcome,
        ),
        HangRealmUntilDeadline,
        HangRealmForever(Arc<Notify>),
    }

    impl KernelBehavior for ProbeBehavior {
        fn replacement_staged(&self) -> bool {
            self.replacement_staged
        }

        fn activate_replacement<'a>(
            &'a mut self,
            _context: &'a KernelContext,
        ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
            Box::pin(async move {
                if !self.replacement_staged {
                    return Err(KernelBehaviorError {
                        detail: "probe is not staged".into(),
                    });
                }
                self.replacement_staged = false;
                self.mailbox_ready = true;
                Ok(KernelStep::Continue(()))
            })
        }
        fn begin_drain(&mut self) -> Result<(), KernelBehaviorError> {
            Ok(())
        }

        fn shutdown_components<'a>(
            &'a mut self,
            context: &'a KernelContext,
            terminal: &'a ActorTerminal,
            deadline: tokio::time::Instant,
        ) -> BoxFuture<
            'a,
            (
                crate::CleanupComponentOutcome,
                crate::CleanupComponentOutcome,
            ),
        > {
            use crate::CleanupComponentOutcome::{Confirmed, Unconfirmed, Unsupported};
            match self.shutdown_override.clone() {
                None => Box::pin(async move {
                    let hook = match self.shutdown(context, terminal).await {
                        Ok(()) => Confirmed,
                        Err(error) => Unconfirmed(error.to_string()),
                    };
                    (hook, Unsupported)
                }),
                Some(ShutdownOverride::Fixed(hook, realm)) => {
                    self.calls.lock().push("shutdown");
                    Box::pin(async move { (hook, realm) })
                }
                Some(ShutdownOverride::HangRealmUntilDeadline) => {
                    self.calls.lock().push("shutdown");
                    Box::pin(async move {
                        let realm = match tokio::time::timeout_at(
                            deadline,
                            tokio::time::sleep(Duration::MAX),
                        )
                        .await
                        {
                            Ok(_) => Confirmed,
                            Err(_) => Unconfirmed(
                                "realm cleanup did not confirm before the shutdown deadline".into(),
                            ),
                        };
                        (Confirmed, realm)
                    })
                }
                Some(ShutdownOverride::HangRealmForever(notify)) => {
                    self.calls.lock().push("shutdown");
                    Box::pin(async move {
                        notify.notified().await;
                        (Confirmed, Confirmed)
                    })
                }
            }
        }

        fn drain<'a>(
            &'a mut self,
            _context: &'a KernelContext,
        ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
            self.calls.lock().push("drain");
            Box::pin(async {
                Ok(KernelStep::Stop {
                    output: (),
                    terminal: ActorTerminal {
                        kind: ActorExitKind::Completed,
                        summary: "drained".into(),
                    },
                })
            })
        }
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
            self.mailbox_calls.lock().push(request.session());
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
                } else if name == "spawn_queued" {
                    let Some(child_behavior) = self.pending_children.pop_front() else {
                        return Err(KernelInvocationFailure::Rejected {
                            actor: context.identity(),
                            detail: "no queued child behavior".into(),
                        });
                    };
                    let child =
                        context
                            .spawn_child(None, child_behavior)
                            .await
                            .map_err(|error| KernelInvocationFailure::Failed {
                                actor: context.identity(),
                                detail: error.to_string(),
                            })?;
                    self.spawned_children.lock().push(child);
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
            _control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
        ) -> BoxFuture<'_, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>> {
            Box::pin(async {
                Ok(KernelStep::Continue(WorkbenchResponse {
                    status: WorkbenchRunStatus::Committed,
                    summary: None,
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
            _control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
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
        mailbox_calls: Arc<Mutex<Vec<SessionId>>>,
        release: Arc<Notify>,
        spawned_child: Arc<Mutex<Option<LocalActorRef>>>,
        child_exits: Arc<Mutex<Vec<ActorTerminal>>>,
    }

    fn behavior(fail_cast: bool) -> ProbeFixture {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mailbox_calls = Arc::new(Mutex::new(Vec::new()));
        let release = Arc::new(Notify::new());
        let spawned_child = Arc::new(Mutex::new(None));
        let child_exits = Arc::new(Mutex::new(Vec::new()));
        let spawned_children = Arc::new(Mutex::new(Vec::new()));
        ProbeFixture {
            behavior: ProbeBehavior {
                replacement_staged: false,
                startup_gate: None,
                calls: Arc::clone(&calls),
                mailbox_calls: Arc::clone(&mailbox_calls),
                release_first: Arc::clone(&release),
                fail_cast,
                mailbox_ready: true,
                spawned_child: Arc::clone(&spawned_child),
                child_exits: Arc::clone(&child_exits),
                pending_children: VecDeque::new(),
                spawned_children,
                shutdown_override: None,
            },
            calls,
            mailbox_calls,
            release,
            spawned_child,
            child_exits,
        }
    }

    #[tokio::test]
    async fn prepared_replacement_defers_shutdown_until_activation() {
        let mut probe = behavior(false);
        probe.behavior.replacement_staged = true;
        probe.behavior.mailbox_ready = false;
        let (actor, task) = spawn_local_actor(None, probe.behavior).await.unwrap();
        let (reply, receive) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Shutdown {
                terminal: ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "queued shutdown".into(),
                },
                reply: reply.into(),
            })
            .unwrap();
        actor
            .address()
            .send_message(KernelMessage::ActivateReplacement {
                backlog: VecDeque::new(),
                draining: false,
            })
            .unwrap();
        let terminal = tokio::time::timeout(Duration::from_secs(2), receive)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(terminal.kind, ActorExitKind::Cancelled);
        task.await.unwrap();
        assert!(matches!(
            actor.terminal().cleanup().unwrap().realm,
            crate::CleanupComponentOutcome::Unsupported
        ));
    }

    #[tokio::test]
    async fn replacement_activation_preserves_backlog_order_and_drain_intent() {
        let mut probe = behavior(false);
        probe.behavior.replacement_staged = true;
        probe.behavior.mailbox_ready = false;
        let (actor, task) = spawn_local_actor(None, probe.behavior).await.unwrap();
        let caller = ActorRef::first(crate::ActorId(99));
        let dropped = Arc::new(AtomicUsize::new(0));
        let make_call = |session| {
            let (reply, receive) = oneshot::channel();
            let message = KernelMessage::Call {
                caller,
                ancestry: crate::CallAncestry::begin(caller),
                request: MailboxValue::probe(SessionId(session), Arc::clone(&dropped)),
                reply: reply.into(),
            };
            (message, receive)
        };
        let (before_fence, before_reply) = make_call(1);
        let (after_fence, after_reply) = make_call(2);
        // New-source input can reach the quiet successor before activation.
        actor.address().send_message(after_fence).unwrap();
        actor
            .address()
            .send_message(KernelMessage::ActivateReplacement {
                backlog: VecDeque::from([before_fence]),
                draining: true,
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            drop(before_reply.await.unwrap().unwrap());
            drop(after_reply.await.unwrap().unwrap());
            assert_eq!(actor.terminal().wait().await.kind, ActorExitKind::Completed);
            task.await.unwrap();
        })
        .await
        .unwrap();
        assert_eq!(*probe.mailbox_calls.lock(), [SessionId(1), SessionId(2)]);
        assert_eq!(*probe.calls.lock(), ["call", "call", "drain", "shutdown"]);
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        assert!(actor
            .cast(
                caller,
                MailboxValue::probe(SessionId(3), Arc::clone(&dropped))
            )
            .is_err());
        assert_eq!(dropped.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn prepared_replacement_abort_cleans_only_an_unactivated_actor() {
        let (active, active_task) = spawn_local_actor(None, behavior(false).behavior)
            .await
            .unwrap();
        assert!(active.abort_prepared_replacement().await.is_err());
        assert!(active.terminal().get().is_none());
        let mut probe = behavior(false).behavior;
        probe.replacement_staged = true;
        probe.mailbox_ready = false;
        let (prepared, task) = spawn_local_actor(None, probe).await.unwrap();
        prepared
            .abort_prepared_replacement()
            .await
            .expect_err("generic probe cannot attest resident realm cleanup");
        task.await.unwrap();
        assert!(matches!(
            prepared.terminal().cleanup().unwrap().realm,
            crate::CleanupComponentOutcome::Unsupported
        ));
        active
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "test finished".into(),
            })
            .await
            .unwrap();
        active_task.await.unwrap();
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
    async fn drain_racing_shutdown_observes_the_exact_retained_terminal() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let (drained, shutdown) = tokio::join!(
            actor.drain(),
            actor.shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "owner stopped".into()
            }),
        );
        drained.unwrap();
        assert_eq!(shutdown.unwrap(), actor.terminal().wait().await);
        task.await.unwrap();
        actor.drain().await.unwrap();
    }

    #[tokio::test]
    async fn drain_fence_finishes_accepted_calls_before_stopping() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        actor
            .address()
            .call(
                |reply| KernelMessage::Tool {
                    invocation: tool_invocation("park-mailbox"),
                    reply,
                },
                None,
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let caller = ActorRef::first(crate::ActorId(99));
        let dropped = Arc::new(AtomicUsize::new(0));
        let (reply, receive) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Call {
                caller,
                ancestry: crate::CallAncestry::begin(caller),
                request: MailboxValue::probe(SessionId(1), Arc::clone(&dropped)),
                reply: reply.into(),
            })
            .unwrap();
        actor.drain().await.unwrap();
        assert!(actor.terminal().get().is_none());
        assert_eq!(
            actor.cast(
                caller,
                MailboxValue::probe(SessionId(1), Arc::clone(&dropped))
            ),
            Err(KernelCallFailure::MailboxClosed(actor.identity()))
        );
        actor
            .address()
            .call(
                |reply| KernelMessage::Tool {
                    invocation: tool_invocation("unpark-mailbox"),
                    reply,
                },
                None,
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(receive.await.unwrap().unwrap());
        assert_eq!(actor.terminal().wait().await.kind, ActorExitKind::Completed);
        task.await.unwrap();
        assert_eq!(
            &*fixture.calls.lock(),
            &[
                "park-mailbox",
                "unpark-mailbox",
                "call",
                "drain",
                "shutdown"
            ]
        );
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn mailbox_admission_is_shared_with_shutdown_and_releases_rejected_inputs() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let handle = actor.clone();
        let caller = ActorRef::first(crate::ActorId(99));
        let dropped = Arc::new(AtomicUsize::new(0));
        let reply = handle
            .call(
                caller,
                crate::CallAncestry::begin(caller),
                MailboxValue::probe(SessionId(1), Arc::clone(&dropped)),
            )
            .await
            .unwrap();
        drop(reply);
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
            })
            .await
            .unwrap();
        task.await.unwrap();
        assert_eq!(
            handle.cast(
                caller,
                MailboxValue::probe(SessionId(1), Arc::clone(&dropped))
            ),
            Err(KernelCallFailure::MailboxClosed(actor.identity()))
        );
        assert!(matches!(handle.call(
            caller,
            crate::CallAncestry::begin(caller),
            MailboxValue::probe(SessionId(1), Arc::clone(&dropped)),
        ).await, Err(KernelCallFailure::MailboxClosed(target)) if target == actor.identity()));
        assert_eq!(dropped.load(Ordering::SeqCst), 3);
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
        assert_eq!(
            directory.context(owner.identity()).unwrap().identity(),
            owner.identity()
        );
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
        assert!(directory
            .context(owner.identity())
            .unwrap()
            .owns_child(child.identity()));
        owner
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "retire first tree".into(),
            })
            .await
            .expect("retire first");
        owner_task.await.expect("first task");
        assert!(directory.context(owner.identity()).is_none());
        assert!(directory.context(sibling.identity()).is_some());
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
    async fn logical_identity_recovery_requires_terminal_predecessor_and_rejects_old_handle() {
        let directory = LocalActorDirectory::default();
        let first_behavior = behavior(false);
        let (first, first_task) = spawn_local_actor_in_directory(
            None,
            first_behavior.behavior,
            crate::Incarnation::FIRST,
            directory.clone(),
        )
        .await
        .expect("first incarnation");
        let successor = crate::ActorRef {
            id: first.identity().id,
            incarnation: crate::Incarnation(2),
        };
        assert!(spawn_local_actor_in_directory_with_identity(
            None,
            behavior(false).behavior,
            successor,
            directory.clone(),
        )
        .await
        .is_err());

        first
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Failed,
                summary: "recoverable failure".into(),
            })
            .await
            .expect("retire predecessor");
        first_task.await.expect("predecessor task");
        let (second, second_task) = spawn_local_actor_in_directory_with_identity(
            None,
            behavior(false).behavior,
            successor,
            directory.clone(),
        )
        .await
        .expect("successor incarnation");
        assert_eq!(second.identity(), successor);
        assert_eq!(
            directory
                .resolve(first.identity())
                .map(|actor| actor.identity()),
            Some(first.identity())
        );
        assert_eq!(
            directory.resolve(successor).map(|actor| actor.identity()),
            Some(successor)
        );
        assert!(first
            .address()
            .send_message(KernelMessage::DrainMailbox)
            .is_err());

        second
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "test complete".into(),
            })
            .await
            .expect("retire successor");
        second_task.await.expect("successor task");
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
    async fn owner_failure_propagates_failure_instead_of_intentional_cancellation() {
        for kind in [ActorExitKind::Failed, ActorExitKind::Cancelled] {
            let fixture = behavior(false);
            let (owner, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
            let (reply, received) = oneshot::channel();
            owner
                .address()
                .send_message(KernelMessage::Tool {
                    invocation: tool_invocation("spawn"),
                    reply: reply.into(),
                })
                .unwrap();
            received.await.unwrap().unwrap();
            let child = fixture.spawned_child.lock().clone().unwrap();
            owner
                .shutdown(ActorTerminal {
                    kind,
                    summary: "stop owner".into(),
                })
                .await
                .unwrap();
            task.await.unwrap();
            assert_eq!(child.terminal().wait().await.kind, kind);
        }
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
            resources: Arc::new(Mutex::new(HashMap::new())),
            forgotten_children: Arc::new(Mutex::new(crate::CleanupComponentOutcome::Confirmed)),
        };
        let result =
            shutdown_children(&context, ActorExitKind::Cancelled, Duration::from_millis(1)).await;
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
        context
            .directory
            .insert(actor.clone(), std::sync::Weak::new());
        assert!(context.forget_terminal_actor(actor.identity()));
        assert!(context.children.lock().is_empty());
        assert!(
            matches!(
                shutdown_children(&context, ActorExitKind::Cancelled, Duration::from_millis(1))
                    .await,
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
                resources: Arc::new(Mutex::new(HashMap::new())),
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
            let outcome =
                shutdown_children(&context, ActorExitKind::Cancelled, Duration::from_secs(1)).await;
            // Probe behavior cannot prove resource-scope cleanup even on successful startup.
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

    // --- Actor exit contract (Q1-B, Q2-B, single publication owner, one
    // shutdown deadline) ---

    /// A behavior whose `pause_failed_handler` always retains the failed
    /// input, mirroring the resident actor's real pausing behavior.
    /// `ProbeBehavior` has no such override and always fails outright, which
    /// is not useful for exercising Q1-B (pausing must be possible at all
    /// before "skip the pause while draining" is a meaningful assertion).
    struct PausingProbe {
        calls: Arc<Mutex<Vec<&'static str>>>,
        block: Arc<Notify>,
        paused: Arc<AtomicBool>,
    }

    impl KernelBehavior for PausingProbe {
        fn begin_drain(&mut self) -> Result<(), KernelBehaviorError> {
            if self.paused.load(Ordering::SeqCst) {
                Err(KernelBehaviorError {
                    detail: "actor is paused".into(),
                })
            } else {
                Ok(())
            }
        }

        fn drain<'a>(
            &'a mut self,
            _context: &'a KernelContext,
        ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
            Box::pin(async {
                Ok(KernelStep::Stop {
                    output: (),
                    terminal: ActorTerminal {
                        kind: ActorExitKind::Completed,
                        summary: "drained".into(),
                    },
                })
            })
        }

        fn pause_failed_handler(&mut self, context: &KernelContext, detail: &str) -> bool {
            self.calls.lock().push("paused");
            self.paused.store(true, Ordering::SeqCst);
            if let Some(actor) = context.resolve(context.identity()) {
                actor.terminal().publish_paused(detail.to_owned());
            }
            true
        }

        fn shutdown_components<'a>(
            &'a mut self,
            _context: &'a KernelContext,
            _terminal: &'a ActorTerminal,
            _deadline: tokio::time::Instant,
        ) -> BoxFuture<
            'a,
            (
                crate::CleanupComponentOutcome,
                crate::CleanupComponentOutcome,
            ),
        > {
            Box::pin(async {
                (
                    crate::CleanupComponentOutcome::Confirmed,
                    crate::CleanupComponentOutcome::Confirmed,
                )
            })
        }

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
            request: MailboxValue,
        ) -> BoxFuture<'_, Result<KernelStep<()>, KernelBehaviorError>> {
            let session = request.session();
            let block = Arc::clone(&self.block);
            Box::pin(async move {
                if session == SessionId(1) {
                    block.notified().await;
                    Ok(KernelStep::Continue(()))
                } else {
                    Err(KernelBehaviorError {
                        detail: "second cast failed".into(),
                    })
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

        fn tool<'a>(
            &'a mut self,
            context: &'a KernelContext,
            _invocation: ToolInvocation,
        ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>> {
            Box::pin(async move {
                Err(KernelInvocationFailure::Rejected {
                    actor: context.identity(),
                    detail: "pausing probe has no tools".into(),
                })
            })
        }

        fn workbench(
            &mut self,
            _context: &KernelContext,
            _request: WorkbenchRequest,
            _control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
        ) -> BoxFuture<'_, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>> {
            Box::pin(async {
                Ok(KernelStep::Continue(WorkbenchResponse {
                    status: WorkbenchRunStatus::Committed,
                    summary: None,
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

    /// Q1-B: a handler failure while a drain was already requested ends the
    /// actor `Failed` immediately, never `Paused`.
    #[tokio::test(start_paused = true)]
    async fn paused_handler_while_draining_publishes_failed_exit() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let block = Arc::new(Notify::new());
        let paused = Arc::new(AtomicBool::new(false));
        let probe = PausingProbe {
            calls: Arc::clone(&calls),
            block: Arc::clone(&block),
            paused,
        };
        let (actor, task) = spawn_local_actor(None, probe).await.unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let _connection = actor
            .terminal()
            .connect_lifecycle(move |event| tx.send(event).is_ok());

        let dropped = Arc::new(AtomicUsize::new(0));
        let sender = ActorRef::first(crate::ActorId(1));
        actor
            .address()
            .send_message(KernelMessage::Cast {
                sender,
                request: MailboxValue::probe(SessionId(1), Arc::clone(&dropped)),
            })
            .unwrap();
        // Let the actor start processing the first (blocking) cast before the
        // drain request and the second cast are enqueued behind it.
        tokio::task::yield_now().await;

        let (drain_reply, drain_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Drain {
                reply: drain_reply.into(),
            })
            .unwrap();
        actor
            .address()
            .send_message(KernelMessage::Cast {
                sender,
                request: MailboxValue::probe(SessionId(2), Arc::clone(&dropped)),
            })
            .unwrap();

        block.notify_one();

        drain_rx.await.unwrap().unwrap();
        let terminal = actor.terminal().wait().await;
        task.await.unwrap();

        assert_eq!(terminal.kind, ActorExitKind::Failed);
        assert!(terminal.summary.contains("second cast failed"));
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                ActorLifecycle::Live,
                ActorLifecycle::Exited(terminal.clone())
            ]
        );
        assert!(
            calls.lock().is_empty(),
            "pause_failed_handler must not run once a drain was requested"
        );
        assert!(actor.terminal().cleanup().unwrap().is_confirmed());
    }

    /// A pause that happens *before* a drain was requested is unaffected by
    /// Q1-B: `begin_drain` keeps rejecting a paused actor explicitly.
    #[tokio::test(start_paused = true)]
    async fn pause_before_drain_still_rejects_drain_and_stays_live() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let block = Arc::new(Notify::new());
        let paused = Arc::new(AtomicBool::new(false));
        let probe = PausingProbe {
            calls: Arc::clone(&calls),
            block,
            paused,
        };
        let (actor, task) = spawn_local_actor(None, probe).await.unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let _connection = actor
            .terminal()
            .connect_lifecycle(move |event| tx.send(event).is_ok());

        let dropped = Arc::new(AtomicUsize::new(0));
        let sender = ActorRef::first(crate::ActorId(1));
        actor
            .address()
            .send_message(KernelMessage::Cast {
                sender,
                request: MailboxValue::probe(SessionId(2), Arc::clone(&dropped)),
            })
            .unwrap();
        for _ in 0..1000 {
            if !calls.lock().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(&*calls.lock(), &["paused"]);

        let rejection = actor.drain().await.expect_err("paused actor rejects drain");
        assert!(matches!(
            rejection,
            crate::KernelInvocationFailure::Rejected { .. }
        ));
        assert!(actor.terminal().get().is_none());

        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "test finished".into(),
            })
            .await
            .unwrap();
        task.await.unwrap();

        let events = rx.try_iter().collect::<Vec<_>>();
        assert_eq!(events[0], ActorLifecycle::Live);
        assert!(matches!(events[1], ActorLifecycle::Paused(_)));
        assert!(matches!(
            events.last().unwrap(),
            ActorLifecycle::Exited(terminal) if terminal.kind == ActorExitKind::Cancelled
        ));
    }

    /// Q2-B: unconfirmed hook/resource-scope cleanup no longer rewrites the exit kind
    /// to `Failed`. The requested kind is preserved and cleanup uncertainty
    /// stays a separate, already-retained fact.
    #[tokio::test(start_paused = true)]
    async fn unconfirmed_cleanup_preserves_requested_exit_kind() {
        for kind in [ActorExitKind::Completed, ActorExitKind::Cancelled] {
            let mut fixture = behavior(false);
            fixture.behavior.shutdown_override = Some(ShutdownOverride::Fixed(
                crate::CleanupComponentOutcome::Unconfirmed("hook".into()),
                crate::CleanupComponentOutcome::Confirmed,
            ));
            let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let _connection = actor
                .terminal()
                .connect_lifecycle(move |event| tx.send(event).is_ok());

            let terminal = actor
                .shutdown(ActorTerminal {
                    kind,
                    summary: "test".into(),
                })
                .await
                .unwrap();
            task.await.unwrap();

            assert_eq!(terminal.kind, kind);
            assert!(!actor.terminal().cleanup().unwrap().is_confirmed());
            assert_eq!(
                rx.try_iter().collect::<Vec<_>>(),
                vec![
                    ActorLifecycle::Live,
                    ActorLifecycle::Exited(terminal.clone())
                ]
            );
        }
    }

    /// Single publication owner: the owner's own exit is never published
    /// (via `finish_actor`/`publish_exit`) before every child already has an
    /// exit.
    #[tokio::test(start_paused = true)]
    async fn owner_exit_follows_every_child_exit() {
        let events: Arc<Mutex<Vec<(&'static str, ActorLifecycle)>>> =
            Arc::new(Mutex::new(Vec::new()));

        let mut fixture = behavior(false);
        fixture
            .behavior
            .pending_children
            .push_back(behavior(false).behavior);
        fixture
            .behavior
            .pending_children
            .push_back(behavior(false).behavior);
        let spawned_children = Arc::clone(&fixture.behavior.spawned_children);
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();

        let mut connections = Vec::new();
        let parent_events = Arc::clone(&events);
        connections.push(actor.terminal().connect_lifecycle(move |event| {
            parent_events.lock().push(("parent", event));
            true
        }));

        for _ in 0..2 {
            let (reply, receive) = oneshot::channel();
            actor
                .address()
                .send_message(KernelMessage::Tool {
                    invocation: tool_invocation("spawn_queued"),
                    reply: reply.into(),
                })
                .unwrap();
            receive.await.unwrap().unwrap();
        }
        let children = spawned_children.lock().clone();
        assert_eq!(children.len(), 2);
        for child in &children {
            let child_events = Arc::clone(&events);
            connections.push(child.terminal().connect_lifecycle(move |event| {
                child_events.lock().push(("child", event));
                true
            }));
        }

        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "owner done".into(),
            })
            .await
            .unwrap();
        task.await.unwrap();
        drop(connections);

        let log = events.lock().clone();
        let parent_exit = log
            .iter()
            .position(|(who, event)| *who == "parent" && matches!(event, ActorLifecycle::Exited(_)))
            .expect("parent exit recorded");
        let child_exits: Vec<_> = log
            .iter()
            .enumerate()
            .filter(|(_, (who, event))| {
                *who == "child" && matches!(event, ActorLifecycle::Exited(_))
            })
            .map(|(index, _)| index)
            .collect();
        assert_eq!(child_exits.len(), 2);
        assert!(
            child_exits.iter().all(|&index| index < parent_exit),
            "both child exits must precede the owner's exit: {log:?}"
        );
    }

    /// W3: a child whose `shutdown_components` never confirms is forced
    /// (published `Cancelled` by the supervisor, then killed) once the
    /// children's share of the shutdown deadline elapses, so the owner's own
    /// exit still publishes rather than hanging on the hung child.
    #[tokio::test(start_paused = true)]
    async fn hung_child_is_forced_before_owner_publishes() {
        let events: Arc<Mutex<Vec<(&'static str, ActorLifecycle)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let hang = Arc::new(Notify::new());

        let mut fixture = behavior(false);
        let mut child = behavior(false).behavior;
        child.shutdown_override = Some(ShutdownOverride::HangRealmForever(Arc::clone(&hang)));
        fixture.behavior.pending_children.push_back(child);
        let spawned_children = Arc::clone(&fixture.behavior.spawned_children);
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();

        let (reply, receive) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Tool {
                invocation: tool_invocation("spawn_queued"),
                reply: reply.into(),
            })
            .unwrap();
        receive.await.unwrap().unwrap();
        let child_ref = spawned_children.lock()[0].clone();

        let mut connections = Vec::new();
        let parent_events = Arc::clone(&events);
        connections.push(actor.terminal().connect_lifecycle(move |event| {
            parent_events.lock().push(("parent", event));
            true
        }));
        let child_events = Arc::clone(&events);
        connections.push(child_ref.terminal().connect_lifecycle(move |event| {
            child_events.lock().push(("child", event));
            true
        }));

        let terminal = actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "owner done".into(),
            })
            .await
            .unwrap();
        task.await.unwrap();
        drop(connections);

        assert_eq!(terminal.kind, ActorExitKind::Completed);
        assert!(matches!(
            actor.terminal().cleanup().unwrap().children(),
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
        assert!(child_ref.terminal().cleanup().is_none());
        let child_terminal = child_ref
            .terminal()
            .get()
            .expect("child was force-published");
        assert_eq!(child_terminal.kind, ActorExitKind::Cancelled);
        assert!(child_terminal.summary.contains("owner actor stopped"));

        let log = events.lock().clone();
        let child_exit = log
            .iter()
            .position(|(who, event)| *who == "child" && matches!(event, ActorLifecycle::Exited(_)))
            .expect("child exit recorded");
        let parent_exit = log
            .iter()
            .position(|(who, event)| *who == "parent" && matches!(event, ActorLifecycle::Exited(_)))
            .expect("parent exit recorded");
        assert!(child_exit < parent_exit);
    }

    /// One shutdown deadline: a resource-scope checkout that never confirms on its own
    /// is still bounded by `finish_actor`'s deadline, so exit publication
    /// cannot be delayed unboundedly by a busy machine.
    #[tokio::test(start_paused = true)]
    async fn busy_machine_cannot_delay_exit_past_deadline() {
        let mut fixture = behavior(false);
        fixture.behavior.shutdown_override = Some(ShutdownOverride::HangRealmUntilDeadline);
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();

        let start = tokio::time::Instant::now();
        let terminal = actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
            })
            .await
            .unwrap();
        task.await.unwrap();

        assert_eq!(terminal.kind, ActorExitKind::Completed);
        assert_eq!(tokio::time::Instant::now() - start, SHUTDOWN_BUDGET);
        assert!(matches!(
            actor.terminal().cleanup().unwrap().realm(),
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
    }

    /// Single publication owner, W2: the replacement fence publishes the
    /// predecessor's exit through `finish_actor`'s `Disposition::Replaced`
    /// arm, the same function that publishes an ordinary stop.
    #[tokio::test(start_paused = true)]
    async fn replacement_fence_publishes_through_finish_actor() {
        let (successor, successor_task) = spawn_local_actor(None, behavior(false).behavior)
            .await
            .unwrap();
        // A real actor supplies a legitimate `RactorRef` for `finish_actor`'s
        // `myself.stop(..)`; its own internal state is otherwise unused here.
        let (placeholder, placeholder_task) = spawn_local_actor(None, behavior(false).behavior)
            .await
            .unwrap();

        let terminal = RetainedActorExit::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let _connection = terminal.connect_lifecycle(move |event| tx.send(event).is_ok());

        let context = std::sync::Arc::new(KernelContext {
            identity: placeholder.identity(),
            myself: placeholder.address().clone(),
            children: Arc::new(Mutex::new(HashMap::new())),
            directory: LocalActorDirectory::default(),
            child_admission_closed: Arc::new(tokio::sync::RwLock::new(false)),
            resources: Arc::new(Mutex::new(HashMap::new())),
            forgotten_children: Arc::new(Mutex::new(crate::CleanupComponentOutcome::Confirmed)),
        });
        let mut state = LocalActorState {
            replacement: None,
            drain: DrainState::Open,
            mailbox_admission: crate::kernel::MailboxAdmission::default(),
            hosted_admission: HostedAdmission::Open,
            context,
            behavior: behavior(false).behavior,
            terminal: terminal.clone(),
            deferred_mailbox: VecDeque::new(),
            mailbox_drain_scheduled: false,
        };

        let summary = format!("replaced by {:?}", successor.identity());
        let published = finish_actor(
            placeholder.address(),
            &mut state,
            Disposition::Replaced {
                successor: successor.identity(),
                summary: summary.clone(),
            },
        )
        .await;

        assert_eq!(
            published,
            ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary,
            }
        );
        assert_eq!(
            rx.try_iter().collect::<Vec<_>>(),
            vec![
                ActorLifecycle::Live,
                ActorLifecycle::Exited(published.clone())
            ]
        );
        assert_eq!(terminal.successor(), Some(successor.identity()));
        assert!(terminal.cleanup().unwrap().is_confirmed());

        placeholder_task.await.unwrap();
        successor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "test done".into(),
            })
            .await
            .unwrap();
        successor_task.await.unwrap();
    }
}
