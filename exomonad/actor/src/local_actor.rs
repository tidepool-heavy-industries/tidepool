//! Canonical Ractor wrapper for Tidepool actor behavior.

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::stream::{FuturesUnordered, StreamExt};
use futures_util::FutureExt;
use ractor::{Actor, ActorProcessingErr, ActorRef as RactorRef, SupervisionEvent};
use tracing::Instrument;

use crate::{
    ActorExitKind, ActorRef, ActorTerminal, ExternalApplicationFailure, ExternalFailureDisposition,
    KernelCallFailure, KernelInvocationFailure, KernelMessage, LocalActorRef, MailboxValue,
    RetainedActorExit,
};
use tidepool_runtime::session::WorkbenchResponse;

mod workbench_step;
pub use workbench_step::{
    ActorAbandonGuard, ActorAdvance, OwnedActorCompletion, OwnedActorTask,
    OwnedWorkbenchCompletion, OwnedWorkbenchTask, WorkbenchAbandonGuard, WorkbenchAdvance,
    WorkbenchDispatch,
};

use workbench_step::ActorTaskExecution;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct KernelBehaviorError {
    pub detail: String,
    pub diagnostic: Option<tidepool_toolchain::failclass::FailureEnvelope>,
}

impl KernelBehaviorError {
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            diagnostic: None,
        }
    }

    pub(crate) fn with_diagnostic(
        detail: String,
        diagnostic: Option<tidepool_toolchain::failclass::FailureEnvelope>,
    ) -> Self {
        Self {
            detail,
            diagnostic: crate::termination::retain_failure_diagnostic(diagnostic),
        }
    }

    fn context(mut self, detail: String) -> Self {
        self.detail = detail;
        self
    }
}

impl From<String> for KernelBehaviorError {
    fn from(detail: String) -> Self {
        Self::new(detail)
    }
}

impl From<&str> for KernelBehaviorError {
    fn from(detail: &str) -> Self {
        Self::new(detail)
    }
}

impl From<crate::ResidentActorWorkbenchError> for KernelBehaviorError {
    fn from(error: crate::ResidentActorWorkbenchError) -> Self {
        error.into_kernel_behavior_error()
    }
}

impl From<KernelInvocationFailure> for KernelBehaviorError {
    fn from(error: KernelInvocationFailure) -> Self {
        error.into_behavior_error()
    }
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
    /// The actor lost the task that owned its behavior. It can report failed
    /// execution and unconfirmed cleanup, but cannot run behavior shutdown.
    OwnerLostExecution,
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
    terminal: RetainedActorExit,
    spawn_ownership: SpawnOwnership,
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

/// Process-wide issuer for freshly reserved logical actor identities.
///
/// Every [`LocalActorDirectory`] in the process draws new IDs from this one
/// counter, so two `ResidentForest`s spawned in the same process never mint
/// colliding `ActorRef`s for unrelated actors. A *recovered* actor's ID is
/// never drawn from here: it is restored verbatim from that forest's own
/// recovery journal (`claim_exact`/`spawn_local_actor_in_directory_with_identity`)
/// and only fenced against this counter's future output.
static NEXT_ACTOR_ID: AtomicU64 = AtomicU64::new(1);

/// A recovered forest restores historical IDs without drawing them from
/// [`NEXT_ACTOR_ID`]; moving the issuer past each one keeps every later
/// fresh reservation, in any forest, from minting a recovered ID.
fn advance_actor_ids_past(actor: crate::ActorId) {
    NEXT_ACTOR_ID.fetch_max(actor.0.saturating_add(1), Ordering::Relaxed);
}

/// Ownership issued before Ractor runs `pre_start`; live supervision may not yet
/// be linked and remains independently queryable after replacement transfers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpawnOwnership {
    Independent,
    Supervised(ActorRef),
}

impl SpawnOwnership {
    pub(crate) fn is_independent(self) -> bool {
        matches!(self, Self::Independent)
    }
}

#[derive(Default)]
struct DirectoryMembership {
    closed: bool,
    entries: HashMap<ActorRef, DirectoryEntry>,
}

/// A sealed membership snapshot must issue cancellation before root cleanup.
pub(crate) struct SealedActorMembership {
    actors: Vec<(LocalActorRef, SpawnOwnership)>,
}

pub(crate) struct ForestRetirementBatch {
    _fence: crate::kernel::RetirementBatch,
    roots: Vec<LocalActorRef>,
}

impl SealedActorMembership {
    pub(crate) fn cancel_all(self, terminal: ActorTerminal) -> ForestRetirementBatch {
        let roots = self
            .actors
            .iter()
            .filter(|(_, ownership)| ownership.is_independent())
            .map(|(actor, _)| actor.clone())
            .collect();
        let fence = crate::kernel::RetirementBatch::issue(
            self.actors
                .into_iter()
                .map(|(actor, _)| (actor, terminal.clone()))
                .collect(),
        );
        ForestRetirementBatch {
            _fence: fence,
            roots,
        }
    }
}

impl ForestRetirementBatch {
    pub(crate) fn into_roots(self) -> Vec<LocalActorRef> {
        self.roots
    }
}

/// Routing and terminal-observation index for one resident actor forest.
///
/// This is deliberately not a scheduler or lifecycle state machine. Ractor
/// owns runnable actors and mailboxes; each actor owns its terminal cell. The
/// directory only resolves the identity carried by a live Haskell `ActorRef`
/// to that pair of owners. Entries intentionally live for the routing
/// domain's lifetime: an exited exact reference must remain resolvable so any
/// number of late `wait` operations can observe its retained result. Logical
/// actor IDs are process-unique (see [`NEXT_ACTOR_ID`]); this directory's own
/// `identities` bookkeeping tracks only which of those process-wide IDs are
/// fenced, claimed, or live in this particular forest.
#[derive(Clone, Default)]
pub struct LocalActorDirectory {
    actors: std::sync::Arc<parking_lot::RwLock<DirectoryMembership>>,
    sessions: std::sync::Arc<parking_lot::RwLock<HashMap<ActorRef, crate::ActorSessionContext>>>,
    identities: std::sync::Arc<parking_lot::Mutex<DirectoryIdentities>>,
}

#[derive(Default)]
struct DirectoryIdentities {
    runtime: HashMap<ractor::ActorId, ActorRef>,
    fenced: std::collections::HashSet<crate::ActorId>,
    claimed: HashMap<crate::ActorId, ActorRef>,
}

struct DirectoryEntry {
    actor: LocalActorRef,
    spawn_ownership: SpawnOwnership,
    // Retaining an exit must not keep its execution context (and thus this
    // directory) alive. Ractor's actor state owns the strong context reference.
    context: std::sync::Weak<KernelContext>,
}

impl LocalActorDirectory {
    fn reserve(&self, incarnation: crate::Incarnation) -> Result<ActorRef, String> {
        loop {
            let next = NEXT_ACTOR_ID.fetch_add(1, Ordering::Relaxed);
            let id = crate::ActorId(next);
            let mut identities = self.identities.lock();
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
            identities.fenced.insert(actor);
            advance_actor_ids_past(actor);
        }
        Ok(())
    }

    fn claim_exact(&self, actor: ActorRef) -> Result<(), String> {
        advance_actor_ids_past(actor.id);
        let mut identities = self.identities.lock();
        if let Some(previous) = identities.claimed.get(&actor.id).copied() {
            let predecessor_is_terminal = self
                .actors
                .read()
                .entries
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
        Ok(())
    }

    fn runtime_identity(&self, actor: ractor::ActorId) -> Option<ActorRef> {
        self.identities.lock().runtime.get(&actor).copied()
    }

    #[must_use]
    pub fn resolve(&self, actor: ActorRef) -> Option<LocalActorRef> {
        self.actors
            .read()
            .entries
            .get(&actor)
            .map(|entry| entry.actor.clone())
    }

    #[must_use]
    pub fn session_context(&self, actor: ActorRef) -> Option<crate::ActorSessionContext> {
        self.sessions.read().get(&actor).cloned()
    }

    fn insert(
        &self,
        actor: LocalActorRef,
        context: std::sync::Weak<KernelContext>,
        spawn_ownership: SpawnOwnership,
    ) -> Result<(), &'static str> {
        // Match claim_exact's identities -> membership lock order. No staged
        // actor becomes routable after the owning shutdown snapshot is sealed.
        let mut identities = self.identities.lock();
        let mut membership = self.actors.write();
        if membership.closed {
            return Err("actor forest admission is closed");
        }
        identities
            .runtime
            .insert(actor.address().get_id(), actor.identity());
        membership.entries.insert(
            actor.identity(),
            DirectoryEntry {
                actor,
                context,
                spawn_ownership,
            },
        );
        Ok(())
    }

    pub(crate) fn seal(&self) -> SealedActorMembership {
        let mut membership = self.actors.write();
        membership.closed = true;
        SealedActorMembership {
            actors: membership
                .entries
                .values()
                .map(|entry| (entry.actor.clone(), entry.spawn_ownership))
                .collect(),
        }
    }

    fn context(&self, actor: ActorRef) -> Option<std::sync::Arc<KernelContext>> {
        self.actors.read().entries.get(&actor)?.context.upgrade()
    }

    fn forget_terminal(&self, actor: ActorRef) -> bool {
        let mut actors = self.actors.write();
        let terminal = actors
            .entries
            .get(&actor)
            .is_some_and(|entry| entry.actor.terminal().get().is_some());
        if terminal {
            actors.entries.remove(&actor);
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
                .spawn_worker_retained(None, behavior, crate::WorkerLifetime::ActorOwned, None)
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

    pub(crate) fn spawn_ownership(&self) -> SpawnOwnership {
        self.spawn_ownership
    }

    pub(crate) fn requested_shutdown(&self) -> Option<ActorTerminal> {
        self.terminal.requested_shutdown()
    }

    pub(crate) fn retained_exit(&self) -> RetainedActorExit {
        self.terminal.clone()
    }

    pub(crate) fn retain_child_startup_cleanup(&self, outcome: crate::CleanupComponentOutcome) {
        let mut retained = self.forgotten_children.lock();
        *retained = combine_cleanup(retained.clone(), outcome);
    }

    pub(crate) async fn wait_requested_shutdown(&self) -> ActorTerminal {
        self.retained_exit().wait_requested_shutdown().await
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
                diagnostic: None,
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
        self.spawn_worker(name, behavior, crate::WorkerLifetime::ActorOwned)
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
        self.spawn_worker_retained(name, behavior, lifetime, None)
            .await
            .map(|(actor, _)| actor)
    }

    pub(crate) async fn spawn_worker_scoped<C: KernelBehavior>(
        &self,
        name: Option<String>,
        behavior: C,
        lifetime: crate::WorkerLifetime,
        admission: Arc<dyn WorkerStartupAdmission>,
    ) -> Result<LocalActorRef, ractor::SpawnErr> {
        self.spawn_worker_retained(name, behavior, lifetime, Some(admission))
            .await
            .map(|(actor, _)| actor)
    }

    async fn spawn_worker_retained<C: KernelBehavior>(
        &self,
        name: Option<String>,
        behavior: C,
        lifetime: crate::WorkerLifetime,
        startup_admission: Option<Arc<dyn WorkerStartupAdmission>>,
    ) -> Result<(LocalActorRef, tokio::sync::OwnedRwLockReadGuard<bool>), ractor::SpawnErr> {
        // Hold admission through registration so retirement cannot miss a
        // child whose startup is already in flight.
        let admission = self.child_admission_closed.clone().read_owned().await;
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
        let startup_refusal = if *admission {
            Some(StartupFailureCause::ChildAdmissionClosed)
        } else {
            startup_admission
                .as_ref()
                .and_then(|admission| admission.reserve(identity).err())
                .map(StartupFailureCause::Reservation)
        };
        // An already prepared behavior still owns real resources on refusal.
        // Run its ordinary cleanup in an unlinked startup, never registering or
        // starting authored work after the admission boundary has closed.
        let cleanup_only = startup_refusal.is_some();
        let arguments = LocalActorArguments {
            spawn_ownership: match lifetime {
                crate::WorkerLifetime::SwarmOwned => SpawnOwnership::Independent,
                crate::WorkerLifetime::InvocationOwned | crate::WorkerLifetime::ActorOwned => {
                    SpawnOwnership::Supervised(self.identity)
                }
            },
            behavior,
            startup_refusal,
            startup_admission,
            terminal: terminal.clone(),
            directory: self.directory.clone(),
            identity,
            mailbox_admission: mailbox_admission.clone(),
        };
        let spawned = if cleanup_only {
            Box::pin(LocalActor::<C>::spawn(
                name,
                LocalActor::<C>(PhantomData),
                arguments,
            ))
            .await
        } else {
            match lifetime {
                crate::WorkerLifetime::InvocationOwned | crate::WorkerLifetime::ActorOwned => {
                    Box::pin(self.myself.spawn_linked(
                        name,
                        LocalActor::<C>(PhantomData),
                        arguments,
                    ))
                    .await
                }
                crate::WorkerLifetime::SwarmOwned => {
                    Box::pin(LocalActor::<C>::spawn(
                        name,
                        LocalActor::<C>(PhantomData),
                        arguments,
                    ))
                    .await
                }
            }
        };
        let (address, task) = match spawned {
            Ok(spawned) => spawned,
            Err(error) => {
                if let Some(cleanup) = startup_cleanup(&error) {
                    self.retain_child_startup_cleanup(combine_cleanup(
                        combine_cleanup(cleanup.hook, cleanup.realm),
                        cleanup.children,
                    ));
                } else {
                    self.retain_child_startup_cleanup(crate::CleanupComponentOutcome::Unconfirmed(
                        format!("child startup failed without retained cleanup: {error}"),
                    ));
                }
                custody.accounted = true;
                return Err(error);
            }
        };
        drop(task);
        let child =
            LocalActorRef::with_identity_admission(address, terminal, identity, mailbox_admission);
        if lifetime != crate::WorkerLifetime::SwarmOwned {
            self.children
                .lock()
                .insert(child.address().get_id(), child.clone());
        }
        custody.accounted = true;
        Ok((child, admission))
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StartupFailureCause {
    #[error("actor child admission is closed")]
    ChildAdmissionClosed,
    #[error("actor forest admission is closed")]
    MembershipClosed,
    #[error("worker reservation refused: {0}")]
    Reservation(String),
    #[error("worker admission refused: {0}")]
    WorkerAdmission(String),
    #[error(transparent)]
    Behavior(KernelBehaviorError),
}

#[derive(Debug, thiserror::Error)]
#[error("{cause}")]
struct StartupRefusal {
    #[source]
    cause: StartupFailureCause,
    cleanup: Option<crate::ResidentCleanupOutcome>,
}

pub(crate) fn startup_cleanup(error: &ractor::SpawnErr) -> Option<crate::ResidentCleanupOutcome> {
    match error {
        ractor::SpawnErr::StartupFailed(error) => {
            error.downcast_ref::<StartupRefusal>()?.cleanup.clone()
        }
        _ => None,
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
                diagnostic: None,
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
                diagnostic: None,
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
                diagnostic: None,
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
            diagnostic: None,
        })
    }

    fn replacement_retired(&mut self, _context: &KernelContext, _terminal: &ActorTerminal) {}

    fn begin_drain(&mut self) -> Result<(), KernelBehaviorError> {
        Err(KernelBehaviorError {
            detail: "actor does not support draining".into(),
            diagnostic: None,
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
                diagnostic: None,
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
                diagnostic: None,
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
                diagnostic: None,
            })
        })
    }

    /// Whether the installed behavior is currently parked on its authored
    /// mailbox receiver. The wrapper retains cast/call ownership while an
    /// external interaction temporarily occupies that continuation.
    fn accepts_mailbox(&self) -> bool {
        true
    }

    /// Permit distinct owned workbench executions to overlap while parked.
    /// Stateful mailbox handlers and serial continuations remain exclusive.
    fn allows_independent_workbench_admission(&self) -> bool {
        false
    }

    /// Serialize administrative source/tool publication without occupying an
    /// independent notebook execution's continuation.
    fn serializes_workbench_publication(
        &self,
        _request: &tidepool_runtime::session::WorkbenchRequest,
    ) -> bool {
        false
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
        hosted_checkpoint_capture: Option<std::sync::Arc<dyn crate::HostedCheckpointCapture>>,
    ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>>;

    fn workbench<'a>(
        &'a mut self,
        context: &'a KernelContext,
        invocation: crate::ActorWorkbenchInvocation,
        control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
    ) -> BoxFuture<'a, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>>;

    /// Capture a resident tool call as an owned actor task. The default keeps
    /// its stateful effects in the existing serial behavior transfer.
    fn dispatch_tool(
        &mut self,
        _context: &KernelContext,
        invocation: exomonad_tool::ToolInvocation,
        hosted_checkpoint_capture: Option<std::sync::Arc<dyn crate::HostedCheckpointCapture>>,
        control: Arc<crate::WorkbenchExecutionControl>,
    ) -> OwnedActorTask<Self, serde_json::Value>
    where
        Self: Sized,
    {
        OwnedActorTask::serial(move |mut behavior: Self, context| {
            Box::pin(async move {
                let tool = behavior.tool(&context, invocation, hosted_checkpoint_capture);
                let result = crate::resident_workbench::with_execution_control(control, tool).await;
                (behavior, OwnedActorCompletion::new(move |_| result))
            })
        })
    }

    /// Hand execution-owned work to a task while retaining actor-owned state
    /// on the mailbox turn. The sequential form keeps an invocation intact
    /// for behaviors whose work remains actor-owned.
    fn dispatch_workbench(
        &mut self,
        _context: &KernelContext,
        invocation: crate::ActorWorkbenchInvocation,
        control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
    ) -> WorkbenchDispatch<Self>
    where
        Self: Sized,
    {
        WorkbenchDispatch::Sequential {
            invocation,
            control,
        }
    }

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

    fn tool_aborted<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> BoxFuture<'a, Result<(), KernelBehaviorError>> {
        Box::pin(async { Ok(()) })
    }

    /// Capture an internal continuation under the same single-admission
    /// scheduler. Behaviors with owned continuation inputs override this
    /// default serial transfer.
    fn dispatch_resume(
        &mut self,
        _context: &KernelContext,
        kind: crate::kernel::KernelResume,
    ) -> Result<OwnedActorTask<Self, ()>, KernelBehaviorError>
    where
        Self: Sized,
    {
        if kind != crate::kernel::KernelResume::ContinueProgram {
            return Err(KernelBehaviorError {
                detail: "actor has no child durability confirmation".into(),
                diagnostic: None,
            });
        }
        Ok(OwnedActorTask::serial(
            move |mut behavior: Self, context| {
                Box::pin(async move {
                    let result = behavior.resume(&context).await.map_err(|error| {
                        KernelInvocationFailure::Failed {
                            receipts: Vec::new(),
                            actor: context.identity(),
                            detail: error.detail,
                            diagnostic: error.diagnostic,
                        }
                    });
                    (behavior, OwnedActorCompletion::new(move |_| result))
                })
            },
        ))
    }

    /// Start an admitted child with its final inherited scope.
    fn release_fork<'a>(
        &'a mut self,
        _context: &'a KernelContext,
        _release: crate::ForkChildRelease,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async {
            Err(KernelBehaviorError {
                detail: "actor has no deferred fork".into(),
                diagnostic: None,
            })
        })
    }

    /// Capture one admitted child release. The default transfers the original
    /// behavior to its existing serial driver; resident initialization replaces
    /// this with tasks that retain their own inputs.
    fn dispatch_release_fork(
        &mut self,
        _context: &KernelContext,
        release: crate::ForkChildRelease,
    ) -> Result<OwnedActorTask<Self, ()>, KernelBehaviorError>
    where
        Self: Sized,
    {
        Ok(OwnedActorTask::serial(
            move |mut behavior: Self, context| {
                Box::pin(async move {
                    let result = behavior
                        .release_fork(&context, release)
                        .await
                        .map_err(|error| KernelInvocationFailure::Failed {
                            receipts: Vec::new(),
                            actor: context.identity(),
                            detail: error.detail,
                            diagnostic: error.diagnostic,
                        });
                    (behavior, OwnedActorCompletion::new(move |_| result))
                })
            },
        ))
    }

    fn resume<'a>(
        &'a mut self,
        _context: &'a KernelContext,
    ) -> BoxFuture<'a, Result<KernelStep<()>, KernelBehaviorError>> {
        Box::pin(async {
            Err(KernelBehaviorError {
                detail: "actor received an internal resume without pending work".into(),
                diagnostic: None,
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

    /// Apply an immutable child exit on the actor queue; external cleanup belongs
    /// to the child and its retained lifecycle owner.
    fn child_exited(&mut self, notice: ChildExitNotice);
}

pub struct LocalActor<B>(PhantomData<fn() -> B>);

/// The caller's existing work owner fences custody before actor startup.
pub(crate) trait WorkerStartupAdmission: Send + Sync {
    fn reserve(&self, actor: ActorRef) -> Result<(), String>;
    fn admit(&self, actor: LocalActorRef) -> Result<(), String>;
}

pub struct LocalActorArguments<B> {
    pub(crate) startup_refusal: Option<StartupFailureCause>,
    pub(crate) spawn_ownership: SpawnOwnership,
    pub behavior: B,
    pub(crate) startup_admission: Option<Arc<dyn WorkerStartupAdmission>>,
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
    behavior: BehaviorSlot<B>,
    pending_tasks: HashMap<u64, PendingActorTask>,
    next_task_generation: u64,
    pending_child_exits: Vec<ChildExitNotice>,
    terminal: RetainedActorExit,
    deferred_mailbox: VecDeque<KernelMessage>,
    mailbox_drain_scheduled: bool,
}

impl<B> Drop for LocalActorState<B> {
    fn drop(&mut self) {
        self.mailbox_admission.close();
        let detail = "actor stopped while its owned task was still running; execution and cleanup are unconfirmed";
        let mut unconfirmed = false;
        for (_, pending) in self.pending_tasks.drain() {
            if let Some(control) = pending.control() {
                control.request_cancellation();
            }
            unconfirmed = true;
            match pending {
                PendingActorTask::Workbench(pending) => {
                    if let Some(control) = pending.control.as_ref() {
                        control.mark_unconfirmed();
                    }
                    settle_pending_workbench(
                        pending,
                        Err(KernelInvocationFailure::Failed {
                            receipts: Vec::new(),
                            actor: self.context.identity,
                            detail: detail.into(),
                            diagnostic: None,
                        }),
                    );
                }
                PendingActorTask::Tool(pending) => {
                    pending.control.mark_unconfirmed();
                    settle_pending_tool(
                        pending,
                        Err(KernelInvocationFailure::Failed {
                            receipts: Vec::new(),
                            actor: self.context.identity,
                            detail: detail.into(),
                            diagnostic: None,
                        }),
                    );
                }
                PendingActorTask::Kernel(_) => {}
            }
        }
        if unconfirmed {
            retain_unconfirmed_exit(&self.terminal, self.context.identity, detail);
        } else if self.terminal.get().is_none() {
            retain_unconfirmed_exit(
                &self.terminal,
                self.context.identity,
                "actor execution ended before publishing its terminal; execution and cleanup are unconfirmed",
            );
        }
        for control in self.mailbox_admission.hosted_cell().take_all_and_clear() {
            control.request_cancellation();
            control.mark_unconfirmed();
            control.settle(Err(KernelInvocationFailure::ActorExited(
                self.context.identity,
            )));
        }
        self.mailbox_admission
            .hosted_cell()
            .finalization_owner_lost();
    }
}

/// The behavior has exactly one owner: the actor or its active serial task.
struct BehaviorSlot<B>(Option<B>);

impl<B> std::ops::Deref for BehaviorSlot<B> {
    type Target = B;

    fn deref(&self) -> &B {
        self.0
            .as_ref()
            .expect("behavior is owned by a pending actor task")
    }
}

impl<B> std::ops::DerefMut for BehaviorSlot<B> {
    fn deref_mut(&mut self) -> &mut B {
        self.0
            .as_mut()
            .expect("behavior is owned by a pending actor task")
    }
}

struct PendingWorkbench {
    step: crate::WorkbenchStepKey,
    reply: ractor::RpcReplyPort<crate::KernelWorkbenchReply>,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
    execution: Option<tidepool_runtime::session::WorkbenchExecutionId>,
    serial: bool,
    publication: bool,
    hosted_cell: crate::kernel::HostedCellSlot,
}

struct PendingTool {
    step: crate::WorkbenchStepKey,
    reply: ractor::RpcReplyPort<crate::KernelInvocationReply>,
    control: Arc<crate::WorkbenchExecutionControl>,
    hosted_cell: crate::kernel::HostedCellSlot,
}

enum PendingActorTask {
    Workbench(PendingWorkbench),
    Tool(PendingTool),
    Kernel(PendingKernel),
}

impl PendingActorTask {
    fn step(&self) -> &crate::WorkbenchStepKey {
        match self {
            Self::Workbench(pending) => &pending.step,
            Self::Tool(pending) => &pending.step,
            Self::Kernel(pending) => &pending.step,
        }
    }

    fn generation(&self) -> u64 {
        match self {
            Self::Workbench(pending) => pending.step.execution_generation(),
            Self::Tool(pending) => pending.step.execution_generation(),
            Self::Kernel(pending) => pending.step.execution_generation(),
        }
    }

    fn workbench(&self) -> Option<&PendingWorkbench> {
        match self {
            Self::Workbench(pending) => Some(pending),
            Self::Tool(_) | Self::Kernel(_) => None,
        }
    }

    fn control(&self) -> Option<&Arc<crate::WorkbenchExecutionControl>> {
        match self {
            Self::Workbench(pending) => pending.control.as_ref(),
            Self::Tool(pending) => Some(&pending.control),
            Self::Kernel(_) => None,
        }
    }

    fn matches_workbench_boundary(
        &self,
        actor: ActorRef,
        boundary: &tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> bool {
        match boundary {
            tidepool_runtime::session::WorkbenchForkBoundary::Hosted(_) => {
                let Some(key) = self
                    .control()
                    .and_then(|control| control.invocation.as_ref())
                else {
                    return false;
                };
                key.matches_boundary(boundary)
                    && self.step().request_execution()
                        == Some(&crate::resident_tools::execution_id(actor, key))
            }
            tidepool_runtime::session::WorkbenchForkBoundary::Execution {
                actor_id,
                incarnation,
                execution_id,
            } => {
                *actor_id == actor.id.0
                    && *incarnation == actor.incarnation.0
                    && self.step().request_execution() == Some(execution_id)
            }
            tidepool_runtime::session::WorkbenchForkBoundary::Route { .. } => false,
        }
    }
}

struct PendingKernel {
    step: crate::WorkbenchStepKey,
    operation: KernelTaskOperation,
}

#[derive(Debug, Clone, Copy)]
enum KernelTaskOperation {
    ReleaseFork,
    Resume,
}

async fn fail_kernel_operation<B: KernelBehavior>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    operation: KernelTaskOperation,
    error: KernelBehaviorError,
) {
    match operation {
        KernelTaskOperation::ReleaseFork => fail_actor_with_error(myself, state, error).await,
        KernelTaskOperation::Resume => fail_handler_with_error(myself, state, error).await,
    }
}

#[cfg(test)]
type WorkbenchTaskOutcome<B> = ActorTaskOutcome<B, WorkbenchResponse>;

enum ActorTaskOutcome<B, T> {
    Rejoined {
        behavior: B,
        completion: OwnedActorCompletion<B, T>,
    },
    Owned(OwnedActorCompletion<B, T>),
    Lost(String),
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

enum MessageDisposition {
    Consumed,
    Dispatch(KernelMessage),
}

async fn handle_parked_message<B: KernelBehavior>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    message: KernelMessage,
) -> Result<(), ActorProcessingErr> {
    match message {
        KernelMessage::ActorStepCompleted { step, outcome } => {
            complete_actor_task(myself, state, step, outcome).await;
            Ok(())
        }
        KernelMessage::DrainMailbox => {
            state.mailbox_drain_scheduled = false;
            let ready = state.deferred_mailbox.iter().position(|message| {
                can_apply_independent_settlement(state, message)
                    || matches!(message, KernelMessage::Workbench { invocation, .. }
                        if can_admit_deferred_workbench(state, &invocation.request))
            });
            if let Some(index) = ready {
                let message = state
                    .deferred_mailbox
                    .remove(index)
                    .expect("selected deferred work");
                match message {
                    KernelMessage::Workbench {
                        invocation,
                        control,
                        reply,
                    } => {
                        start_workbench(myself, state, invocation, control, reply);
                    }
                    settlement => apply_hosted_settlement(state, settlement).await,
                }
                schedule_deferred_mailbox(myself, state)?;
            }
            Ok(())
        }
        settlement if can_apply_independent_settlement(state, &settlement) => {
            apply_hosted_settlement(state, settlement).await;
            schedule_deferred_mailbox(myself, state)?;
            Ok(())
        }
        KernelMessage::SealHostedWork { reply } => {
            if !matches!(state.hosted_admission, HostedAdmission::Closing)
                && !state
                    .deferred_mailbox
                    .iter()
                    .any(|message| matches!(message, KernelMessage::Shutdown { .. }))
            {
                state.hosted_admission = HostedAdmission::Sealed;
                reply
                    .send(crate::HostedWorkSeal {
                        actor: state.context.identity,
                    })
                    .ok();
            }
            Ok(())
        }
        KernelMessage::Shutdown { terminal, reply } => {
            for pending in state.pending_tasks.values() {
                match pending {
                    PendingActorTask::Workbench(pending) => {
                        if let Some(control) = &pending.control {
                            control.request_cancellation();
                        }
                    }
                    PendingActorTask::Tool(pending) => {
                        pending.control.request_cancellation();
                    }
                    PendingActorTask::Kernel(_) => {}
                }
            }
            state.mailbox_admission.close();
            state
                .deferred_mailbox
                .push_back(KernelMessage::Shutdown { terminal, reply });
            Ok(())
        }
        KernelMessage::ReconcileWorkbenchCancellation {
            execution,
            invocation,
            reply,
        } => {
            if pending_cancellation(state, &execution, invocation.as_ref()) {
                reply
                    .send(crate::WorkbenchCancellationOutcome::Unconfirmed {
                        execution: execution.clone(),
                    })
                    .ok();
            } else {
                state
                    .deferred_mailbox
                    .push_back(KernelMessage::ReconcileWorkbenchCancellation {
                        execution,
                        invocation,
                        reply,
                    });
            }
            Ok(())
        }
        KernelMessage::ReconcileWorkbenchBoundary { boundary, reply } => {
            if state.pending_tasks.values().any(|pending| {
                pending.matches_workbench_boundary(state.context.identity, &boundary)
            }) {
                reply
                    .send(crate::WorkbenchBoundaryReconciliation::Pending)
                    .ok();
            } else {
                state
                    .deferred_mailbox
                    .push_back(KernelMessage::ReconcileWorkbenchBoundary { boundary, reply });
            }
            Ok(())
        }
        KernelMessage::Workbench {
            invocation,
            control,
            reply,
        } if can_admit_workbench_request(state, &invocation.request) => {
            if !matches!(state.hosted_admission, HostedAdmission::Open) {
                let rejection = Err(KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor: state.context.identity,
                    detail: "hosted work admission is sealed".into(),
                    diagnostic: None,
                });
                if let Some(control) = control {
                    control.settle_not_admitted(rejection.clone());
                    state.mailbox_admission.hosted_cell().complete(&control);
                }
                reply.send(rejection).ok();
            } else {
                start_workbench(myself, state, invocation, control, reply);
            }
            Ok(())
        }
        other => {
            state.deferred_mailbox.push_back(other);
            Ok(())
        }
    }
}
fn admit_idle_message<B: KernelBehavior>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    message: KernelMessage,
) -> Result<MessageDisposition, ActorProcessingErr> {
    let message = match message {
        message @ KernelMessage::Shutdown { .. } if state.behavior.replacement_staged() => {
            state.deferred_mailbox.push_back(message);
            return Ok(MessageDisposition::Consumed);
        }
        message @ KernelMessage::RouteReady { .. }
            if state.replacement.is_some() || state.behavior.replacement_staged() =>
        {
            state.deferred_mailbox.push_back(message);
            return Ok(MessageDisposition::Consumed);
        }
        message @ (KernelMessage::Cast { .. }
        | KernelMessage::Call { .. }
        | KernelMessage::Source(_))
            if state.replacement.is_some()
                || !state.deferred_mailbox.is_empty()
                || !state.behavior.accepts_mailbox() =>
        {
            state.deferred_mailbox.push_back(message);
            schedule_deferred_mailbox(myself, state)?;
            return Ok(MessageDisposition::Consumed);
        }
        message @ KernelMessage::Workbench { .. }
            if !state.deferred_mailbox.is_empty()
                && matches!(&message, KernelMessage::Workbench { invocation, .. }
                    if state.behavior.serializes_workbench_publication(&invocation.request)) =>
        {
            state.deferred_mailbox.push_back(message);
            schedule_deferred_mailbox(myself, state)?;
            return Ok(MessageDisposition::Consumed);
        }
        KernelMessage::DrainMailbox => {
            state.mailbox_drain_scheduled = false;
            if state.replacement.is_some() {
                return Ok(MessageDisposition::Consumed);
            }
            let message = if state.behavior.accepts_mailbox() {
                state.deferred_mailbox.pop_front()
            } else if !state.behavior.replacement_staged() {
                state
                    .deferred_mailbox
                    .iter()
                    .position(|message| deferred_control(message).is_some())
                    .and_then(|index| state.deferred_mailbox.remove(index))
            } else {
                None
            };
            let Some(message) = message else {
                return Ok(MessageDisposition::Consumed);
            };
            message
        }
        message => message,
    };
    Ok(MessageDisposition::Dispatch(message))
}
impl<B> Actor for LocalActor<B>
where
    B: KernelBehavior,
{
    type Msg = KernelMessage;
    // Ractor carries startup state through its spawn and processing futures.
    // Keep the behavior and pending-task custody in one stable allocation.
    type State = Box<LocalActorState<B>>;
    type Arguments = LocalActorArguments<B>;

    async fn pre_start(
        &self,
        myself: RactorRef<Self::Msg>,
        arguments: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let identity = arguments.identity;
        let context = std::sync::Arc::new(KernelContext {
            identity,
            terminal: arguments.terminal.clone(),
            spawn_ownership: arguments.spawn_ownership,
            myself,
            children: std::sync::Arc::new(parking_lot::Mutex::new(HashMap::new())),
            resources: Arc::new(Mutex::new(HashMap::new())),
            directory: arguments.directory,
            child_admission_closed: std::sync::Arc::new(tokio::sync::RwLock::new(false)),
            forgotten_children: std::sync::Arc::new(parking_lot::Mutex::new(
                crate::CleanupComponentOutcome::Confirmed,
            )),
        });
        let startup_admission = arguments.startup_admission;
        let startup_refusal = arguments.startup_refusal;
        let mut state = Box::new(LocalActorState {
            replacement: None,
            drain: DrainState::Open,
            mailbox_admission: arguments.mailbox_admission,
            context,
            behavior: BehaviorSlot(Some(arguments.behavior)),
            pending_tasks: HashMap::new(),
            next_task_generation: 0,
            pending_child_exits: Vec::new(),
            terminal: arguments.terminal,
            deferred_mailbox: VecDeque::new(),
            mailbox_drain_scheduled: false,
            hosted_admission: HostedAdmission::Open,
        });
        if let Some(cause) = startup_refusal {
            finish_actor(
                &state.context.myself.clone(),
                &mut state,
                Disposition::Stop(ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: cause.to_string(),
                    diagnostic: None,
                }),
            )
            .await;
            return Err(Box::new(StartupRefusal {
                cause,
                cleanup: state.terminal.cleanup(),
            }));
        }
        let child = LocalActorRef::with_identity_admission(
            state.context.myself.clone(),
            state.terminal.clone(),
            state.context.identity,
            state.mailbox_admission.clone(),
        );
        if let Err(detail) = state.context.directory.insert(
            child.clone(),
            std::sync::Arc::downgrade(&state.context),
            state.context.spawn_ownership,
        ) {
            finish_actor(
                &state.context.myself.clone(),
                &mut state,
                Disposition::Stop(ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: detail.into(),
                    diagnostic: None,
                }),
            )
            .await;
            return Err(Box::new(StartupRefusal {
                cause: StartupFailureCause::MembershipClosed,
                cleanup: state.terminal.cleanup(),
            }));
        }
        if let Some(admission) = startup_admission {
            if let Err(detail) = admission.admit(child) {
                finish_actor(
                    &state.context.myself.clone(),
                    &mut state,
                    Disposition::Stop(ActorTerminal {
                        kind: crate::ActorExitKind::Cancelled,
                        summary: detail.clone(),
                        diagnostic: None,
                    }),
                )
                .await;
                return Err(Box::new(StartupRefusal {
                    cause: StartupFailureCause::WorkerAdmission(detail),
                    cleanup: state.terminal.cleanup(),
                }));
            }
        }
        if !state.behavior.replacement_staged() {
            if let Some(terminal) = state.context.requested_shutdown() {
                finish_actor(
                    &state.context.myself.clone(),
                    &mut state,
                    Disposition::Stop(terminal),
                )
                .await;
                return Ok(state);
            }
        }
        match state.behavior.start(&state.context).await {
            Ok(KernelStep::Continue(())) => {}
            Ok(KernelStep::ContinueLater(())) => {
                state.context.myself.send_message(KernelMessage::Resume {
                    kind: crate::kernel::KernelResume::ContinueProgram,
                })?;
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
                let terminal = ActorTerminal::failed(
                    format!("actor startup failed: {error}"),
                    error.diagnostic.clone(),
                );
                finish_actor(
                    &state.context.myself.clone(),
                    &mut state,
                    Disposition::Stop(terminal),
                )
                .await;
                return Err(Box::new(StartupRefusal {
                    cause: StartupFailureCause::Behavior(error),
                    cleanup: state.terminal.cleanup(),
                }));
            }
        }
        if !state.behavior.replacement_staged() {
            if let Some(terminal) = state.context.requested_shutdown() {
                finish_actor(
                    &state.context.myself.clone(),
                    &mut state,
                    Disposition::Stop(terminal),
                )
                .await;
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
    // Every `reply.send(..)` below is a oneshot back to an RPC-style caller
    // (Call, Tool, Workbench, Drain, Shutdown, ...). A dropped receiver means
    // the caller already gave up (timed out, was cancelled, or its own actor
    // died) before this message finished processing; the state mutation this
    // arm performs already happened, so losing the reply loses only the
    // caller's *notification* of it, never the effect itself. `.ok()` is
    // therefore best-effort throughout this handler.
    async fn handle(
        &self,
        myself: RactorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        if !state.pending_tasks.is_empty() {
            handle_parked_message(&myself, state, message).await?;
            return Ok(());
        }
        let message = match admit_idle_message(&myself, state, message)? {
            MessageDisposition::Consumed => return Ok(()),
            MessageDisposition::Dispatch(message) => message,
        };
        match message {
            KernelMessage::AbortReplacement { reply } => {
                if !state.behavior.replacement_staged() {
                    reply
                        .send(Err(KernelBehaviorError {
                            detail: "actor is not a prepared replacement".into(),
                            diagnostic: None,
                        }))
                        .ok();
                    return Ok(());
                }
                let terminal = finish_actor(
                    &myself,
                    state,
                    Disposition::Stop(ActorTerminal {
                        kind: ActorExitKind::Cancelled,
                        summary: "replacement preparation aborted".into(),
                        diagnostic: None,
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
                        diagnostic: None,
                    })
                };
                reply.send(result).ok();
            }
            KernelMessage::Replace { definition, reply } => {
                if state.replacement.is_some() {
                    let failure = match state.behavior.discard_replacement(*definition).await {
                        Ok(()) => crate::KernelInvocationFailure::Rejected {
                            receipts: Vec::new(),
                            actor: state.context.identity,
                            detail: "actor replacement is already in progress".into(),
                            diagnostic: None,
                        },
                        Err(error) => crate::KernelInvocationFailure::Failed {
                            receipts: Vec::new(),
                            actor: state.context.identity,
                            detail: format!(
                                "actor replacement is already in progress; rejected recipe cleanup unconfirmed: {error}"
                            ),
                            diagnostic: error.diagnostic,
                        },
                    };
                    reply.send(Err(failure)).ok();
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
                            reply
                                .send(Err(crate::KernelInvocationFailure::Rejected {
                                    receipts: Vec::new(),
                                    actor: state.context.identity,
                                    detail: error.detail,
                                    diagnostic: error.diagnostic,
                                }))
                                .ok();
                        }
                    }
                }
            }
            KernelMessage::ReplacementFence => {
                state.mailbox_admission.wait_transactions().await;
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
                        reply
                            .send(Err(crate::KernelInvocationFailure::Failed {
                                receipts: Vec::new(),
                                actor: state.context.identity,
                                detail: error.detail,
                                diagnostic: error.diagnostic,
                            }))
                            .ok();
                    }
                    state.replacement = Some(pending);
                    return Ok(());
                }
                transfer_supervised_children(&state.context, &successor_context);
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
                    reply.send(terminal.clone()).ok();
                }
                if let Some(reply) = pending.reply {
                    reply.send(Ok(pending.successor)).ok();
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
                    Ok(step) => {
                        if let Some(terminal) = state.context.requested_shutdown() {
                            finish_actor(&myself, state, Disposition::Stop(terminal)).await;
                        } else {
                            finish_after_step(&myself, state, step).await;
                        }
                    }
                    Err(error) => fail_actor_with_error(&myself, state, error).await,
                }
            }
            KernelMessage::Drain { reply } => {
                if state.replacement.is_some() {
                    reply
                        .send(Err(KernelBehaviorError {
                            detail: "actor replacement is in progress".into(),
                            diagnostic: None,
                        }))
                        .ok();
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
                            diagnostic: None,
                        })?;
                    state.behavior.close_sources();
                    state.drain = DrainState::Fencing;
                    Ok(())
                });
                reply.send(result).ok();
            }
            KernelMessage::DrainFence => {
                state.drain = DrainState::Draining;
            }
            KernelMessage::Source(delivery) => {
                match state.behavior.source(&state.context, delivery).await {
                    Ok(step) => finish_after_step(&myself, state, step).await,
                    Err(error) => {
                        let detail = format!("source handler failed: {error}");
                        fail_handler_with_error(&myself, state, error.context(detail)).await
                    }
                }
            }
            KernelMessage::SealHostedWork { reply } => {
                if matches!(state.hosted_admission, HostedAdmission::Closing) {
                    drop(reply);
                    return Ok(());
                }
                state.hosted_admission = HostedAdmission::Sealed;
                reply
                    .send(crate::HostedWorkSeal {
                        actor: state.context.identity,
                    })
                    .ok();
            }
            KernelMessage::Cast { sender, request } => {
                match state.behavior.cast(&state.context, sender, request).await {
                    Ok(step) => finish_after_step(&myself, state, step).await,
                    Err(error) => {
                        let detail = format!("actor cast failed: {error}");
                        fail_handler_with_error(&myself, state, error.context(detail)).await
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
                    reply.send(Err(error)).ok();
                }
                Ok(ancestry) => match state
                    .behavior
                    .call(&state.context, caller, ancestry, request)
                    .await
                {
                    Ok(step) => {
                        settle_step(&myself, state, step, |value| {
                            reply.send(Ok(value)).ok();
                        })
                        .await;
                    }
                    Err(error) => {
                        let detail = error.to_string();
                        reply
                            .send(Err(KernelCallFailure::Handler {
                                actor: state.context.identity,
                                detail: detail.clone(),
                                diagnostic: error.diagnostic.clone(),
                            }))
                            .ok();
                        fail_handler_with_error(
                            &myself,
                            state,
                            error.context(format!("actor call failed: {detail}")),
                        )
                        .await;
                    }
                },
            },
            KernelMessage::Tool { invocation, reply } => {
                if !matches!(state.hosted_admission, HostedAdmission::Open) {
                    reply
                        .send(Err(KernelInvocationFailure::Rejected {
                            receipts: Vec::new(),
                            actor: state.context.identity,
                            detail: "hosted work admission is sealed".into(),
                            diagnostic: None,
                        }))
                        .ok();
                } else {
                    start_tool(&myself, state, invocation, None, reply);
                    return Ok(());
                }
            }
            KernelMessage::ToolWithHostedCheckpoint {
                invocation,
                capture,
                reply,
            } => {
                if !matches!(state.hosted_admission, HostedAdmission::Open) {
                    reply
                        .send(Err(KernelInvocationFailure::Rejected {
                            receipts: Vec::new(),
                            actor: state.context.identity,
                            detail: "hosted work admission is sealed".into(),
                            diagnostic: None,
                        }))
                        .ok();
                } else {
                    start_tool(&myself, state, invocation, Some(capture), reply);
                    return Ok(());
                }
            }
            settlement @ (KernelMessage::ReconcileWorkbenchBoundary { .. }
            | KernelMessage::ToolCompleted { .. }
            | KernelMessage::ToolAborted { .. }) => {
                apply_hosted_settlement(state, settlement).await;
            }
            KernelMessage::ReleaseFork { release } => {
                if matches!(state.hosted_admission, HostedAdmission::Closing) {
                    return Ok(());
                }
                if release.child() != state.context.identity() {
                    tracing::warn!(actor = %state.context.identity(), release = ?release, "foreign child release refused");
                    return Ok(());
                }
                start_kernel_task(
                    &myself,
                    state,
                    KernelTaskOperation::ReleaseFork,
                    |behavior, context| behavior.dispatch_release_fork(context, release),
                )
                .await;
                return Ok(());
            }
            KernelMessage::Workbench {
                invocation,
                control,
                reply,
            } => {
                if !matches!(state.hosted_admission, HostedAdmission::Open) {
                    let rejection = Err(KernelInvocationFailure::Rejected {
                        receipts: Vec::new(),
                        actor: state.context.identity,
                        detail: "hosted work admission is sealed".into(),
                        diagnostic: None,
                    });
                    if let Some(control) = control {
                        control.settle_not_admitted(rejection.clone());
                        state.mailbox_admission.hosted_cell().complete(&control);
                    }
                    reply.send(rejection).ok();
                } else {
                    start_workbench(&myself, state, invocation, control, reply);
                    return Ok(());
                }
            }
            KernelMessage::ActorStepCompleted { step, .. } => {
                tracing::warn!(actor = %state.context.identity, ?step, "stale actor completion ignored");
            }
            settlement @ KernelMessage::ReconcileWorkbenchCancellation { .. } => {
                apply_hosted_settlement(state, settlement).await;
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
            KernelMessage::Resume { kind } => {
                start_kernel_task(
                    &myself,
                    state,
                    KernelTaskOperation::Resume,
                    |behavior, context| behavior.dispatch_resume(context, kind),
                )
                .await;
                return Ok(());
            }
            KernelMessage::ExternalApplicationFailed { failure, reply } => {
                let detail = format!("native actor application failed: {}", failure.detail);
                let disposition = state
                    .behavior
                    .external_application_failed(&state.context, failure)
                    .await;
                reply.send(disposition).ok();
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
                reply.send(terminal).ok();
            }
        }
        maybe_drain(&myself, state).await;
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
                return Ok(());
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
        let notice = ChildExitNotice {
            owner: state.context.identity,
            child,
            terminal,
        };
        if !state.pending_tasks.is_empty() {
            state.pending_child_exits.push(notice);
        } else {
            state.behavior.child_exited(notice);
        }
        Ok(())
    }
}

fn start_workbench<B: KernelBehavior>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    invocation: crate::ActorWorkbenchInvocation,
    control: Option<Arc<crate::WorkbenchExecutionControl>>,
    reply: ractor::RpcReplyPort<crate::KernelWorkbenchReply>,
) {
    assert!(
        state.pending_tasks.is_empty() || can_admit_deferred_workbench(state, &invocation.request),
        "workbench admission requires an independent owned-task lane"
    );
    let control = Some(control.unwrap_or_else(crate::WorkbenchExecutionControl::untracked));
    let Some(generation) = state.next_task_generation.checked_add(1) else {
        let failure = Err(KernelInvocationFailure::Failed {
            receipts: Vec::new(),
            actor: state.context.identity,
            detail: "actor step generation exhausted".into(),
            diagnostic: None,
        });
        if let Some(control) = control {
            control.settle_not_admitted(failure.clone());
            state.mailbox_admission.hosted_cell().complete(&control);
        }
        reply.send(failure).ok();
        return;
    };
    state.next_task_generation = generation;
    let execution = invocation.request.execution_id().cloned();
    let step = crate::WorkbenchStepKey::new(state.context.identity, generation, execution.clone());
    let concurrent = !state.pending_tasks.is_empty();
    let mut pending = PendingActorTask::Workbench(PendingWorkbench {
        step,
        reply,
        control: control.clone(),
        execution,
        serial: false,
        publication: state
            .behavior
            .serializes_workbench_publication(&invocation.request),
        hosted_cell: Arc::clone(state.mailbox_admission.hosted_cell()),
    });
    let dispatch = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state
            .behavior
            .dispatch_workbench(&state.context, invocation, control)
    })) {
        Ok(dispatch) => dispatch,
        Err(_) => {
            fail_unconfirmed_task(
                myself,
                state,
                pending,
                "workbench dispatch panicked; execution outcome is unconfirmed".into(),
            );
            return;
        }
    };
    let task = match dispatch {
        WorkbenchDispatch::Owned(task) => task,
        WorkbenchDispatch::Sequential {
            invocation,
            control,
        } if concurrent => {
            if let Some(control) = pending
                .workbench()
                .and_then(|pending| pending.control.as_ref())
            {
                pending
                    .workbench()
                    .expect("workbench task")
                    .hosted_cell
                    .complete(control);
            }
            state.deferred_mailbox.push_front(KernelMessage::Workbench {
                invocation,
                control,
                reply: match pending {
                    PendingActorTask::Workbench(pending) => pending.reply,
                    PendingActorTask::Tool(_) => unreachable!("workbench admission"),
                    PendingActorTask::Kernel(_) => unreachable!("workbench admission"),
                },
            });
            return;
        }
        WorkbenchDispatch::Sequential {
            invocation,
            control,
        } => OwnedActorTask::serial(move |mut behavior: B, context| {
            Box::pin(async move {
                let result = behavior.workbench(&context, invocation, control).await;
                (behavior, OwnedActorCompletion::new(move |_| result))
            })
        }),
    };
    if let Some(control) = pending
        .workbench()
        .and_then(|pending| pending.control.as_ref())
    {
        pending
            .workbench()
            .expect("workbench task")
            .hosted_cell
            .claim(control);
    }
    if let PendingActorTask::Workbench(workbench) = &mut pending {
        workbench.serial = task.is_serial();
    }
    let generation = pending.generation();
    let previous = state.pending_tasks.insert(generation, pending);
    assert!(previous.is_none(), "actor execution generation is unique");
    spawn_actor_task(myself, state, generation, task);
}

fn has_independent_workbench_tasks<B: KernelBehavior>(state: &LocalActorState<B>) -> bool {
    !state.pending_tasks.is_empty()
        && state.behavior.0.is_some()
        && state.behavior.allows_independent_workbench_admission()
        && state.replacement.is_none()
        && !state.behavior.replacement_staged()
        && state.pending_tasks.values().all(|pending| {
            matches!(pending, PendingActorTask::Workbench(workbench) if !workbench.serial)
        })
}

fn can_admit_independent_workbench<B: KernelBehavior>(state: &LocalActorState<B>) -> bool {
    has_independent_workbench_tasks(state)
        && state.deferred_mailbox.iter().all(|message| {
            matches!(
                deferred_control(message),
                Some(DeferredControl::HostedSettlement)
            ) || matches!(message, KernelMessage::Workbench { invocation, .. }
                    if state.behavior.serializes_workbench_publication(&invocation.request))
        })
        && matches!(state.hosted_admission, HostedAdmission::Open)
}

fn pending_cancellation<B: KernelBehavior>(
    state: &LocalActorState<B>,
    execution: &tidepool_runtime::session::WorkbenchExecutionId,
    invocation: Option<&exomonad_tool::ToolInvocationContext>,
) -> bool {
    let call = invocation
        .cloned()
        .map(crate::resident_tools::WorkbenchCallKey::from);
    state.pending_tasks.values().any(|pending| match pending {
        PendingActorTask::Workbench(pending) => {
            pending.execution.as_ref() == Some(execution)
                && pending
                    .control
                    .as_ref()
                    .and_then(|control| control.invocation.as_ref())
                    == call.as_ref()
        }
        PendingActorTask::Tool(pending) => {
            pending.step.request_execution() == Some(execution)
                && call
                    .as_ref()
                    .is_some_and(|call| pending.control.invocation.as_ref() == Some(call))
        }
        PendingActorTask::Kernel(_) => false,
    })
}

fn can_apply_independent_settlement<B: KernelBehavior>(
    state: &LocalActorState<B>,
    message: &KernelMessage,
) -> bool {
    if !has_independent_workbench_tasks(state) {
        return false;
    }
    match message {
        KernelMessage::ToolCompleted { boundary, .. }
        | KernelMessage::ToolAborted { boundary, .. }
        | KernelMessage::ReconcileWorkbenchBoundary { boundary, .. } => {
            !matches!(
                boundary,
                tidepool_runtime::session::WorkbenchForkBoundary::Route { .. }
            ) && !state
                .pending_tasks
                .values()
                .any(|pending| pending.matches_workbench_boundary(state.context.identity, boundary))
        }
        KernelMessage::ReconcileWorkbenchCancellation {
            execution,
            invocation,
            ..
        } => !pending_cancellation(state, execution, invocation.as_ref()),
        _ => false,
    }
}

/// Apply one exact hosted boundary on its serialized actor turn. Admission
/// beside parked cells proves that this boundary owns no pending execution;
/// stateful tools, kernel continuations and mailbox protocols stay exclusive.
async fn apply_hosted_settlement<B: KernelBehavior>(
    state: &mut LocalActorState<B>,
    message: KernelMessage,
) {
    match message {
        KernelMessage::ReconcileWorkbenchBoundary { boundary, reply } => {
            let outcome = state
                .behavior
                .reconcile_workbench_boundary(&state.context, boundary)
                .await
                .unwrap_or(crate::WorkbenchBoundaryReconciliation::Pending);
            reply.send(outcome).ok();
        }
        KernelMessage::ReconcileWorkbenchCancellation {
            execution,
            invocation,
            reply,
        } => {
            let outcome = state
                .behavior
                .reconcile_workbench_cancellation(execution, invocation);
            reply.send(outcome).ok();
        }
        KernelMessage::ToolCompleted { boundary, reply } => {
            if matches!(state.hosted_admission, HostedAdmission::Closing) {
                reply
                    .send(Err(KernelInvocationFailure::Rejected {
                        receipts: Vec::new(),
                        actor: state.context.identity,
                        detail: "hosted completion boundary is closed".into(),
                        diagnostic: None,
                    }))
                    .ok();
                return;
            }
            let result = state
                .behavior
                .tool_completed(&state.context, boundary)
                .await
                .map(|()| serde_json::Value::Null)
                .map_err(|error| KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor: state.context.identity,
                    detail: error.detail,
                    diagnostic: error.diagnostic,
                });
            reply.send(result).ok();
        }
        KernelMessage::ToolAborted { boundary, reply } => {
            let result = state
                .behavior
                .tool_aborted(&state.context, boundary)
                .await
                .map(|()| serde_json::Value::Null)
                .map_err(|error| KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor: state.context.identity,
                    detail: error.detail,
                    diagnostic: error.diagnostic,
                });
            reply.send(result).ok();
        }
        _ => unreachable!("exact hosted settlement"),
    }
}

fn can_admit_deferred_workbench<B: KernelBehavior>(
    state: &LocalActorState<B>,
    request: &tidepool_runtime::session::WorkbenchRequest,
) -> bool {
    can_admit_independent_workbench(state)
        && (!state.behavior.serializes_workbench_publication(request)
            || !state.pending_tasks.values().any(|pending| {
                matches!(pending, PendingActorTask::Workbench(workbench) if workbench.publication)
            }))
}

fn can_admit_workbench_request<B: KernelBehavior>(
    state: &LocalActorState<B>,
    request: &tidepool_runtime::session::WorkbenchRequest,
) -> bool {
    can_admit_deferred_workbench(state, request)
        && (!state.behavior.serializes_workbench_publication(request)
            || state.deferred_mailbox.is_empty())
}

fn start_tool<B: KernelBehavior>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    invocation: exomonad_tool::ToolInvocation,
    hosted_checkpoint_capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
    reply: ractor::RpcReplyPort<crate::KernelInvocationReply>,
) {
    assert!(
        state.pending_tasks.is_empty(),
        "tool admission is exclusive"
    );
    let control = crate::WorkbenchExecutionControl::from_invocation(invocation.context.clone());
    state.mailbox_admission.hosted_cell().claim(&control);
    let Some(generation) = state.next_task_generation.checked_add(1) else {
        settle_pending_tool(
            PendingTool {
                step: crate::WorkbenchStepKey::new(state.context.identity, u64::MAX, None),
                reply,
                control,
                hosted_cell: Arc::clone(state.mailbox_admission.hosted_cell()),
            },
            Err(KernelInvocationFailure::Failed {
                receipts: Vec::new(),
                actor: state.context.identity,
                detail: "actor step generation exhausted".into(),
                diagnostic: None,
            }),
        );
        return;
    };
    state.next_task_generation = generation;
    let execution = control.execution_id(state.context.identity);
    let pending = PendingActorTask::Tool(PendingTool {
        step: crate::WorkbenchStepKey::new(state.context.identity, generation, Some(execution)),
        reply,
        control: Arc::clone(&control),
        hosted_cell: Arc::clone(state.mailbox_admission.hosted_cell()),
    });
    let task = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.behavior.dispatch_tool(
            &state.context,
            invocation,
            hosted_checkpoint_capture,
            control,
        )
    })) {
        Ok(task) => task,
        Err(_) => {
            fail_unconfirmed_task(
                myself,
                state,
                pending,
                "tool dispatch panicked; invocation outcome is unconfirmed".into(),
            );
            return;
        }
    };
    let key = pending.generation();
    state.pending_tasks.insert(key, pending);
    spawn_actor_task(myself, state, generation, task);
}

async fn start_kernel_task<B: KernelBehavior>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    operation: KernelTaskOperation,
    dispatch: impl FnOnce(&mut B, &KernelContext) -> Result<OwnedActorTask<B, ()>, KernelBehaviorError>,
) {
    let Some(generation) = state.next_task_generation.checked_add(1) else {
        fail_actor(myself, state, "actor step generation exhausted".into()).await;
        return;
    };
    state.next_task_generation = generation;
    assert!(
        state.pending_tasks.is_empty(),
        "kernel admission is exclusive"
    );
    let pending = PendingActorTask::Kernel(PendingKernel {
        step: crate::WorkbenchStepKey::new(state.context.identity, generation, None),
        operation,
    });
    let key = pending.generation();
    state.pending_tasks.insert(key, pending);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dispatch(&mut state.behavior, &state.context)
    })) {
        Ok(Ok(task)) => spawn_actor_task(myself, state, generation, task),
        Ok(Err(error)) => {
            state.pending_tasks.remove(&generation);
            fail_kernel_operation(myself, state, operation, {
                let detail = format!("{operation:?} dispatch failed: {error}");
                error.context(detail)
            })
            .await;
        }
        Err(_) => {
            let pending = state
                .pending_tasks
                .remove(&generation)
                .expect("admitted kernel step");
            fail_unconfirmed_task(
                myself,
                state,
                pending,
                format!("{operation:?} dispatch panicked; cleanup is unconfirmed"),
            );
        }
    }
}

fn spawn_actor_task<B: KernelBehavior, T: Send + 'static>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    generation: u64,
    task: OwnedActorTask<B, T>,
) {
    let serial = task.is_serial();
    if let Some(PendingActorTask::Workbench(pending)) = state.pending_tasks.get_mut(&generation) {
        pending.serial = serial;
    }
    let step = state
        .pending_tasks
        .get(&generation)
        .expect("admitted task")
        .step()
        .clone();
    tracing::info!(target: "exomonad_actor::workbench_phase", ?step, phase = "task_dispatched", "actor phase");
    let myself = myself.clone();
    let worker = match task.into_execution() {
        ActorTaskExecution::Owned(run) => tokio::spawn(
            async move {
                match std::panic::AssertUnwindSafe(run).catch_unwind().await {
                    Ok(completion) => ActorTaskOutcome::Owned(completion),
                    Err(_) => ActorTaskOutcome::Lost("execution-owned actor task panicked".into()),
                }
            }
            .instrument(tracing::Span::current()),
        ),
        ActorTaskExecution::Serial(run) => {
            let context = Arc::clone(&state.context);
            let behavior = state
                .behavior
                .0
                .take()
                .expect("one serial actor continuation");
            tokio::spawn(
                async move {
                    match std::panic::AssertUnwindSafe(run(behavior, context))
                        .catch_unwind()
                        .await
                    {
                        Ok((behavior, completion)) => ActorTaskOutcome::Rejoined {
                            behavior,
                            completion,
                        },
                        Err(_) => {
                            ActorTaskOutcome::Lost("serial actor continuation panicked".into())
                        }
                    }
                }
                .instrument(tracing::Span::current()),
            )
        }
    };
    tokio::spawn(async move {
        let outcome: Box<dyn std::any::Any + Send> = match worker.await {
            Ok(outcome) => Box::new(outcome),
            Err(error) => Box::new(ActorTaskOutcome::<B, T>::Lost(error.to_string())),
        };
        if myself
            .send_message(KernelMessage::ActorStepCompleted {
                step: step.clone(),
                outcome,
            })
            .is_err()
        {
            tracing::warn!(
                ?step,
                "actor owner stopped before task completion was delivered"
            );
        }
    });
}

enum TaskApplyFailure {
    Invocation(KernelInvocationFailure),
    Unconfirmed(String),
}

/// Called only after the shared full-step fence. A transferred behavior must
/// not be restored by a stale completion or an outcome of a different kind.
async fn apply_actor_outcome<B: KernelBehavior, T: 'static>(
    state: &mut LocalActorState<B>,
    outcome: Box<dyn std::any::Any + Send>,
) -> Result<ActorAdvance<B, T>, TaskApplyFailure> {
    let outcome = outcome.downcast::<ActorTaskOutcome<B, T>>().map_err(|_| {
        TaskApplyFailure::Unconfirmed(
            "actor step returned a different behavior or output type".into(),
        )
    })?;
    let completion = match *outcome {
        ActorTaskOutcome::Rejoined {
            behavior,
            completion,
        } => {
            state.behavior.0 = Some(behavior);
            completion
        }
        ActorTaskOutcome::Owned(completion) => completion,
        ActorTaskOutcome::Lost(detail) => return Err(TaskApplyFailure::Unconfirmed(detail)),
    };
    match std::panic::AssertUnwindSafe(completion.finish(&mut state.behavior, &state.context))
        .catch_unwind()
        .await
    {
        Ok(result) => result.map_err(TaskApplyFailure::Invocation),
        Err(_) => Err(TaskApplyFailure::Unconfirmed(
            "actor completion panicked".into(),
        )),
    }
}

fn park_actor_task<B: KernelBehavior, T: Send + 'static>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    mut pending: PendingActorTask,
    task: OwnedActorTask<B, T>,
) {
    if task.is_serial() && !state.pending_tasks.is_empty() {
        drop(task);
        fail_unconfirmed_task(
            myself,
            state,
            pending,
            "serial actor continuation parked while independent workbench tasks remain active"
                .into(),
        );
        return;
    }
    let Some(generation) = state.next_task_generation.checked_add(1) else {
        drop(task);
        fail_unconfirmed_task(
            myself,
            state,
            pending,
            "actor step generation exhausted".into(),
        );
        return;
    };
    state.next_task_generation = generation;
    let next_step = pending.step().next_step(generation);
    match &mut pending {
        PendingActorTask::Workbench(pending) => pending.step = next_step,
        PendingActorTask::Tool(pending) => pending.step = next_step,
        PendingActorTask::Kernel(pending) => pending.step = next_step,
    }
    let execution_generation = pending.generation();
    state.pending_tasks.insert(execution_generation, pending);
    spawn_actor_task(myself, state, execution_generation, task);
}

async fn complete_actor_task<B: KernelBehavior>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    step: crate::WorkbenchStepKey,
    outcome: Box<dyn std::any::Any + Send>,
) {
    let execution_generation = step.execution_generation();
    if state
        .pending_tasks
        .get(&execution_generation)
        .is_none_or(|pending| pending.step() != &step)
    {
        tracing::warn!(actor = %state.context.identity, ?step, "stale actor completion ignored");
        return;
    }
    let pending = state
        .pending_tasks
        .remove(&execution_generation)
        .expect("matching pending actor task");
    tracing::info!(target: "exomonad_actor::workbench_phase", ?step, phase = "task_completion_admitted", "actor phase");
    match pending {
        PendingActorTask::Workbench(pending) => {
            match apply_actor_outcome::<B, WorkbenchResponse>(state, outcome).await {
                Ok(ActorAdvance::Park(task)) => {
                    park_actor_task(myself, state, PendingActorTask::Workbench(pending), task);
                    return;
                }
                Ok(ActorAdvance::Complete(step)) => {
                    settle_step(myself, state, step, |output| {
                        settle_pending_workbench(pending, Ok(output))
                    })
                    .await;
                }
                Err(TaskApplyFailure::Invocation(error)) => {
                    let retire = matches!(
                        &error,
                        KernelInvocationFailure::TerminalTransferFailed { .. }
                    );
                    let failure = KernelBehaviorError::with_diagnostic(
                        error.to_string(),
                        error.failure_diagnostic().cloned(),
                    );
                    settle_pending_workbench(pending, Err(error));
                    if retire {
                        fail_actor_with_error(myself, state, failure).await;
                        return;
                    }
                }
                Err(TaskApplyFailure::Unconfirmed(detail)) => {
                    fail_unconfirmed_task(
                        myself,
                        state,
                        PendingActorTask::Workbench(pending),
                        detail,
                    );
                    return;
                }
            }
        }
        PendingActorTask::Tool(pending) => {
            match apply_actor_outcome::<B, serde_json::Value>(state, outcome).await {
                Ok(ActorAdvance::Park(task)) => {
                    park_actor_task(myself, state, PendingActorTask::Tool(pending), task);
                    return;
                }
                Ok(ActorAdvance::Complete(step)) => {
                    settle_step(myself, state, step, |output| {
                        settle_pending_tool(pending, Ok(output))
                    })
                    .await;
                }
                Err(TaskApplyFailure::Invocation(error)) => {
                    let retire = matches!(
                        &error,
                        KernelInvocationFailure::TerminalTransferFailed { .. }
                    );
                    let failure = KernelBehaviorError::with_diagnostic(
                        error.to_string(),
                        error.failure_diagnostic().cloned(),
                    );
                    settle_pending_tool(pending, Err(error));
                    if retire {
                        fail_actor_with_error(myself, state, failure).await;
                        return;
                    }
                }
                Err(TaskApplyFailure::Unconfirmed(detail)) => {
                    fail_unconfirmed_task(myself, state, PendingActorTask::Tool(pending), detail);
                    return;
                }
            }
        }
        PendingActorTask::Kernel(pending) => {
            match apply_actor_outcome::<B, ()>(state, outcome).await {
                Ok(ActorAdvance::Park(task)) => {
                    park_actor_task(myself, state, PendingActorTask::Kernel(pending), task);
                    return;
                }
                Ok(ActorAdvance::Complete(step)) => finish_after_step(myself, state, step).await,
                Err(TaskApplyFailure::Invocation(error)) => {
                    let detail = format!("{:?} failed: {error}", pending.operation);
                    fail_kernel_operation(
                        myself,
                        state,
                        pending.operation,
                        error.into_behavior_error().context(detail),
                    )
                    .await;
                }
                Err(TaskApplyFailure::Unconfirmed(detail)) => {
                    fail_unconfirmed_task(myself, state, PendingActorTask::Kernel(pending), detail);
                    return;
                }
            }
        }
    }
    if state.pending_tasks.is_empty() && state.terminal.get().is_none() {
        for notice in std::mem::take(&mut state.pending_child_exits) {
            state.behavior.child_exited(notice);
        }
    }
    if state.pending_tasks.is_empty() {
        maybe_drain(myself, state).await;
    }
    if state.terminal.get().is_none() {
        if let Err(error) = schedule_deferred_mailbox(myself, state) {
            fail_actor(
                myself,
                state,
                format!("could not resume actor mailbox: {error}"),
            )
            .await;
        }
    }
}

async fn maybe_drain<B: KernelBehavior>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
) {
    if state.pending_tasks.is_empty()
        && state.replacement.is_none()
        && matches!(state.drain, DrainState::Draining)
        && state.deferred_mailbox.is_empty()
        && state.behavior.accepts_mailbox()
        && state.terminal.get().is_none()
    {
        match state.behavior.drain(&state.context).await {
            Ok(step) => finish_after_step(myself, state, step).await,
            Err(error) => {
                let detail = format!("actor drain failed: {error}");
                fail_handler_with_error(myself, state, error.context(detail)).await;
            }
        }
    }
}

fn fail_unconfirmed_task<B: KernelBehavior>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    pending: PendingActorTask,
    detail: String,
) {
    let failure = KernelInvocationFailure::Failed {
        receipts: Vec::new(),
        actor: state.context.identity,
        detail: detail.clone(),
        diagnostic: None,
    };
    match pending {
        PendingActorTask::Workbench(pending) => {
            if let Some(control) = pending.control.as_ref() {
                control.mark_unconfirmed();
            }
            settle_pending_workbench(pending, Err(failure));
        }
        PendingActorTask::Tool(pending) => {
            pending.control.mark_unconfirmed();
            settle_pending_tool(pending, Err(failure));
        }
        PendingActorTask::Kernel(_) => {}
    }
    state.hosted_admission = HostedAdmission::Closing;
    state.mailbox_admission.close();
    retain_unconfirmed_exit(&state.terminal, state.context.identity, &detail);
    myself.stop(Some(detail));
}

fn settle_pending_workbench(pending: PendingWorkbench, mut reply: crate::KernelWorkbenchReply) {
    if let Some(control) = pending.control {
        reply = if matches!(&reply, Err(KernelInvocationFailure::Rejected { .. })) {
            control.settle_not_admitted(reply)
        } else {
            control.settle_reply(reply)
        };
        pending.hosted_cell.complete(&control);
    }
    let delivered = pending.reply.send(reply).is_ok();
    tracing::info!(target: "exomonad_actor::workbench_phase", step = ?pending.step, delivered, phase = "reply_settled", "workbench phase");
}

pub(crate) fn tool_control_reply(
    reply: &crate::KernelInvocationReply,
) -> crate::KernelWorkbenchReply {
    match reply {
        Ok(value) => Ok(WorkbenchResponse {
            status: tidepool_runtime::session::WorkbenchRunStatus::Committed,
            summary: None,
            items: vec![tidepool_runtime::session::WorkbenchItemReceipt {
                index: 0,
                kind: None,
                span: None,
                source_items: Vec::new(),
                status: tidepool_runtime::session::WorkbenchItemStatus::Committed,
                output: value.to_string(),
                value: None,
                diagnostics: Vec::new(),
                failure_layer: None,
                warnings: Vec::new(),
                installed_bindings: Vec::new(),
                operations: Vec::new(),
                terminal_transfer: None,
            }],
            next_index: 1,
            total: 1,
            publication: None,
        }),
        Err(error) => Err(error.clone()),
    }
}

fn settle_pending_tool(pending: PendingTool, mut reply: crate::KernelInvocationReply) {
    if let Err(error) = pending.control.settle_reply(tool_control_reply(&reply)) {
        reply = Err(error);
    }
    pending.hosted_cell.complete(&pending.control);
    let delivered = pending.reply.send(reply).is_ok();
    tracing::info!(
        target: "exomonad_actor::workbench_phase",
        step = ?pending.step,
        delivered,
        phase = "tool_reply_settled",
        "actor phase"
    );
}

fn retain_unconfirmed_exit(terminal: &RetainedActorExit, actor: ActorRef, detail: &str) {
    let unconfirmed = crate::CleanupComponentOutcome::Unconfirmed(detail.into());
    terminal.retain_cleanup(crate::ResidentCleanupOutcome {
        actor,
        hook: unconfirmed.clone(),
        realm: unconfirmed.clone(),
        children: unconfirmed,
    });
    if terminal.get().is_none() {
        publish_exit(
            terminal,
            &failed_terminal(detail.into()),
            ExitAuthority::OwnerLostExecution,
        );
    }
}

/// Hosted invocations and settlement do not require a resident receiver.
/// Their queued forms retain the same admission boundary.
enum DeferredControl {
    Workbench,
    HostedInvocation,
    HostedSettlement,
    RouteSettlement,
    Shutdown,
}

fn deferred_control(message: &KernelMessage) -> Option<DeferredControl> {
    match message {
        KernelMessage::Workbench { .. } => Some(DeferredControl::Workbench),
        KernelMessage::Tool { .. } | KernelMessage::ToolWithHostedCheckpoint { .. } => {
            Some(DeferredControl::HostedInvocation)
        }
        KernelMessage::ToolCompleted {
            boundary: tidepool_runtime::session::WorkbenchForkBoundary::Route { .. },
            ..
        }
        | KernelMessage::ReconcileWorkbenchBoundary {
            boundary: tidepool_runtime::session::WorkbenchForkBoundary::Route { .. },
            ..
        } => Some(DeferredControl::RouteSettlement),
        KernelMessage::ToolCompleted { .. }
        | KernelMessage::ToolAborted { .. }
        | KernelMessage::ReconcileWorkbenchBoundary { .. }
        | KernelMessage::ReconcileWorkbenchCancellation { .. } => {
            Some(DeferredControl::HostedSettlement)
        }
        KernelMessage::Shutdown { .. } => Some(DeferredControl::Shutdown),
        _ => None,
    }
}

fn schedule_deferred_mailbox<B>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
) -> Result<(), ActorProcessingErr>
where
    B: KernelBehavior,
{
    let runnable_control = !state.behavior.replacement_staged()
        && state
            .deferred_mailbox
            .iter()
            .any(|message| deferred_control(message).is_some());
    let runnable = if state.pending_tasks.is_empty() {
        state.behavior.accepts_mailbox() || runnable_control
    } else {
        state.deferred_mailbox.iter().any(|message| {
            can_apply_independent_settlement(state, message)
                || matches!(message, KernelMessage::Workbench { invocation, .. }
                    if can_admit_deferred_workbench(state, &invocation.request))
        })
    };
    if state.replacement.is_none()
        && state.terminal.get().is_none()
        && runnable
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
    spawn_local_actor_in_directory_with_factory(name, incarnation, directory, |_| behavior).await
}

/// Build admission state from the original identity issued by this directory.
pub(crate) async fn spawn_local_actor_in_directory_with_factory<B, F>(
    name: Option<String>,
    incarnation: crate::Incarnation,
    directory: LocalActorDirectory,
    build: F,
) -> Result<(LocalActorRef, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr>
where
    B: KernelBehavior,
    F: FnOnce(ActorRef) -> B,
{
    let identity = directory
        .reserve(incarnation)
        .map_err(|error| ractor::SpawnErr::StartupFailed(std::io::Error::other(error).into()))?;
    spawn_reserved_local_actor(name, build(identity), identity, directory).await
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
            spawn_ownership: SpawnOwnership::Independent,
            startup_refusal: None,
            behavior,
            startup_admission: None,
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
async fn fail_handler_with_error<B>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    error: KernelBehaviorError,
) where
    B: KernelBehavior,
{
    if matches!(state.drain, DrainState::Open)
        && state
            .behavior
            .pause_failed_handler(&state.context, &error.detail)
    {
        return;
    }
    fail_actor_with_error(myself, state, error).await;
}

async fn fail_actor<B>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    detail: String,
) where
    B: KernelBehavior,
{
    fail_actor_with_error(myself, state, KernelBehaviorError::new(detail)).await;
}

async fn fail_actor_with_error<B>(
    myself: &RactorRef<KernelMessage>,
    state: &mut LocalActorState<B>,
    error: KernelBehaviorError,
) where
    B: KernelBehavior,
{
    finish_actor(
        myself,
        state,
        Disposition::Stop(ActorTerminal::failed(error.detail, error.diagnostic)),
    )
    .await;
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
        if let Err(error) = myself.send_message(KernelMessage::Resume {
            kind: crate::kernel::KernelResume::ContinueProgram,
        }) {
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
    state.mailbox_admission.wait_transactions().await;
    // Wait for admitted startup to register, then permanently reject creation,
    // including through cloned contexts and shutdown hooks.
    *state.context.child_admission_closed.write().await = true;
    match disposition {
        Disposition::Stop(requested) => {
            let requested = requested.bound_diagnostic();
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
                diagnostic: None,
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
    ActorTerminal::failed(summary, None)
}

pub(crate) fn combine_cleanup(
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

/// The accepted replacement owns the same children and resource scopes. This
/// changes live Ractor supervision, never the spawn fact used at startup.
fn transfer_supervised_children(predecessor: &KernelContext, successor: &KernelContext) {
    {
        let mut children = predecessor.children.lock();
        let mut inherited = successor.children.lock();
        for (id, child) in children.drain() {
            child.address().get_cell().link(successor.myself.get_cell());
            inherited.insert(id, child);
        }
    }
    {
        let mut resources = predecessor.resources.lock();
        for (id, child) in resources.drain() {
            child.cell.link(successor.myself.get_cell());
            successor.resources.lock().insert(id, child);
        }
    }
    let mut inherited = successor.forgotten_children.lock();
    *inherited = combine_cleanup(
        inherited.clone(),
        std::mem::replace(
            &mut *predecessor.forgotten_children.lock(),
            crate::CleanupComponentOutcome::Confirmed,
        ),
    );
}

async fn shutdown_children(
    context: &KernelContext,
    owner_exit: ActorExitKind,
    timeout: Duration,
) -> crate::CleanupComponentOutcome {
    let (children, mut outcome) = {
        let children = context.children.lock();
        (
            children.values().cloned().collect::<Vec<_>>(),
            context.forgotten_children.lock().clone(),
        )
    };
    let requested = ActorTerminal {
        kind: match owner_exit {
            ActorExitKind::Failed => ActorExitKind::Failed,
            ActorExitKind::Completed | ActorExitKind::Cancelled => ActorExitKind::Cancelled,
        },
        summary: "owner actor stopped".into(),
        diagnostic: None,
    };
    let batch = crate::kernel::RetirementBatch::issue(
        children
            .into_iter()
            .map(|child| (child, requested.clone()))
            .collect(),
    );
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
    // Include resource cleanup evidence retained after the child fence.
    outcome = combine_cleanup(outcome, context.forgotten_children.lock().clone());
    let mut shutdowns = FuturesUnordered::new();
    for (child, requested) in batch.into_actors() {
        shutdowns.push(async move {
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
mod reply_settlement_history;

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use crate::ActorLifecycle;
    use std::sync::Arc;

    use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};
    use parking_lot::Mutex;
    use tidepool_repr::SessionId;
    use tidepool_runtime::session::{WorkbenchRequest, WorkbenchResponse, WorkbenchRunStatus};
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
        start_observations: Option<Arc<Mutex<Vec<(KernelContext, Option<ActorRef>)>>>>,
        cleanup_peers: Option<Arc<Mutex<Vec<LocalActorRef>>>>,
        replacement_staged: bool,
        startup_gate: Option<(Arc<Notify>, Arc<Notify>)>,
        calls: Arc<Mutex<Vec<&'static str>>>,
        mailbox_calls: Arc<Mutex<Vec<SessionId>>>,
        release_first: Arc<Notify>,
        fail_cast: bool,
        resume_failure: Option<KernelBehaviorError>,
        terminal_callbacks: Arc<Mutex<Vec<ActorTerminal>>>,
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
        workbench_gate: Option<(Arc<Notify>, Arc<Notify>)>,
        workbench_panics: bool,
        owned_workbench: bool,
        independent_workbench_admission: bool,
        administrative_workbench_gate: Option<(Arc<Notify>, Arc<Notify>)>,
        owned_cleanup: Option<Arc<Notify>>,
        owned_finish_fails: bool,
        owned_successor_gate: Option<(Arc<Notify>, Arc<Notify>)>,
        owned_successor_source: Option<String>,
        owned_successor_panics: bool,
        dispatch_panics: bool,
        kernel_probe: Option<KernelProbe>,
    }

    #[derive(Clone)]
    struct KernelProbe {
        first: (Arc<Notify>, Arc<Notify>),
        next: (Arc<Notify>, Arc<Notify>),
        abandoned: Arc<Notify>,
        panic_successor: bool,
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
        fn allows_independent_workbench_admission(&self) -> bool {
            self.independent_workbench_admission
        }

        fn serializes_workbench_publication(&self, request: &WorkbenchRequest) -> bool {
            request.cell_source() == Some("reload")
        }

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
                        diagnostic: None,
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
                        diagnostic: None,
                    },
                })
            })
        }
        fn accepts_mailbox(&self) -> bool {
            self.mailbox_ready
        }

        fn start(
            &mut self,
            context: &KernelContext,
        ) -> BoxFuture<'_, Result<KernelStep<()>, KernelBehaviorError>> {
            if let Some(observations) = &self.start_observations {
                observations
                    .lock()
                    .push((context.clone(), context.supervisor_identity()));
            }
            let gate = self.startup_gate.clone();
            Box::pin(async move {
                if let Some((entered, release)) = gate {
                    entered.notify_one();
                    release.notified().await;
                }
                Ok(KernelStep::Continue(()))
            })
        }

        fn dispatch_release_fork(
            &mut self,
            context: &KernelContext,
            release: crate::ForkChildRelease,
        ) -> Result<OwnedActorTask<Self, ()>, KernelBehaviorError> {
            let probe = self.kernel_probe.clone().expect("configured kernel probe");
            assert_eq!(release.child(), context.identity());
            let abandoned = Arc::clone(&probe.abandoned);
            Ok(OwnedActorTask::new(Box::pin(async move {
                probe.first.0.notify_one();
                probe.first.1.notified().await;
                OwnedActorCompletion::advance(move |behavior: &mut Self, _| {
                    behavior.calls.lock().push("kernel-first");
                    Ok(ActorAdvance::Park(OwnedActorTask::new(Box::pin(
                        async move {
                            probe.next.0.notify_one();
                            probe.next.1.notified().await;
                            if probe.panic_successor {
                                panic!("kernel successor probe panic");
                            }
                            OwnedActorCompletion::advance(|behavior: &mut Self, context| {
                                behavior.calls.lock().push("kernel-last");
                                Ok(ActorAdvance::Complete(match context.requested_shutdown() {
                                    Some(terminal) => KernelStep::Stop {
                                        output: (),
                                        terminal,
                                    },
                                    None => KernelStep::Continue(()),
                                }))
                            })
                        },
                    ))))
                })
                .with_abandon_guard(ActorAbandonGuard::new(move || abandoned.notify_one()))
            })))
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
                        diagnostic: None,
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
            _hosted_checkpoint_capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
        ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>> {
            Box::pin(async move {
                let name = invocation.name;
                if name == "spawn" {
                    let child = context
                        .spawn_child(None, FailingChild)
                        .await
                        .map_err(|error| KernelInvocationFailure::Failed {
                            receipts: Vec::new(),
                            actor: context.identity(),
                            detail: error.to_string(),
                            diagnostic: None,
                        })?;
                    *self.spawned_child.lock() = Some(child);
                } else if name == "spawn_queued" {
                    let Some(child_behavior) = self.pending_children.pop_front() else {
                        return Err(KernelInvocationFailure::Rejected {
                            receipts: Vec::new(),
                            actor: context.identity(),
                            detail: "no queued child behavior".into(),
                            diagnostic: None,
                        });
                    };
                    let child =
                        context
                            .spawn_child(None, child_behavior)
                            .await
                            .map_err(|error| KernelInvocationFailure::Failed {
                                receipts: Vec::new(),
                                actor: context.identity(),
                                detail: error.to_string(),
                                diagnostic: None,
                            })?;
                    self.spawned_children.lock().push(child);
                } else if name == "finish" {
                    return Ok(KernelStep::Stop {
                        output: serde_json::Value::String(name),
                        terminal: ActorTerminal {
                            kind: ActorExitKind::Completed,
                            summary: "finished through an agent tool".into(),
                            diagnostic: None,
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
            _invocation: crate::ActorWorkbenchInvocation,
            _control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
        ) -> BoxFuture<'_, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>> {
            let gate = self.workbench_gate.clone();
            let panics = self.workbench_panics;
            Box::pin(async move {
                self.calls.lock().push("workbench-start");
                if let Some((entered, release)) = gate {
                    entered.notify_one();
                    release.notified().await;
                }
                if panics {
                    panic!("workbench probe panic");
                }
                self.calls.lock().push("workbench-end");
                Ok(KernelStep::Continue(WorkbenchResponse {
                    status: WorkbenchRunStatus::Committed,
                    summary: None,
                    items: Vec::new(),
                    next_index: 0,
                    total: 0,
                    publication: None,
                }))
            })
        }

        fn dispatch_workbench(
            &mut self,
            context: &KernelContext,
            invocation: crate::ActorWorkbenchInvocation,
            control: Option<Arc<crate::WorkbenchExecutionControl>>,
        ) -> WorkbenchDispatch<Self> {
            if self.dispatch_panics {
                panic!("workbench dispatch probe panic");
            }
            if !self.owned_workbench {
                return WorkbenchDispatch::Sequential {
                    invocation,
                    control,
                };
            }
            let calls = Arc::clone(&self.calls);
            let gate = if self.serializes_workbench_publication(&invocation.request) {
                self.administrative_workbench_gate.clone()
            } else {
                self.workbench_gate.clone()
            };
            let panics = self.workbench_panics;
            let cleanup = self.owned_cleanup.clone();
            let finish_fails = self.owned_finish_fails;
            let successor_gate = if self
                .owned_successor_source
                .as_deref()
                .is_none_or(|source| invocation.request.cell_source() == Some(source))
            {
                self.owned_successor_gate.clone()
            } else {
                None
            };
            let successor_panics = self.owned_successor_panics;
            let actor = context.identity;
            WorkbenchDispatch::Owned(OwnedWorkbenchTask::new(Box::pin(async move {
                let guard =
                    cleanup.map(|cleanup| WorkbenchAbandonGuard::new(move || cleanup.notify_one()));
                calls.lock().push("workbench-start");
                if let Some((entered, release)) = gate {
                    entered.notify_one();
                    release.notified().await;
                }
                if panics {
                    panic!("owned workbench probe panic");
                }
                if let Some((entered, release)) = successor_gate {
                    let completion =
                        OwnedWorkbenchCompletion::advance(move |behavior: &mut Self, _context| {
                            behavior.calls.lock().push("workbench-next-step");
                            Ok(WorkbenchAdvance::Park(OwnedWorkbenchTask::new(Box::pin(
                                async move {
                                    if let Some(control) = &control {
                                        control.arm_sleep();
                                    }
                                    entered.notify_one();
                                    match &control {
                                        Some(control) => tokio::select! {
                                            () = release.notified() => { control.finish_sleep(); }
                                            () = control.wait_for_cancellation() => {
                                                control.acknowledge_cancellation();
                                            }
                                        },
                                        None => release.notified().await,
                                    }
                                    if successor_panics {
                                        panic!("owned successor probe panic");
                                    }
                                    OwnedWorkbenchCompletion::new(move |behavior: &mut Self| {
                                        behavior.calls.lock().push("workbench-end");
                                        Ok(KernelStep::Continue(WorkbenchResponse {
                                            status: WorkbenchRunStatus::Committed,
                                            summary: None,
                                            items: Vec::new(),
                                            next_index: 0,
                                            total: 0,
                                            publication: None,
                                        }))
                                    })
                                },
                            ))))
                        });
                    return match guard {
                        Some(guard) => completion.with_abandon_guard(guard),
                        None => completion,
                    };
                }
                let completion = OwnedWorkbenchCompletion::new(move |behavior: &mut Self| {
                    behavior.calls.lock().push("workbench-end");
                    if finish_fails {
                        return Err(KernelInvocationFailure::Failed {
                            receipts: Vec::new(),
                            actor,
                            detail: "owned completion probe failed".into(),
                            diagnostic: None,
                        });
                    }
                    Ok(KernelStep::Continue(WorkbenchResponse {
                        status: WorkbenchRunStatus::Committed,
                        summary: None,
                        items: Vec::new(),
                        next_index: 0,
                        total: 0,
                        publication: None,
                    }))
                });
                match guard {
                    Some(guard) => completion.with_abandon_guard(guard),
                    None => completion,
                }
            })))
        }

        fn tool_completed<'a>(
            &'a mut self,
            _context: &'a KernelContext,
            _boundary: tidepool_runtime::session::WorkbenchForkBoundary,
        ) -> BoxFuture<'a, Result<(), KernelBehaviorError>> {
            Box::pin(async move {
                self.calls.lock().push("tool-completed");
                Ok(())
            })
        }

        fn resume(
            &mut self,
            _context: &KernelContext,
        ) -> BoxFuture<'_, Result<KernelStep<()>, KernelBehaviorError>> {
            Box::pin(async move {
                self.calls.lock().push("resume-start");
                if let Some(error) = self.resume_failure.take() {
                    return Err(error);
                }
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
            terminal: &ActorTerminal,
        ) -> BoxFuture<'_, Result<(), KernelBehaviorError>> {
            let terminal = terminal.clone();
            Box::pin(async move {
                if let Some(peers) = &self.cleanup_peers {
                    for peer in peers.lock().iter() {
                        assert!(
                            peer.terminal().requested_shutdown().is_some(),
                            "peer cleanup before cancellation fence"
                        );
                    }
                }
                self.terminal_callbacks.lock().push(terminal);
                self.calls.lock().push("shutdown");
                Ok(())
            })
        }

        fn stopped(
            &mut self,
            _context: &KernelContext,
            terminal: &ActorTerminal,
        ) -> BoxFuture<'_, ()> {
            self.terminal_callbacks.lock().push(terminal.clone());
            Box::pin(async {})
        }

        fn child_exited(&mut self, notice: ChildExitNotice) {
            self.child_exits.lock().push(notice.terminal);
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
                    diagnostic: None,
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
            _hosted_checkpoint_capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
        ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>> {
            Box::pin(async move {
                Err(KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor: context.identity(),
                    detail: "child has no tool policy".into(),
                    diagnostic: None,
                })
            })
        }

        fn workbench<'a>(
            &'a mut self,
            context: &'a KernelContext,
            _invocation: crate::ActorWorkbenchInvocation,
            _control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
        ) -> BoxFuture<'a, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>> {
            Box::pin(async move {
                Err(KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor: context.identity(),
                    detail: "child has no workbench".into(),
                    diagnostic: None,
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

        fn child_exited(&mut self, _notice: ChildExitNotice) {}
    }

    struct ProbeFixture {
        behavior: ProbeBehavior,
        calls: Arc<Mutex<Vec<&'static str>>>,
        mailbox_calls: Arc<Mutex<Vec<SessionId>>>,
        release: Arc<Notify>,
        spawned_child: Arc<Mutex<Option<LocalActorRef>>>,
        child_exits: Arc<Mutex<Vec<ActorTerminal>>>,
        terminal_callbacks: Arc<Mutex<Vec<ActorTerminal>>>,
    }

    fn behavior(fail_cast: bool) -> ProbeFixture {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mailbox_calls = Arc::new(Mutex::new(Vec::new()));
        let release = Arc::new(Notify::new());
        let spawned_child = Arc::new(Mutex::new(None));
        let child_exits = Arc::new(Mutex::new(Vec::new()));
        let spawned_children = Arc::new(Mutex::new(Vec::new()));
        let terminal_callbacks = Arc::new(Mutex::new(Vec::new()));
        ProbeFixture {
            behavior: ProbeBehavior {
                start_observations: None,
                cleanup_peers: None,
                replacement_staged: false,
                startup_gate: None,
                calls: Arc::clone(&calls),
                mailbox_calls: Arc::clone(&mailbox_calls),
                release_first: Arc::clone(&release),
                fail_cast,
                resume_failure: None,
                terminal_callbacks: terminal_callbacks.clone(),
                mailbox_ready: true,
                spawned_child: Arc::clone(&spawned_child),
                child_exits: Arc::clone(&child_exits),
                pending_children: VecDeque::new(),
                spawned_children,
                shutdown_override: None,
                workbench_gate: None,
                workbench_panics: false,
                owned_workbench: false,
                independent_workbench_admission: false,
                administrative_workbench_gate: None,
                owned_cleanup: None,
                owned_finish_fails: false,
                owned_successor_gate: None,
                owned_successor_source: None,
                owned_successor_panics: false,
                dispatch_panics: false,
                kernel_probe: None,
            },
            calls,
            mailbox_calls,
            release,
            spawned_child,
            child_exits,
            terminal_callbacks,
        }
    }

    fn send_workbench(actor: &LocalActorRef) -> oneshot::Receiver<crate::KernelWorkbenchReply> {
        send_workbench_request(actor, WorkbenchRequest::from_cell_input("pure ()"), None)
    }

    fn send_workbench_request(
        actor: &LocalActorRef,
        request: WorkbenchRequest,
        control: Option<Arc<crate::WorkbenchExecutionControl>>,
    ) -> oneshot::Receiver<crate::KernelWorkbenchReply> {
        let (reply, receive) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Workbench {
                invocation: crate::ActorWorkbenchInvocation::unbound(request),
                control,
                reply: reply.into(),
            })
            .expect("queue workbench");
        receive
    }

    fn kernel_probe(panic_successor: bool) -> KernelProbe {
        KernelProbe {
            first: (Arc::new(Notify::new()), Arc::new(Notify::new())),
            next: (Arc::new(Notify::new()), Arc::new(Notify::new())),
            abandoned: Arc::new(Notify::new()),
            panic_successor,
        }
    }

    fn send_kernel_release(actor: &LocalActorRef) -> tidepool_runtime::session::PersistentSession {
        let (release, _, machine) =
            crate::resident_actor::child_initialization::scheduler_fixture(actor.identity());
        actor
            .address()
            .send_message(KernelMessage::ReleaseFork { release })
            .expect("release child");
        machine
    }

    #[tokio::test]
    async fn hosted_tools_queued_during_child_initialization_settle_without_receiver() {
        struct UnavailableCapture;
        impl crate::HostedCheckpointCapture for UnavailableCapture {
            fn capture(
                &self,
                _: &str,
                _: &tidepool_runtime::session::WorkbenchForkBoundary,
            ) -> Result<crate::HostedCheckpointAttachment, crate::HostedCheckpointCaptureError>
            {
                Err(crate::HostedCheckpointCaptureError::Unavailable)
            }
        }

        let mut fixture = behavior(false);
        fixture.behavior.mailbox_ready = false;
        let probe = kernel_probe(false);
        fixture.behavior.kernel_probe = Some(probe.clone());
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let _machine = send_kernel_release(&actor);
        probe.first.0.notified().await;
        let (plain_tx, mut plain_rx) = oneshot::channel();
        let (captured_tx, mut captured_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Tool {
                invocation: tool_invocation("plain"),
                reply: plain_tx.into(),
            })
            .unwrap();
        actor
            .address()
            .send_message(KernelMessage::ToolWithHostedCheckpoint {
                invocation: tool_invocation("captured"),
                capture: Arc::new(UnavailableCapture),
                reply: captured_tx.into(),
            })
            .unwrap();
        let workbench_control = crate::WorkbenchExecutionControl::untracked();
        let mut workbench_rx = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("pure ()"),
            Some(Arc::clone(&workbench_control)),
        );
        let (following_tx, mut following_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Tool {
                invocation: tool_invocation("following"),
                reply: following_tx.into(),
            })
            .unwrap();
        // The seal acknowledges all preceding deliveries while initialization
        // remains exclusive. Their admission is rechecked when it finishes.
        actor.seal_hosted_work().await.unwrap();
        assert!(matches!(
            plain_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            captured_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            workbench_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            following_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        probe.first.1.notify_one();
        probe.next.0.notified().await;
        probe.next.1.notify_one();
        for receive in [plain_rx, captured_rx] {
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(2), receive).await.unwrap().unwrap(),
                Err(KernelInvocationFailure::Rejected { detail, .. })
                    if detail == "hosted work admission is sealed"
            ));
        }
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), workbench_rx).await.unwrap().unwrap(),
            Err(KernelInvocationFailure::Rejected { detail, .. })
                if detail == "hosted work admission is sealed"
        ));
        assert!(matches!(
            workbench_control.terminal_reply(),
            Some(Err(KernelInvocationFailure::Rejected { detail, .. }))
                if detail == "hosted work admission is sealed"
        ));
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), following_rx).await.unwrap().unwrap(),
            Err(KernelInvocationFailure::Rejected { detail, .. })
                if detail == "hosted work admission is sealed"
        ));
        assert_eq!(&*fixture.calls.lock(), &["kernel-first", "kernel-last"]);
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "tool initialization queue checked".into(),
                diagnostic: None,
            })
            .await
            .unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn owned_kernel_steps_share_single_admission_and_fence_stale_completion() {
        let mut fixture = behavior(false);
        let probe = kernel_probe(false);
        fixture.behavior.kernel_probe = Some(probe.clone());
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let _machine = send_kernel_release(&actor);
        probe.first.0.notified().await;
        let mut workbench = send_workbench(&actor);
        probe.first.1.notify_one();
        probe.next.0.notified().await;
        assert!(matches!(
            workbench.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        let stale_dropped = Arc::new(Notify::new());
        let dropped = Arc::clone(&stale_dropped);
        let stale = OwnedWorkbenchCompletion::new(|_: &mut ProbeBehavior| {
            panic!("stale wrong-output completion must not apply");
        })
        .with_abandon_guard(ActorAbandonGuard::new(move || dropped.notify_one()));
        actor
            .address()
            .send_message(KernelMessage::ActorStepCompleted {
                step: crate::WorkbenchStepKey::new(actor.identity(), 1, None),
                outcome: Box::new(WorkbenchTaskOutcome::Owned(stale)),
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), stale_dropped.notified())
            .await
            .unwrap();
        assert_eq!(&*fixture.calls.lock(), &["kernel-first"]);
        assert!(matches!(
            workbench.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        probe.next.1.notify_one();
        assert!(workbench.await.unwrap().is_ok());
        assert_eq!(
            &*fixture.calls.lock(),
            &[
                "kernel-first",
                "kernel-last",
                "workbench-start",
                "workbench-end"
            ]
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(10), probe.abandoned.notified())
                .await
                .is_err()
        );
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn owned_kernel_successor_panic_keeps_cleanup_unconfirmed() {
        let mut fixture = behavior(false);
        let probe = kernel_probe(true);
        fixture.behavior.kernel_probe = Some(probe.clone());
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let _machine = send_kernel_release(&actor);
        probe.first.0.notified().await;
        probe.first.1.notify_one();
        probe.next.0.notified().await;
        probe.next.1.notify_one();
        tokio::time::timeout(Duration::from_secs(2), probe.abandoned.notified())
            .await
            .unwrap();
        assert_eq!(actor.terminal().wait().await.kind, ActorExitKind::Failed);
        task.await.unwrap();
        assert!(matches!(
            actor.terminal().cleanup().unwrap().hook,
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
        assert_eq!(&*fixture.calls.lock(), &["kernel-first"]);
    }

    #[tokio::test]
    async fn owned_kernel_shutdown_keeps_queue_fenced_until_original_task_settles() {
        let mut fixture = behavior(false);
        let probe = kernel_probe(false);
        fixture.behavior.kernel_probe = Some(probe.clone());
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let _machine = send_kernel_release(&actor);
        probe.first.0.notified().await;
        probe.first.1.notify_one();
        probe.next.0.notified().await;
        let mut workbench = send_workbench(&actor);
        let stopping = actor.clone();
        let shutdown = tokio::spawn(async move {
            stopping
                .shutdown(ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "stop during child initialization".into(),
                    diagnostic: None,
                })
                .await
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            actor.terminal().wait_requested_shutdown(),
        )
        .await
        .unwrap();
        assert!(!shutdown.is_finished());
        assert!(matches!(
            workbench.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        probe.next.1.notify_one();
        assert_eq!(
            shutdown.await.unwrap().unwrap().kind,
            ActorExitKind::Cancelled
        );
        task.await.unwrap();
        assert!(workbench.await.is_err());
        assert_eq!(
            &*fixture.calls.lock(),
            &["kernel-first", "kernel-last", "shutdown"]
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(10), probe.abandoned.notified())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn workbench_keeps_control_responsive_and_serializes_notebook_calls() {
        let mut fixture = behavior(false);
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        fixture.behavior.workbench_gate = Some((Arc::clone(&entered), Arc::clone(&release)));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let execution = tidepool_runtime::session::WorkbenchExecutionId::from_digest([7; 16]);
        let first = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("pure ()").with_execution_id(execution.clone()),
            Some(crate::WorkbenchExecutionControl::untracked()),
        );
        entered.notified().await;
        let (reconcile_tx, reconcile_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ReconcileWorkbenchCancellation {
                execution: execution.clone(),
                invocation: None,
                reply: reconcile_tx.into(),
            })
            .expect("reconcile active workbench");
        assert!(matches!(
            reconcile_rx.await.expect("reconciliation"),
            crate::WorkbenchCancellationOutcome::Unconfirmed { execution: actual }
                if actual == execution
        ));
        let rejected_control = crate::WorkbenchExecutionControl::untracked();
        let second = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("pure ()"),
            Some(Arc::clone(&rejected_control)),
        );
        actor
            .seal_hosted_work()
            .await
            .expect("seal while notebook runs");
        assert_eq!(&*fixture.calls.lock(), &["workbench-start"]);
        release.notify_one();
        assert!(first.await.expect("first reply").is_ok());
        assert!(matches!(
            second.await.expect("second reply"),
            Err(KernelInvocationFailure::Rejected { .. })
        ));
        assert!(matches!(
            rejected_control.terminal_reply(),
            Some(Err(KernelInvocationFailure::Rejected { .. }))
        ));
        assert_eq!(
            &*fixture.calls.lock(),
            &["workbench-start", "workbench-end"]
        );
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn second_workbench_starts_only_after_first_returns() {
        let mut fixture = behavior(false);
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        fixture.behavior.workbench_gate = Some((Arc::clone(&entered), Arc::clone(&release)));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let first = send_workbench(&actor);
        entered.notified().await;
        let second = send_workbench(&actor);
        tokio::task::yield_now().await;
        assert_eq!(&*fixture.calls.lock(), &["workbench-start"]);
        release.notify_one();
        assert!(first.await.expect("first reply").is_ok());
        entered.notified().await;
        assert_eq!(
            &*fixture.calls.lock(),
            &["workbench-start", "workbench-end", "workbench-start"]
        );
        release.notify_one();
        assert!(second.await.expect("second reply").is_ok());
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn owned_successive_steps_fence_stale_completion_and_retain_terminal_reply() {
        let mut fixture = behavior(false);
        fixture.behavior.owned_workbench = true;
        let first_entered = Arc::new(Notify::new());
        let first_release = Arc::new(Notify::new());
        let next_entered = Arc::new(Notify::new());
        let next_release = Arc::new(Notify::new());
        let cleaned = Arc::new(Notify::new());
        fixture.behavior.workbench_gate =
            Some((Arc::clone(&first_entered), Arc::clone(&first_release)));
        fixture.behavior.owned_successor_gate =
            Some((Arc::clone(&next_entered), Arc::clone(&next_release)));
        fixture.behavior.owned_cleanup = Some(Arc::clone(&cleaned));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let execution = tidepool_runtime::session::WorkbenchExecutionId::from_digest([79; 16]);
        let control = crate::WorkbenchExecutionControl::untracked();
        let mut reply = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("two steps").with_execution_id(execution.clone()),
            Some(Arc::clone(&control)),
        );
        first_entered.notified().await;
        first_release.notify_one();
        next_entered.notified().await;
        assert!(matches!(
            reply.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(control.terminal_reply().is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), cleaned.notified())
                .await
                .is_err()
        );
        let stale_dropped = Arc::new(Notify::new());
        let notify = Arc::clone(&stale_dropped);
        let stale = OwnedWorkbenchCompletion::new(|_: &mut ProbeBehavior| {
            panic!("stale finalizer must never execute")
        })
        .with_abandon_guard(WorkbenchAbandonGuard::new(move || notify.notify_one()));
        actor
            .address()
            .send_message(KernelMessage::ActorStepCompleted {
                step: crate::WorkbenchStepKey::new(actor.identity(), 1, Some(execution.clone())),
                outcome: Box::new(WorkbenchTaskOutcome::Owned(stale)),
            })
            .expect("stale first-step completion");
        tokio::time::timeout(Duration::from_secs(2), stale_dropped.notified())
            .await
            .expect("stale cleanup claim dropped");
        assert!(control.terminal_reply().is_none());
        control.request_cancellation();
        let response = reply
            .await
            .expect("terminal reply")
            .expect("terminal result");
        assert_eq!(response.status, WorkbenchRunStatus::Committed);
        assert!(matches!(
            control.cancellation_outcome(execution, Ok(response)),
            crate::WorkbenchCancellationOutcome::Cancelled { .. }
        ));
        assert_eq!(
            &*fixture.calls.lock(),
            &["workbench-start", "workbench-next-step", "workbench-end"]
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), cleaned.notified())
                .await
                .is_err()
        );
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn owned_successor_panic_abandons_transferred_cleanup_claim() {
        let mut fixture = behavior(false);
        fixture.behavior.owned_workbench = true;
        fixture.behavior.owned_successor_panics = true;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let cleaned = Arc::new(Notify::new());
        fixture.behavior.owned_successor_gate = Some((Arc::clone(&entered), Arc::clone(&release)));
        fixture.behavior.owned_cleanup = Some(Arc::clone(&cleaned));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let reply = send_workbench(&actor);
        entered.notified().await;
        release.notify_one();
        assert!(matches!(
            reply.await.expect("reply"),
            Err(KernelInvocationFailure::Failed { .. })
        ));
        tokio::time::timeout(Duration::from_secs(2), cleaned.notified())
            .await
            .expect("successor drops exact cleanup claim");
        assert_eq!(actor.terminal().wait().await.kind, ActorExitKind::Failed);
        task.await.expect("actor task");
        assert!(matches!(
            actor.terminal().cleanup().expect("cleanup").hook,
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
    }

    #[tokio::test]
    async fn execution_owned_workbench_settles_exact_step_before_next_admission() {
        let mut fixture = behavior(false);
        fixture.behavior.owned_workbench = true;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        fixture.behavior.workbench_gate = Some((Arc::clone(&entered), Arc::clone(&release)));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let execution = tidepool_runtime::session::WorkbenchExecutionId::from_digest([77; 16]);
        let first = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("first").with_execution_id(execution.clone()),
            None,
        );
        entered.notified().await;
        let second = send_workbench(&actor);
        let (cancel_tx, cancel_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ReconcileWorkbenchCancellation {
                invocation: None,
                execution: execution.clone(),
                reply: cancel_tx.into(),
            })
            .expect("reconcile pending execution");
        assert!(matches!(
            cancel_rx.await.expect("cancellation reply"),
            crate::WorkbenchCancellationOutcome::Unconfirmed { execution: found }
                if found == execution
        ));
        assert_eq!(&*fixture.calls.lock(), &["workbench-start"]);
        release.notify_one();
        assert!(first.await.expect("first reply").is_ok());
        entered.notified().await;
        assert_eq!(
            &*fixture.calls.lock(),
            &["workbench-start", "workbench-end", "workbench-start"]
        );
        release.notify_one();
        assert!(second.await.expect("second reply").is_ok());
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");
        assert_eq!(
            &*fixture.calls.lock(),
            &[
                "workbench-start",
                "workbench-end",
                "workbench-start",
                "workbench-end",
                "shutdown",
            ]
        );
    }

    #[tokio::test]
    async fn independent_workbench_admission_fences_parked_execution_by_full_step() {
        let mut fixture = behavior(false);
        fixture.behavior.owned_workbench = true;
        fixture.behavior.independent_workbench_admission = true;
        let parked = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        fixture.behavior.owned_successor_gate = Some((Arc::clone(&parked), Arc::clone(&resume)));
        fixture.behavior.owned_successor_source = Some("A".into());
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");

        let first_execution =
            tidepool_runtime::session::WorkbenchExecutionId::from_digest([81; 16]);
        let mut first = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("A").with_execution_id(first_execution.clone()),
            None,
        );
        parked.notified().await;

        let second_execution =
            tidepool_runtime::session::WorkbenchExecutionId::from_digest([82; 16]);
        let second = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("B").with_execution_id(second_execution),
            None,
        );
        assert!(second.await.expect("second reply").is_ok());
        assert!(matches!(
            first.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        actor
            .address()
            .send_message(KernelMessage::ActorStepCompleted {
                step: crate::WorkbenchStepKey::new(actor.identity(), 1, Some(first_execution)),
                outcome: Box::new(()),
            })
            .expect("queue stale pre-park step");
        tokio::task::yield_now().await;
        assert!(matches!(
            first.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        resume.notify_one();
        assert!(first.await.expect("resumed first reply").is_ok());
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn administrative_workbench_serializes_reload_without_blocking_cells() {
        let mut fixture = behavior(false);
        fixture.behavior.owned_workbench = true;
        fixture.behavior.independent_workbench_admission = true;
        let parked = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        fixture.behavior.owned_successor_gate = Some((parked.clone(), resume.clone()));
        fixture.behavior.owned_successor_source = Some("A".into());
        let reload_started = Arc::new(Notify::new());
        let reload_release = Arc::new(Notify::new());
        fixture.behavior.administrative_workbench_gate =
            Some((reload_started.clone(), reload_release.clone()));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let first = send_workbench_request(&actor, WorkbenchRequest::from_cell_input("A"), None);
        parked.notified().await;
        let reload =
            send_workbench_request(&actor, WorkbenchRequest::from_cell_input("reload"), None);
        reload_started.notified().await;
        let next_reload =
            send_workbench_request(&actor, WorkbenchRequest::from_cell_input("reload"), None);
        let third = send_workbench_request(&actor, WorkbenchRequest::from_cell_input("C"), None);
        assert!(tokio::time::timeout(Duration::from_secs(2), third)
            .await
            .expect("third cell progresses around reloads")
            .expect("third reply")
            .is_ok());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), reload_started.notified())
                .await
                .is_err(),
            "the second reload must not start before the first settles"
        );
        reload_release.notify_one();
        assert!(reload.await.expect("first reload reply").is_ok());
        tokio::time::timeout(Duration::from_secs(2), reload_started.notified())
            .await
            .expect("queued reload starts while A remains parked");
        let fourth = send_workbench_request(&actor, WorkbenchRequest::from_cell_input("D"), None);
        assert!(tokio::time::timeout(Duration::from_secs(2), fourth)
            .await
            .expect("fourth cell progresses")
            .expect("fourth reply")
            .is_ok());
        reload_release.notify_one();
        assert!(next_reload.await.expect("second reload reply").is_ok());
        resume.notify_one();
        assert!(first.await.expect("resumed first reply").is_ok());
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn queued_administrative_workbench_runs_without_a_mailbox_receiver() {
        let mut fixture = behavior(false);
        fixture.behavior.mailbox_ready = false;
        fixture.behavior.owned_workbench = true;
        fixture.behavior.independent_workbench_admission = true;
        let reload_started = Arc::new(Notify::new());
        let reload_release = Arc::new(Notify::new());
        fixture.behavior.administrative_workbench_gate =
            Some((reload_started.clone(), reload_release.clone()));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let first =
            send_workbench_request(&actor, WorkbenchRequest::from_cell_input("reload"), None);
        reload_started.notified().await;
        let second =
            send_workbench_request(&actor, WorkbenchRequest::from_cell_input("reload"), None);
        let control = send_workbench_request(&actor, WorkbenchRequest::from_cell_input("C"), None);
        assert!(tokio::time::timeout(Duration::from_secs(2), control)
            .await
            .expect("cell progresses beside the sole reload")
            .expect("cell reply")
            .is_ok());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), reload_started.notified())
                .await
                .is_err(),
            "queued reload preserves the administrative publication boundary"
        );
        reload_release.notify_one();
        assert!(first.await.expect("first reload settles").is_ok());
        tokio::time::timeout(Duration::from_secs(2), reload_started.notified())
            .await
            .expect("queued reload starts after the last task settles without a mailbox receiver");
        reload_release.notify_one();
        assert!(second.await.expect("second reload settles").is_ok());
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn settled_workbench_acknowledges_while_another_execution_stays_parked() {
        let mut fixture = behavior(false);
        fixture.behavior.owned_workbench = true;
        fixture.behavior.independent_workbench_admission = true;
        let parked = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        fixture.behavior.owned_successor_gate = Some((parked.clone(), resume.clone()));
        fixture.behavior.owned_successor_source = Some("A".into());
        let calls = fixture.calls.clone();
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let invocation = ToolInvocationContext::external(
            "ack-thread".into(),
            "A".into(),
            "A".into(),
            Some("A".into()),
            None,
        );
        let control = crate::WorkbenchExecutionControl::from_invocation(Some(invocation));
        let execution = control.execution_id(actor.identity());
        let first = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("A").with_execution_id(execution.clone()),
            Some(control),
        );
        parked.notified().await;
        let (own_tx, mut own_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ToolCompleted {
                boundary: tidepool_runtime::session::WorkbenchForkBoundary::external(
                    "ack-thread".into(),
                    "A".into(),
                    "A".into(),
                ),
                reply: own_tx.into(),
            })
            .expect("queue A's own completion");
        let (execution_ack_tx, mut execution_ack_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ToolCompleted {
                boundary: tidepool_runtime::session::WorkbenchForkBoundary::Execution {
                    actor_id: actor.identity().id.0,
                    incarnation: actor.identity().incarnation.0,
                    execution_id: execution,
                },
                reply: execution_ack_tx.into(),
            })
            .expect("queue A's exact execution completion");
        let second = send_workbench_request(&actor, WorkbenchRequest::from_cell_input("B"), None);
        assert!(tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .expect("B progresses past A's pending acknowledgement")
            .expect("B reply")
            .is_ok());
        let (ack_tx, ack_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ToolCompleted {
                boundary: tidepool_runtime::session::WorkbenchForkBoundary::external(
                    "ack-thread".into(),
                    "B".into(),
                    "B".into(),
                ),
                reply: ack_tx.into(),
            })
            .expect("acknowledge settled B");
        assert!(tokio::time::timeout(Duration::from_secs(2), ack_rx)
            .await
            .expect("B acknowledgement ignores unrelated A")
            .expect("ack reply")
            .is_ok());
        let third = send_workbench_request(&actor, WorkbenchRequest::from_cell_input("C"), None);
        assert!(tokio::time::timeout(Duration::from_secs(2), third)
            .await
            .expect("C progresses")
            .expect("C reply")
            .is_ok());
        assert!(
            matches!(own_rx.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
            "A's own completion stays fenced"
        );
        assert!(
            matches!(
                execution_ack_rx.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ),
            "A's exact execution completion stays fenced"
        );
        assert_eq!(
            calls
                .lock()
                .iter()
                .filter(|call| **call == "tool-completed")
                .count(),
            1
        );
        let (route_tx, mut route_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ReconcileWorkbenchBoundary {
                boundary: tidepool_runtime::session::WorkbenchForkBoundary::Route {
                    actor_id: actor.identity().id.0,
                    incarnation: actor.identity().incarnation.0,
                    watch_id: 7,
                },
                reply: route_tx.into(),
            })
            .expect("queue exclusive route reconciliation");
        let mut fourth =
            send_workbench_request(&actor, WorkbenchRequest::from_cell_input("D"), None);
        let (blocked_tx, blocked_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ToolCompleted {
                boundary: tidepool_runtime::session::WorkbenchForkBoundary::external(
                    "ack-thread".into(),
                    "B".into(),
                    "B".into(),
                ),
                reply: blocked_tx.into(),
            })
            .expect("observe actor turn after route and D");
        assert!(tokio::time::timeout(Duration::from_secs(2), blocked_rx)
            .await
            .expect("settled B still acknowledges")
            .expect("observer ack reply")
            .is_ok());
        assert!(
            matches!(
                route_rx.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ),
            "route reconciliation stays exclusive while A is parked"
        );
        assert!(
            matches!(fourth.try_recv(), Err(oneshot::error::TryRecvError::Empty)),
            "queued route reconciliation remains an admission barrier"
        );
        resume.notify_one();
        assert!(first.await.expect("A resumes").is_ok());
        assert!(tokio::time::timeout(Duration::from_secs(2), own_rx)
            .await
            .expect("A's deferred acknowledgement drains")
            .expect("A ack reply")
            .is_ok());
        assert!(
            tokio::time::timeout(Duration::from_secs(2), execution_ack_rx)
                .await
                .expect("A's exact execution acknowledgement drains")
                .expect("execution ack reply")
                .is_ok()
        );
        tokio::time::timeout(Duration::from_secs(2), route_rx)
            .await
            .expect("route reconciliation drains after A settles")
            .expect("route reply");
        assert!(tokio::time::timeout(Duration::from_secs(2), fourth)
            .await
            .expect("D resumes after exclusive route reconciliation")
            .expect("D reply")
            .is_ok());
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn legacy_tool_ingress_waits_for_parked_owned_workbench() {
        let mut fixture = behavior(false);
        fixture.behavior.owned_workbench = true;
        fixture.behavior.independent_workbench_admission = true;
        let parked = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        fixture.behavior.owned_successor_gate = Some((Arc::clone(&parked), Arc::clone(&resume)));
        fixture.behavior.owned_successor_source = Some("A".into());
        let calls = Arc::clone(&fixture.calls);
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let first = send_workbench_request(&actor, WorkbenchRequest::from_cell_input("A"), None);
        parked.notified().await;

        let (reply, tool) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Tool {
                invocation: tool_invocation("queued"),
                reply: reply.into(),
            })
            .expect("queue legacy tool call");
        tokio::task::yield_now().await;
        assert!(!calls.lock().contains(&"second"));

        resume.notify_one();
        assert!(first.await.expect("workbench reply").is_ok());
        assert!(tool.await.expect("tool reply").is_ok());
        assert_eq!(
            &*calls.lock(),
            &[
                "workbench-start",
                "workbench-next-step",
                "workbench-end",
                "second",
            ]
        );
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn abandoned_workbench_waiter_still_settles_retained_control() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let control = crate::WorkbenchExecutionControl::untracked();
        let receiver = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("pure ()"),
            Some(Arc::clone(&control)),
        );
        drop(receiver);
        let settled = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(reply) = control.terminal_reply() {
                    break reply;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actor settled retained control");
        assert!(settled.is_ok());
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn abandoned_hosted_caller_keeps_active_cell_visible_until_actor_settles() {
        let mut fixture = behavior(false);
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        fixture.behavior.workbench_gate = Some((Arc::clone(&entered), Arc::clone(&release)));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let client = crate::resident_tools::ResidentToolClient::local(actor.clone());
        let caller = tokio::spawn(async move {
            client
                .dispatch_workbench(
                    WorkbenchRequest::from_cell_input("pure ()"),
                    Some(ToolInvocationContext::external(
                        "thread".into(),
                        "turn".into(),
                        "call".into(),
                        None,
                        None,
                    )),
                )
                .await
        });
        entered.notified().await;
        let control = actor
            .hosted_cell()
            .find(|_| true)
            .expect("accepted control");
        caller.abort();
        let _ = caller.await;
        assert!(actor.hosted_cell_computing());
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if control.terminal_reply().is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actor settled abandoned call");
        assert!(control.terminal_reply().expect("terminal").is_ok());
        assert!(!actor.hosted_cell_computing());
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn actor_exit_settles_transport_control_before_sender_accepts() {
        let (actor, task) = spawn_local_actor(None, behavior(false).behavior)
            .await
            .expect("spawn");
        let control = crate::WorkbenchExecutionControl::untracked();
        actor.hosted_cell().publish_transport(Arc::clone(&control));

        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");

        assert!(matches!(
            control.terminal_reply(),
            Some(Err(KernelInvocationFailure::ActorExited(identity)))
                if identity == actor.identity()
        ));
        actor.hosted_cell().accept(&control);
        assert!(actor.hosted_cell().find(|_| true).is_none());
    }

    #[tokio::test]
    async fn retirement_waits_for_short_host_admission_transaction() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let lease = actor.admit_transaction().unwrap();
        let retiring = actor.shutdown(ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "done".into(),
            diagnostic: None,
        });
        tokio::pin!(retiring);
        // Polling submits shutdown and closes admission before yielding.
        assert!(futures_util::poll!(&mut retiring).is_pending());
        assert!(actor.admit_transaction().is_err());
        assert!(actor.terminal().get().is_none());
        assert!(!fixture.calls.lock().contains(&"shutdown"));
        drop(lease);
        retiring.await.unwrap();
        task.await.unwrap();
        assert!(fixture.calls.lock().contains(&"shutdown"));
        assert!(actor.admit_transaction().is_err());
    }

    #[tokio::test]
    async fn admitted_completion_precedes_shutdown_after_workbench() {
        let mut fixture = behavior(false);
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        fixture.behavior.workbench_gate = Some((Arc::clone(&entered), Arc::clone(&release)));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let workbench = send_workbench(&actor);
        entered.notified().await;
        let (completed_tx, completed_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ToolCompleted {
                boundary: tidepool_runtime::session::WorkbenchForkBoundary::external(
                    "thread".into(),
                    "call".into(),
                    "call".into(),
                ),
                reply: completed_tx.into(),
            })
            .expect("queue completion");
        let terminal = ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "stop".into(),
            diagnostic: None,
        };
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Shutdown {
                terminal: terminal.clone(),
                reply: shutdown_tx.into(),
            })
            .expect("queue shutdown");
        release.notify_one();
        assert!(workbench.await.expect("workbench reply").is_ok());
        assert!(completed_rx.await.expect("completion reply").is_ok());
        assert_eq!(shutdown_rx.await.expect("shutdown reply"), terminal);
        task.await.expect("actor task");
        assert_eq!(
            &*fixture.calls.lock(),
            &[
                "workbench-start",
                "workbench-end",
                "tool-completed",
                "shutdown"
            ]
        );
    }

    async fn assert_settlement_controls_drain_with_parked_mailbox(owned: bool) {
        let mut fixture = behavior(false);
        fixture.behavior.mailbox_ready = false;
        fixture.behavior.owned_workbench = owned;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        fixture.behavior.workbench_gate = Some((Arc::clone(&entered), Arc::clone(&release)));
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let workbench = send_workbench(&actor);
        entered.notified().await;
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
            .unwrap();
        let boundary = tidepool_runtime::session::WorkbenchForkBoundary::external(
            "parked-thread".into(),
            "completed-call".into(),
            "completed-call".into(),
        );
        let (completed_tx, mut completed_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ToolCompleted {
                boundary: boundary.clone(),
                reply: completed_tx.into(),
            })
            .unwrap();
        let (reconcile_tx, reconcile_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ReconcileWorkbenchBoundary {
                boundary,
                reply: reconcile_tx.into(),
            })
            .unwrap();
        let execution = tidepool_runtime::session::WorkbenchExecutionId::from_digest([78; 16]);
        let (cancel_tx, cancel_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ReconcileWorkbenchCancellation {
                invocation: None,
                execution: execution.clone(),
                reply: cancel_tx.into(),
            })
            .unwrap();
        // A subsequent seal acknowledges that the earlier controls entered
        // the pending workbench queue before its task is released.
        let (seal_tx, seal_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::SealHostedWork {
                reply: seal_tx.into(),
            })
            .unwrap();
        seal_rx.await.unwrap();
        assert!(matches!(
            completed_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(&*fixture.calls.lock(), &["workbench-start"]);
        release.notify_one();
        assert!(workbench.await.unwrap().is_ok());
        tokio::time::timeout(Duration::from_secs(2), async {
            assert!(completed_rx.await.unwrap().is_ok());
            assert!(matches!(reconcile_rx.await.unwrap(), crate::WorkbenchBoundaryReconciliation::Pending));
            assert!(matches!(cancel_rx.await.unwrap(), crate::WorkbenchCancellationOutcome::UnknownEvaluation { execution: found } if found == execution));
        }).await.expect("settlement controls must drain without a resident receiver");
        assert!(matches!(
            call_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        assert_eq!(
            &*fixture.calls.lock(),
            &["workbench-start", "workbench-end", "tool-completed"]
        );
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .unwrap();
        task.await.unwrap();
        assert!(call_rx.await.is_err());
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn sequential_workbench_completion_drains_settlement_with_parked_mailbox() {
        assert_settlement_controls_drain_with_parked_mailbox(false).await;
    }

    #[tokio::test]
    async fn owned_workbench_completion_drains_settlement_with_parked_mailbox() {
        assert_settlement_controls_drain_with_parked_mailbox(true).await;
    }

    #[tokio::test]
    async fn workbench_shutdown_waits_for_completion_and_ignores_stale_step() {
        let mut fixture = behavior(false);
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        fixture.behavior.workbench_gate = Some((Arc::clone(&entered), Arc::clone(&release)));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let reply = send_workbench(&actor);
        entered.notified().await;
        actor
            .address()
            .send_message(KernelMessage::ActorStepCompleted {
                step: crate::WorkbenchStepKey::new(actor.identity(), u64::MAX, None),
                outcome: Box::new(()),
            })
            .expect("stale step");
        let terminal = ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "stop".into(),
            diagnostic: None,
        };
        let actor_for_shutdown = actor.clone();
        let requested = terminal.clone();
        let shutdown = tokio::spawn(async move { actor_for_shutdown.shutdown(requested).await });
        while actor.terminal().requested_shutdown().is_none() {
            tokio::task::yield_now().await;
        }
        assert!(actor.terminal().get().is_none());
        assert_eq!(&*fixture.calls.lock(), &["workbench-start"]);
        release.notify_one();
        assert!(reply.await.expect("workbench reply").is_ok());
        assert_eq!(
            shutdown.await.expect("shutdown task").expect("shutdown"),
            terminal
        );
        task.await.expect("actor task");
        assert_eq!(
            &*fixture.calls.lock(),
            &["workbench-start", "workbench-end", "shutdown"]
        );
    }

    #[tokio::test]
    async fn stale_step_and_cancellation_do_not_touch_a_new_workbench_execution() {
        let mut fixture = behavior(false);
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        fixture.behavior.workbench_gate = Some((Arc::clone(&entered), Arc::clone(&release)));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let first_execution =
            tidepool_runtime::session::WorkbenchExecutionId::from_digest([31; 16]);
        let first = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("first").with_execution_id(first_execution.clone()),
            None,
        );
        entered.notified().await;
        release.notify_one();
        assert!(first.await.expect("first reply").is_ok());

        let second_execution =
            tidepool_runtime::session::WorkbenchExecutionId::from_digest([32; 16]);
        let mut second = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("second").with_execution_id(second_execution.clone()),
            None,
        );
        entered.notified().await;

        actor
            .address()
            .send_message(KernelMessage::ActorStepCompleted {
                step: crate::WorkbenchStepKey::new(
                    actor.identity(),
                    1,
                    Some(first_execution.clone()),
                ),
                outcome: Box::new(()),
            })
            .expect("queue old step completion");
        let (cancel_tx, cancel_rx) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ReconcileWorkbenchCancellation {
                invocation: None,
                execution: first_execution.clone(),
                reply: cancel_tx.into(),
            })
            .expect("queue old cancellation reconciliation");

        assert!(matches!(
            second.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        release.notify_one();
        assert!(second.await.expect("second reply").is_ok());
        assert!(matches!(
            cancel_rx.await.expect("old cancellation reply"),
            crate::WorkbenchCancellationOutcome::UnknownEvaluation { execution }
                if execution == first_execution
        ));
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn workbench_panic_settles_waiter_and_fails_actor() {
        let mut fixture = behavior(false);
        fixture.behavior.workbench_panics = true;
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let execution = tidepool_runtime::session::WorkbenchExecutionId::from_digest([8; 16]);
        let control = crate::WorkbenchExecutionControl::untracked();
        let reply = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("panic").with_execution_id(execution.clone()),
            Some(Arc::clone(&control)),
        );
        let reply = reply.await.expect("workbench reply");
        assert!(matches!(
            &reply,
            Err(KernelInvocationFailure::Failed { .. })
        ));
        assert!(matches!(
            control.cancellation_outcome(execution, reply),
            crate::WorkbenchCancellationOutcome::Unconfirmed { .. }
        ));
        assert_eq!(actor.terminal().wait().await.kind, ActorExitKind::Failed);
        task.await.expect("actor task");
        assert!(matches!(
            actor.terminal().cleanup().expect("cleanup evidence").hook,
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
        assert_eq!(&*fixture.calls.lock(), &["workbench-start"]);
    }

    #[tokio::test]
    async fn execution_owned_workbench_panic_retains_unconfirmed_cleanup() {
        let mut fixture = behavior(false);
        fixture.behavior.owned_workbench = true;
        fixture.behavior.workbench_panics = true;
        let cleaned = Arc::new(Notify::new());
        fixture.behavior.owned_cleanup = Some(Arc::clone(&cleaned));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let execution = tidepool_runtime::session::WorkbenchExecutionId::from_digest([9; 16]);
        let control = crate::WorkbenchExecutionControl::untracked();
        let reply = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("panic").with_execution_id(execution.clone()),
            Some(Arc::clone(&control)),
        )
        .await
        .expect("workbench reply");
        assert!(matches!(
            &reply,
            Err(KernelInvocationFailure::Failed { .. })
        ));
        assert!(matches!(
            control.cancellation_outcome(execution, reply),
            crate::WorkbenchCancellationOutcome::Unconfirmed { .. }
        ));
        assert_eq!(actor.terminal().wait().await.kind, ActorExitKind::Failed);
        task.await.expect("actor task");
        tokio::time::timeout(Duration::from_secs(2), cleaned.notified())
            .await
            .expect("panicked worker's execution guard cleaned up");
        assert!(matches!(
            actor.terminal().cleanup().expect("cleanup evidence").hook,
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
        assert_eq!(&*fixture.calls.lock(), &["workbench-start"]);
    }

    #[tokio::test]
    async fn execution_owned_finalizer_failure_releases_cleanup_claim() {
        let mut fixture = behavior(false);
        fixture.behavior.owned_workbench = true;
        fixture.behavior.owned_finish_fails = true;
        let cleaned = Arc::new(Notify::new());
        fixture.behavior.owned_cleanup = Some(Arc::clone(&cleaned));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let reply = send_workbench(&actor).await.expect("workbench reply");
        assert!(matches!(
            reply,
            Err(KernelInvocationFailure::Failed { detail, .. })
                if detail == "owned completion probe failed"
        ));
        tokio::time::timeout(Duration::from_secs(2), cleaned.notified())
            .await
            .expect("failed finalizer's execution guard cleaned up");
        assert_eq!(
            &*fixture.calls.lock(),
            &["workbench-start", "workbench-end"]
        );
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "done".into(),
                diagnostic: None,
            })
            .await
            .expect("shutdown");
        task.await.expect("actor task");
    }

    #[tokio::test]
    async fn dispatch_panic_settles_exact_control_as_unconfirmed() {
        let mut fixture = behavior(false);
        fixture.behavior.dispatch_panics = true;
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let execution = tidepool_runtime::session::WorkbenchExecutionId::from_digest([10; 16]);
        let control = crate::WorkbenchExecutionControl::untracked();
        let reply = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("panic").with_execution_id(execution.clone()),
            Some(Arc::clone(&control)),
        )
        .await
        .expect("workbench reply");
        assert!(matches!(
            &reply,
            Err(KernelInvocationFailure::Failed { .. })
        ));
        assert!(matches!(
            control.cancellation_outcome(execution, reply),
            crate::WorkbenchCancellationOutcome::Unconfirmed { .. }
        ));
        assert_eq!(actor.terminal().wait().await.kind, ActorExitKind::Failed);
        task.await.expect("actor task");
        assert!(matches!(
            actor.terminal().cleanup().expect("cleanup evidence").hook,
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
    }

    #[tokio::test]
    async fn forced_actor_stop_retains_uncertain_workbench_until_worker_returns() {
        let mut fixture = behavior(false);
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        fixture.behavior.workbench_gate = Some((Arc::clone(&entered), Arc::clone(&release)));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let control = crate::WorkbenchExecutionControl::untracked();
        let reply = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("pure ()"),
            Some(Arc::clone(&control)),
        );
        entered.notified().await;
        actor.address().stop(None);
        task.await.expect("actor task");
        assert_eq!(actor.terminal().wait().await.kind, ActorExitKind::Failed);
        assert!(matches!(
            actor.terminal().cleanup().expect("cleanup evidence").hook,
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
        assert!(matches!(
            reply.await.expect("workbench reply"),
            Err(KernelInvocationFailure::Failed { .. })
        ));
        assert!(matches!(
            control.terminal_reply(),
            Some(Err(KernelInvocationFailure::Failed { .. }))
        ));
        assert_eq!(&*fixture.calls.lock(), &["workbench-start"]);
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if fixture.calls.lock().len() == 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned worker completed after actor stop");
        assert_eq!(
            &*fixture.calls.lock(),
            &["workbench-start", "workbench-end"]
        );
        assert!(matches!(
            control.terminal_reply(),
            Some(Err(KernelInvocationFailure::Failed { .. }))
        ));
    }

    #[tokio::test]
    async fn forced_stop_abandons_execution_owned_completion() {
        let mut fixture = behavior(false);
        fixture.behavior.owned_workbench = true;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let cleaned = Arc::new(Notify::new());
        fixture.behavior.workbench_gate = Some((Arc::clone(&entered), Arc::clone(&release)));
        fixture.behavior.owned_cleanup = Some(Arc::clone(&cleaned));
        let (actor, task) = spawn_local_actor(None, fixture.behavior)
            .await
            .expect("spawn");
        let control = crate::WorkbenchExecutionControl::untracked();
        let reply = send_workbench_request(
            &actor,
            WorkbenchRequest::from_cell_input("pure ()"),
            Some(Arc::clone(&control)),
        );
        entered.notified().await;
        actor.address().stop(None);
        task.await.expect("actor task");
        assert!(matches!(
            reply.await.expect("workbench reply"),
            Err(KernelInvocationFailure::Failed { .. })
        ));
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), cleaned.notified())
            .await
            .expect("execution-owned completion was abandoned");
        assert!(matches!(
            actor.terminal().cleanup().expect("cleanup evidence").hook,
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
        assert_eq!(&*fixture.calls.lock(), &["workbench-start"]);
    }

    fn observed_probe(
        observations: &Arc<Mutex<Vec<(KernelContext, Option<ActorRef>)>>>,
    ) -> ProbeBehavior {
        let mut probe = behavior(false).behavior;
        probe.start_observations = Some(observations.clone());
        probe
    }

    #[tokio::test]
    async fn startup_ownership_precedes_live_link_and_survives_reparenting() {
        let observations = Arc::new(Mutex::new(Vec::new()));
        let directory = LocalActorDirectory::default();
        let (parent, parent_task) = spawn_local_actor_in_directory(
            None,
            observed_probe(&observations),
            crate::Incarnation(1),
            directory.clone(),
        )
        .await
        .unwrap();
        let parent_context = observations.lock()[0].0.clone();
        assert_eq!(
            parent_context.spawn_ownership(),
            SpawnOwnership::Independent
        );
        assert_eq!(observations.lock()[0].1, None);
        let mut children = Vec::new();
        for lifetime in [
            crate::WorkerLifetime::ActorOwned,
            crate::WorkerLifetime::InvocationOwned,
            crate::WorkerLifetime::SwarmOwned,
        ] {
            let child = parent_context
                .spawn_worker(None, observed_probe(&observations), lifetime)
                .await
                .unwrap();
            let (context, startup_parent) = observations.lock().last().unwrap().clone();
            assert_eq!(
                startup_parent, None,
                "Ractor has not linked during pre_start"
            );
            match lifetime {
                crate::WorkerLifetime::SwarmOwned => {
                    assert_eq!(context.spawn_ownership(), SpawnOwnership::Independent);
                    assert_eq!(context.supervisor_identity(), None);
                }
                _ => {
                    assert_eq!(
                        context.spawn_ownership(),
                        SpawnOwnership::Supervised(parent.identity())
                    );
                    assert_eq!(context.supervisor_identity(), Some(parent.identity()));
                }
            }
            children.push((child, context));
        }
        let (replacement_parent, replacement_task) = spawn_local_actor_in_directory(
            None,
            observed_probe(&observations),
            crate::Incarnation(1),
            directory.clone(),
        )
        .await
        .unwrap();
        let replacement_context = observations.lock().last().unwrap().0.clone();
        transfer_supervised_children(&parent_context, &replacement_context);
        assert!(!parent_context.owns_child(children[0].0.identity()));
        assert!(replacement_context.owns_child(children[0].0.identity()));
        let child_context = &children[0].1;
        assert_eq!(
            child_context.spawn_ownership(),
            SpawnOwnership::Supervised(parent.identity())
        );
        assert_eq!(
            child_context.supervisor_identity(),
            Some(replacement_parent.identity())
        );
        let (successor, admission) = child_context
            .spawn_successor(observed_probe(&observations))
            .await
            .unwrap();
        drop(admission);
        let successor_context = observations.lock().last().unwrap().0.clone();
        assert_eq!(
            successor_context.spawn_ownership(),
            SpawnOwnership::Supervised(replacement_parent.identity())
        );
        assert_eq!(
            successor_context.supervisor_identity(),
            Some(replacement_parent.identity())
        );
        assert!(replacement_context.owns_child(successor.identity()));
        let batch = directory.seal().cancel_all(ActorTerminal::new(
            ActorExitKind::Cancelled,
            "test retirement",
        ));
        let roots = batch.into_roots();
        assert_eq!(roots.len(), 3, "two parents and the unlinked swarm child");
        for root in roots {
            root.shutdown(ActorTerminal::new(
                ActorExitKind::Cancelled,
                "test retirement",
            ))
            .await
            .unwrap();
        }
        parent_task.await.unwrap();
        replacement_task.await.unwrap();
        assert_eq!(
            successor.terminal().get().unwrap().kind,
            ActorExitKind::Cancelled
        );
    }

    #[tokio::test]
    async fn admitted_pre_start_is_included_in_sealed_retirement_snapshot() {
        let observations = Arc::new(Mutex::new(Vec::new()));
        let directory = LocalActorDirectory::default();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut probe = observed_probe(&observations);
        probe.startup_gate = Some((entered.clone(), release.clone()));
        let spawning = {
            let directory = directory.clone();
            tokio::spawn(async move {
                spawn_local_actor_in_directory(None, probe, crate::Incarnation(1), directory).await
            })
        };
        entered.notified().await;
        let batch = directory.seal().cancel_all(ActorTerminal::new(
            ActorExitKind::Cancelled,
            "closed while starting",
        ));
        let roots = batch.into_roots();
        assert_eq!(roots.len(), 1);
        assert!(roots[0].terminal().get().is_none());
        assert!(roots[0].terminal().requested_shutdown().is_some());
        release.notify_one();
        let (actor, task) = spawning.await.unwrap().unwrap();
        task.await.unwrap();
        assert_eq!(actor.identity(), roots[0].identity());
        assert_eq!(
            actor.terminal().get().unwrap().kind,
            ActorExitKind::Cancelled
        );
        assert!(matches!(
            actor.terminal().cleanup().unwrap().realm,
            crate::CleanupComponentOutcome::Unsupported
        ));
    }

    struct FenceResource;

    impl ractor::Actor for FenceResource {
        type Msg = ();
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ractor::ActorRef<()>,
            _arguments: (),
        ) -> Result<(), ractor::ActorProcessingErr> {
            Ok(())
        }
        async fn handle(
            &self,
            _myself: ractor::ActorRef<()>,
            _message: (),
            _state: &mut (),
        ) -> Result<(), ractor::ActorProcessingErr> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn owned_child_teardown_fences_siblings_before_resource_cleanup() {
        let observations = Arc::new(Mutex::new(Vec::new()));
        let (parent, task) = spawn_local_actor(None, observed_probe(&observations))
            .await
            .unwrap();
        let context = observations.lock()[0].0.clone();
        let peers = Arc::new(Mutex::new(Vec::new()));
        for _ in 0..2 {
            let mut child = observed_probe(&observations);
            child.cleanup_peers = Some(peers.clone());
            let actor = context.spawn_child(None, child).await.unwrap();
            peers.lock().push(actor);
        }
        let resource_observed = Arc::new(AtomicBool::new(false));
        context
            .spawn_resource(None, FenceResource, (), {
                let peers = peers.clone();
                let resource_observed = resource_observed.clone();
                move |_| {
                    // This is an actual resource-retirement callback, invoked before
                    // child drain futures. Without the batch neither sibling has
                    // its retirement intent at this point.
                    for peer in peers.lock().iter() {
                        assert!(peer.terminal().requested_shutdown().is_some());
                    }
                    resource_observed.store(true, Ordering::SeqCst);
                    Box::pin(async { crate::CleanupComponentOutcome::Unsupported })
                }
            })
            .await
            .unwrap();
        let shutdown = parent
            .shutdown_with_cleanup(ActorTerminal::new(
                ActorExitKind::Cancelled,
                "owner retirement",
            ))
            .await
            .unwrap();
        task.await.unwrap();
        assert!(resource_observed.load(Ordering::SeqCst));
        assert!(matches!(
            shutdown.cleanup.realm,
            crate::CleanupComponentOutcome::Unsupported
        ));
        // Generic probes cannot issue realm proof. Every sibling nevertheless
        // has its genuine actor-owned cancellation and component evidence.
        assert!(matches!(
            shutdown.cleanup.children,
            crate::CleanupComponentOutcome::Unconfirmed(_)
        ));
        for child in peers.lock().iter() {
            assert_eq!(
                child.terminal().get().unwrap().kind,
                ActorExitKind::Cancelled
            );
            assert!(child.terminal().cleanup().is_some());
        }
    }

    #[tokio::test]
    async fn cancelled_prepared_replacement_accepts_custody_without_running_backlog() {
        let mut probe = behavior(false);
        probe.behavior.replacement_staged = true;
        probe.behavior.mailbox_ready = false;
        let (actor, task) = spawn_local_actor(None, probe.behavior).await.unwrap();
        let batch = crate::kernel::RetirementBatch::issue(vec![(
            actor.clone(),
            ActorTerminal::new(ActorExitKind::Cancelled, "cancelled before activation"),
        )]);
        let caller = ActorRef::first(crate::ActorId(99));
        let dropped = Arc::new(AtomicUsize::new(0));
        let (reply, receive) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::ActivateReplacement {
                backlog: VecDeque::from([KernelMessage::Call {
                    caller,
                    ancestry: crate::CallAncestry::begin(caller),
                    request: MailboxValue::probe(SessionId(7), dropped.clone()),
                    reply: reply.into(),
                }]),
                draining: false,
            })
            .unwrap();
        task.await.unwrap();
        assert!(
            receive.await.is_err(),
            "cancelled backlog cannot execute authored call"
        );
        assert!(probe.mailbox_calls.lock().is_empty());
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(
            actor.terminal().get().unwrap().kind,
            ActorExitKind::Cancelled
        );
        assert!(matches!(
            actor.terminal().cleanup().unwrap().realm,
            crate::CleanupComponentOutcome::Unsupported
        ));
        assert_eq!(batch.into_actors().len(), 1);
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
                    diagnostic: None,
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
                diagnostic: None,
            })
            .await
            .unwrap();
        active_task.await.unwrap();
    }

    #[tokio::test]
    async fn one_actor_never_reenters_while_an_operation_is_pending() {
        let mut fixture = behavior(false);
        fixture.behavior.mailbox_ready = false;
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
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), second_rx)
                .await
                .expect("queued native tool resumes without a mailbox receiver")
                .expect("second reply")
                .unwrap(),
            "second"
        );
        assert_eq!(
            &*fixture.calls.lock(),
            &["first-start", "first-end", "second"]
        );

        let terminal = ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "done".into(),
            diagnostic: None,
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
    async fn cancelled_owned_drain_before_dispatch_keeps_target_admission_open() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let control = crate::WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        assert!(control.request_cancellation());
        assert!(matches!(
            crate::resident_actor::wait_drain_event(&actor, &control, &RetainedActorExit::new())
                .await,
            crate::resident_actor::DrainWaitEvent::Cancelled
        ));
        assert!(control.cancellation_requested());
        assert!(actor.terminal().get().is_none());
        actor
            .address()
            .call(
                |reply| KernelMessage::Tool {
                    invocation: tool_invocation("still-open"),
                    reply,
                },
                None,
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "test finished".into(),
                diagnostic: None,
            })
            .await
            .unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_owned_drain_waiter_does_not_withdraw_the_accepted_target_fence() {
        let mut fixture = behavior(false);
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        fixture.behavior.workbench_gate = Some((Arc::clone(&entered), Arc::clone(&release)));
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let workbench = send_workbench(&actor);
        entered.notified().await;
        let control = crate::WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        let retirement = RetainedActorExit::new();
        let mut drain = Box::pin(crate::resident_actor::wait_drain_event(
            &actor,
            &control,
            &retirement,
        ));
        assert!(matches!(
            futures_util::poll!(&mut drain),
            std::task::Poll::Pending
        ));
        assert!(control.request_cancellation());
        assert!(matches!(
            drain.await,
            crate::resident_actor::DrainWaitEvent::Cancelled
        ));
        assert!(control.cancellation_requested());
        assert!(actor.terminal().get().is_none());
        release.notify_one();
        assert!(workbench.await.unwrap().is_ok());
        let terminal = tokio::time::timeout(Duration::from_secs(2), actor.terminal().wait())
            .await
            .expect("accepted drain remains owned by its target");
        assert_eq!(terminal.kind, ActorExitKind::Completed);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn prior_owned_retirement_wins_when_target_drain_is_already_ready() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        actor
            .shutdown(ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "target completed".into(),
                diagnostic: None,
            })
            .await
            .unwrap();
        task.await.unwrap();
        let control = crate::WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        let retirement = RetainedActorExit::new();
        let terminal = ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "original caller retires".into(),
            diagnostic: None,
        };
        retirement.request_shutdown(terminal.clone());
        assert!(matches!(
            crate::resident_actor::wait_drain_event(&actor, &control, &retirement).await,
            crate::resident_actor::DrainWaitEvent::Retired(observed) if observed == terminal
        ));
        assert!(
            control.request_cancellation(),
            "drain never claimed native delivery"
        );
    }

    #[tokio::test]
    async fn drain_racing_shutdown_observes_the_exact_retained_terminal() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let (drained, shutdown) = tokio::join!(
            actor.drain(),
            actor.shutdown(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "owner stopped".into(),
                diagnostic: None,
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
        let mut accepted_call = Box::pin(actor.call(
            caller,
            crate::CallAncestry::begin(caller),
            MailboxValue::probe(SessionId(1), Arc::clone(&dropped)),
        ));
        assert!(
            std::future::poll_fn(|context| {
                std::task::Poll::Ready(accepted_call.as_mut().poll(context).is_pending())
            })
            .await
        );
        actor.drain().await.unwrap();
        assert!(actor.terminal().get().is_none());
        assert_eq!(
            actor.cast(
                caller,
                MailboxValue::probe(SessionId(1), Arc::clone(&dropped))
            ),
            Err(KernelCallFailure::MailboxClosed(actor.identity()))
        );
        assert!(matches!(
            actor
                .call(
                    caller,
                    crate::CallAncestry::begin(caller),
                    MailboxValue::probe(SessionId(2), Arc::clone(&dropped)),
                )
                .await,
            Err(KernelCallFailure::MailboxClosed(target)) if target == actor.identity()
        ));
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
        drop(accepted_call.await.unwrap());
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
        assert_eq!(dropped.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn dropping_a_mailbox_waiter_does_not_cancel_accepted_work() {
        let fixture = behavior(false);
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
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
            .unwrap()
            .unwrap()
            .unwrap();

        let caller = ActorRef::first(crate::ActorId(99));
        let dropped = Arc::new(AtomicUsize::new(0));
        let mut waiter = Box::pin(actor.call(
            caller,
            crate::CallAncestry::begin(caller),
            MailboxValue::probe(SessionId(1), Arc::clone(&dropped)),
        ));
        assert!(
            std::future::poll_fn(|context| {
                std::task::Poll::Ready(waiter.as_mut().poll(context).is_pending())
            })
            .await
        );
        drop(waiter);

        actor
            .address()
            .call(
                |reply| KernelMessage::Tool {
                    invocation: tool_invocation("unpark-mailbox"),
                    reply,
                },
                Some(Duration::from_secs(1)),
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        actor.drain().await.unwrap();
        assert_eq!(actor.terminal().wait().await.kind, ActorExitKind::Completed);
        assert_eq!(
            &fixture.calls.lock()[..3],
            &["park-mailbox", "unpark-mailbox", "call"]
        );
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        task.await.unwrap();
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
                diagnostic: None,
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
            diagnostic: None,
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
            diagnostic: None,
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
    async fn raw_terminal_is_bounded_before_cleanup_callbacks_and_shutdown_reply() {
        let fixture = behavior(false);
        let callbacks = fixture.terminal_callbacks.clone();
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let original = ActorTerminal {
            kind: ActorExitKind::Failed,
            summary: "original terminal disposition".into(),
            diagnostic: Some(tidepool_toolchain::failclass::FailureEnvelope {
                class: tidepool_toolchain::failclass::FailureClass::UserHaskell,
                phase: tidepool_toolchain::failclass::Phase::Compile,
                cause: Some(tidepool_toolchain::failclass::CompileFailureCause::SourceDiagnostics),
                message: "λ\n".repeat(32 * 1024),
            }),
        };
        let (reply, received) = oneshot::channel();
        actor
            .address()
            .send_message(KernelMessage::Shutdown {
                terminal: original.clone(),
                reply: reply.into(),
            })
            .expect("admit raw public terminal without an intent precheck");
        let returned = received.await.expect("actual shutdown reply");
        task.await.expect("actor task joined");
        assert_eq!(returned.kind, original.kind);
        assert_eq!(returned.summary, original.summary);
        assert!(returned.diagnostic.as_ref().unwrap().message.len() <= 16 * 1024);
        assert_eq!(actor.terminal().get(), Some(returned.clone()));
        assert_eq!(*callbacks.lock(), vec![returned.clone(), returned.clone()]);
        let directory = tempfile::tempdir().unwrap();
        let anchor =
            tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path()).unwrap();
        let journal = crate::ActorRecoveryJournal::open(&anchor, "actors.jsonl").unwrap();
        let descriptor = crate::ActorDescriptor::new(
            "raw-terminal",
            crate::ActorPlacement {
                session: SessionId(1),
                lexical_scope: tidepool_codegen::scope::ScopeId(1),
                resource_scope: tidepool_codegen::suspension::RealmId(1),
            },
        );
        journal.admit(actor.identity(), &descriptor, &[]).unwrap();
        journal.retire(actor.identity(), original).unwrap();
        assert_eq!(journal.records()[0].terminal, Some(returned.clone()));
        assert_eq!(
            actor
                .shutdown(ActorTerminal::new(
                    ActorExitKind::Cancelled,
                    "later cleanup"
                ))
                .await
                .unwrap(),
            returned
        );
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
        assert_eq!(terminal.diagnostic, None);
        let supervisor = ActorRef::first(crate::ActorId(99));
        let observed = actor
            .retire_by(
                supervisor,
                ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "late retirement".into(),
                    diagnostic: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(observed, terminal);
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
                diagnostic: None,
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
            diagnostic: None,
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
                diagnostic: None,
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
                diagnostic: None,
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
                diagnostic: None,
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
                diagnostic: None,
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
        assert_eq!(child_terminal.diagnostic, None);
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
                    diagnostic: None,
                },
                reply: shutdown_tx.into(),
            })
            .expect("shutdown owner");
        shutdown_rx.await.expect("shutdown reply");
        owner_task.await.expect("owner task");
    }
    #[tokio::test]
    async fn compiler_diagnostic_survives_child_kernel_task_and_retirement() {
        use tidepool_toolchain::failclass::{CompileFailureCause, FailureClass, Phase};

        tidepool_testing::eval_harness::require_extract();
        let compile_error = tidepool_toolchain::artifacts::check_source(
            &tidepool_toolchain::artifacts::SourceCheckRequest {
                source: include_str!("local_actor/failure_origin.hs"),
                include: &[],
                fallback_module_name: "FailureOrigin",
            },
        )
        .expect_err("the genuine compiler must reject the authored unknown identifier");
        assert!(matches!(
            compile_error,
            tidepool_toolchain::CompileError::Diagnostics(_)
        ));
        let failure =
            crate::ResidentActorWorkbenchError::Compile(compile_error).into_kernel_behavior_error();
        let expected = failure
            .diagnostic
            .clone()
            .expect("compiler classification retained");
        assert_eq!(expected.class, FailureClass::UserHaskell);
        assert_eq!(expected.phase, Phase::Compile);
        assert_eq!(expected.cause, Some(CompileFailureCause::SourceDiagnostics));
        assert!(expected.message.contains("missingChildFailureOrigin"));
        assert!(expected.message.contains("FailureOrigin.hs"));

        let mut child_fixture = behavior(false);
        child_fixture.behavior.resume_failure = Some(failure);
        let mut owner_fixture = behavior(false);
        owner_fixture
            .behavior
            .pending_children
            .push_back(child_fixture.behavior);
        let children = owner_fixture.behavior.spawned_children.clone();
        let (owner, owner_task) = spawn_local_actor(None, owner_fixture.behavior)
            .await
            .expect("spawn owning actor");
        let (reply, receive) = oneshot::channel();
        owner
            .address()
            .send_message(KernelMessage::Tool {
                invocation: tool_invocation("spawn_queued"),
                reply: reply.into(),
            })
            .expect("admit child through the actual owner");
        receive
            .await
            .expect("child admission reply")
            .expect("child admitted");
        let child = children
            .lock()
            .first()
            .cloned()
            .expect("exact admitted child");
        child
            .address()
            .send_message(KernelMessage::Resume {
                kind: crate::kernel::KernelResume::ContinueProgram,
            })
            .expect("admit serial child kernel task");
        let terminal = tokio::time::timeout(Duration::from_secs(10), child.terminal().wait())
            .await
            .expect("child failure must settle");
        assert_eq!(terminal.kind, ActorExitKind::Failed);
        assert_eq!(terminal.diagnostic, Some(expected));
        let repeated = child
            .shutdown(ActorTerminal::new(
                ActorExitKind::Cancelled,
                "later cancellation",
            ))
            .await
            .expect("repeat child observation");
        assert_eq!(repeated, terminal);
        assert_eq!(child.terminal().get(), Some(terminal.clone()));

        let directory = tempfile::tempdir().expect("durable lifecycle directory");
        let anchor = tidepool_atomic_write::DirectoryAnchor::open_existing(directory.path())
            .expect("durable lifecycle anchor");
        let journal = crate::ActorRecoveryJournal::open(&anchor, "actors.jsonl")
            .expect("original lifecycle owner");
        let descriptor = crate::ActorDescriptor::new(
            "compiler-failure-child",
            crate::ActorPlacement {
                session: SessionId(1),
                lexical_scope: tidepool_codegen::scope::ScopeId(1),
                resource_scope: tidepool_codegen::suspension::RealmId(1),
            },
        );
        journal
            .admit(child.identity(), &descriptor, &[])
            .expect("record exact child identity");
        journal
            .retire(child.identity(), terminal.clone())
            .expect("persist original terminal");
        journal
            .retire(child.identity(), terminal.clone())
            .expect("idempotent same retirement");
        assert!(journal
            .retire(
                child.identity(),
                ActorTerminal::new(ActorExitKind::Cancelled, "later cleanup",)
            )
            .is_err());
        drop(journal);
        let journal = crate::ActorRecoveryJournal::open_existing(&anchor, "actors.jsonl")
            .expect("reopen typed diagnostic evidence");
        assert_eq!(journal.records()[0].terminal, Some(terminal));

        let (reply, receive) = oneshot::channel();
        owner
            .address()
            .send_message(KernelMessage::Tool {
                invocation: tool_invocation("second"),
                reply: reply.into(),
            })
            .expect("failure remains isolated to its child");
        assert_eq!(
            receive.await.expect("owner reply").expect("owner alive"),
            "second"
        );
        owner
            .shutdown(ActorTerminal::new(ActorExitKind::Completed, "owner done"))
            .await
            .expect("retire owner");
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
                    diagnostic: None,
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
            terminal: actor.terminal().clone(),
            spawn_ownership: SpawnOwnership::Independent,
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
        // best-effort: draining the paired oneshot; its value isn't asserted here.
        receiver.await.ok();
        assert!(
            actor.terminal().cleanup().is_none(),
            "forced terminal must not manufacture component proof"
        );
        let observed = actor
            .shutdown_with_cleanup(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "observe only".into(),
                diagnostic: None,
            })
            .await
            .unwrap();
        assert!(!observed.cleanup.is_confirmed());
        assert_eq!(observed.cleanup.actor(), actor.identity());
        context
            .directory
            .insert(
                actor.clone(),
                std::sync::Weak::new(),
                SpawnOwnership::Independent,
            )
            .unwrap();
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
                terminal: owner.terminal().clone(),
                spawn_ownership: SpawnOwnership::Independent,
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
                    diagnostic: None,
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
                    diagnostic: None,
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
                        diagnostic: None,
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
                        diagnostic: None,
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
            _hosted_checkpoint_capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
        ) -> BoxFuture<'a, Result<KernelStep<serde_json::Value>, KernelInvocationFailure>> {
            Box::pin(async move {
                Err(KernelInvocationFailure::Rejected {
                    receipts: Vec::new(),
                    actor: context.identity(),
                    detail: "pausing probe has no tools".into(),
                    diagnostic: None,
                })
            })
        }

        fn workbench(
            &mut self,
            _context: &KernelContext,
            _invocation: crate::ActorWorkbenchInvocation,
            _control: Option<std::sync::Arc<crate::WorkbenchExecutionControl>>,
        ) -> BoxFuture<'_, Result<KernelStep<WorkbenchResponse>, KernelInvocationFailure>> {
            Box::pin(async {
                Ok(KernelStep::Continue(WorkbenchResponse {
                    status: WorkbenchRunStatus::Committed,
                    summary: None,
                    items: Vec::new(),
                    next_index: 0,
                    total: 0,
                    publication: None,
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

        fn child_exited(&mut self, _notice: ChildExitNotice) {}
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
                diagnostic: None,
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

    #[tokio::test]
    async fn forest_shutdown_preserves_confirmed_and_unconfirmed_component_evidence() {
        use crate::{CleanupComponentOutcome as Component, ForestRootShutdown};
        for realm in [
            Component::Confirmed,
            Component::Unconfirmed("busy resident machine".into()),
            Component::Unsupported,
        ] {
            let mut fixture = behavior(false);
            fixture.behavior.shutdown_override =
                Some(ShutdownOverride::Fixed(Component::Confirmed, realm.clone()));
            let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
            let outcome =
                crate::resident_actor::shutdown_forest_root(&actor, Duration::from_secs(1)).await;
            task.await.unwrap();
            assert_eq!(
                outcome.is_confirmed(),
                matches!(realm, Component::Confirmed)
            );
            let ForestRootShutdown::Settled(shutdown) = outcome else {
                panic!("actor did not settle");
            };
            assert_eq!(shutdown.cleanup.actor(), actor.identity());
            assert_eq!(shutdown.cleanup.realm(), &realm);
            assert_eq!(shutdown.terminal.kind, ActorExitKind::Cancelled);
            let repeated =
                crate::resident_actor::shutdown_forest_root(&actor, Duration::from_secs(1)).await;
            assert_eq!(repeated, ForestRootShutdown::Settled(shutdown));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn forest_shutdown_timeout_remains_unconfirmed_after_forced_actor_exit() {
        let mut fixture = behavior(false);
        fixture.behavior.shutdown_override =
            Some(ShutdownOverride::HangRealmForever(Arc::new(Notify::new())));
        let (actor, task) = spawn_local_actor(None, fixture.behavior).await.unwrap();
        let outcome =
            crate::resident_actor::shutdown_forest_root(&actor, Duration::from_secs(1)).await;
        assert_eq!(
            outcome,
            crate::ForestRootShutdown::TimedOut {
                actor: actor.identity()
            }
        );
        assert!(!outcome.is_confirmed());
        task.await.unwrap();
        assert!(
            !outcome.is_confirmed(),
            "scheduler exit must not confirm resource cleanup"
        );
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
                    diagnostic: None,
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
                diagnostic: None,
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
                diagnostic: None,
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
                diagnostic: None,
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
            terminal: terminal.clone(),
            spawn_ownership: SpawnOwnership::Independent,
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
            behavior: BehaviorSlot(Some(behavior(false).behavior)),
            pending_tasks: HashMap::new(),
            next_task_generation: 0,
            pending_child_exits: Vec::new(),
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
                diagnostic: None,
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
                diagnostic: None,
            })
            .await
            .unwrap();
        successor_task.await.unwrap();
    }
}

#[cfg(test)]
#[path = "local_actor/terminal_transfer_tests.rs"]
mod terminal_transfer_tests;
