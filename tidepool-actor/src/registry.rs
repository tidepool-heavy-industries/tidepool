use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::MonotonicIdIssuer;
use tidepool_repr::PrincipalId;
use tidepool_runtime::session::RootCustody;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::agent_session::AgentSessionState;
use crate::mailbox::{InstalledActorState, InstalledReceiver};
use crate::{
    ActorAgentSession, ActorEffectProfile, ActorEvent, ActorEventRecord, ActorExitKind, ActorId,
    ActorOperationClass, ActorPlacement, ActorRef, ActorSessionContext, ActorSourceImports,
    ActorTerminal, CallDisposition, CallFailure, CallId, CallStatus, CallTicket, EventCausality,
    ExitObservation, MailboxFailure, MailboxMessageKind, MailboxValue, MessageId, ParkedObligation,
    StartInitiator, WaitDisposition, WaitError, WaitId, WaitTicket,
};

/// Immutable attributes selected before an actor begins initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorDescriptor {
    label: String,
    profile: ActorEffectProfile,
    effect_names: Vec<String>,
    effect_policy: EffectRunPolicy,
    live_payload: LivePayloadPolicy,
    placement: ActorPlacement,
    source_imports: ActorSourceImports,
}

impl ActorDescriptor {
    /// Build the initial actor execution contract. Every Haskell request
    /// suspends into the actor-local Rust interpreter, and effect payloads use
    /// the shared Haskell request convention.
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        effect_names: impl IntoIterator<Item = impl Into<String>>,
        placement: ActorPlacement,
    ) -> Self {
        Self {
            label: label.into(),
            profile: ActorEffectProfile::ReadWrite,
            effect_names: effect_names.into_iter().map(Into::into).collect(),
            effect_policy: EffectRunPolicy::HandleOrSuspend,
            live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            placement,
            source_imports: ActorSourceImports::default(),
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn profile(&self) -> ActorEffectProfile {
        self.profile
    }

    /// Select the immutable named effect profile before actor allocation.
    #[must_use]
    pub fn with_profile(mut self, profile: ActorEffectProfile) -> Self {
        self.profile = profile;
        self
    }

    #[must_use]
    pub fn effect_names(&self) -> &[String] {
        &self.effect_names
    }

    #[must_use]
    pub fn effect_policy(&self) -> EffectRunPolicy {
        self.effect_policy
    }

    #[must_use]
    pub fn live_payload_policy(&self) -> LivePayloadPolicy {
        self.live_payload
    }

    #[must_use]
    pub fn placement(&self) -> ActorPlacement {
        self.placement
    }

    /// Install the exact declaration membrane compiled for this actor. This
    /// consumes the descriptor so the import set is fixed before startup.
    #[must_use]
    pub fn with_source_imports(mut self, source_imports: ActorSourceImports) -> Self {
        self.source_imports = source_imports;
        self
    }

    #[must_use]
    pub fn source_imports(&self) -> &ActorSourceImports {
        &self.source_imports
    }
}

/// A private initialization capability. No callable [`ActorRef`] is exposed
/// until [`ActorRegistry::publish_ready`] consumes the readiness transition.
pub struct StartingActor {
    actor: ActorRef,
    registry: Weak<RegistryInner>,
    armed: bool,
}

impl std::fmt::Debug for StartingActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartingActor").finish_non_exhaustive()
    }
}

impl StartingActor {
    #[must_use]
    pub(crate) fn actor(&self) -> ActorRef {
        self.actor
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorLifecycle {
    Initializing,
    Ready,
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorTurnKind {
    Haskell,
    AgentSession,
    Advisory,
    Mailbox,
}

/// Ephemeral, identity-only readiness emitted by registry state transitions.
/// The registry remains authoritative: each wake tells the host which exact
/// item to recheck and carries no lifecycle state or live-value custody.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorRuntimeWake {
    ActorReady { actor: ActorRef },
    MailboxReady { actor: ActorRef },
    CallReady { caller: ActorRef, call: CallId },
    WaitReady { waiter: ActorRef, wait: WaitId },
    ActorExited { actor: ActorRef },
}

/// The sole receiving end of an [`ActorRegistry`]'s ephemeral readiness
/// stream. It is intentionally not clonable: scheduling has one owner.
pub struct ActorRuntimeWakes {
    receiver: UnboundedReceiver<ActorRuntimeWake>,
    ready: HashSet<ActorRuntimeWake>,
}

impl ActorRuntimeWakes {
    /// Wait for at least one registry transition, then absorb every wake that
    /// is already queued. Returns `false` only after the registry has dropped
    /// the stream.
    pub async fn wait(&mut self) -> bool {
        let Some(wake) = self.receiver.recv().await else {
            return false;
        };
        self.record(wake);
        self.drain_available();
        true
    }

    /// Absorb every currently queued wake without blocking.
    pub fn drain_available(&mut self) {
        while let Ok(wake) = self.receiver.try_recv() {
            self.record(wake);
        }
    }

    /// Consume one exact level trigger after the host has installed the state
    /// needed to service it. Early call and wait settlement therefore remains
    /// buffered, while repeated mailbox readiness collapses to one recheck.
    pub fn take(&mut self, wake: ActorRuntimeWake) -> bool {
        self.ready.remove(&wake)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ready.is_empty() && self.receiver.is_empty()
    }

    /// Discard residual identity-only triggers after their owning host has
    /// reached terminal quiescence. No lifecycle or value custody lives here.
    pub(crate) fn discard_all(&mut self) {
        self.drain_available();
        self.ready.clear();
    }

    fn record(&mut self, wake: ActorRuntimeWake) {
        self.ready.insert(wake);
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ActorRegistryError {
    #[error("actor {0:?} is unknown")]
    Unknown(ActorRef),
    #[error("actor reference {given:?} is stale; current incarnation is {current:?}")]
    Stale { given: ActorRef, current: ActorRef },
    #[error("actor {0:?} is still initializing")]
    Initializing(ActorRef),
    #[error("actor {0:?} is already ready")]
    AlreadyReady(ActorRef),
    #[error("actor {0:?} has exited")]
    Exited(ActorRef),
    #[error("actor {0:?} has not exited")]
    NotExited(ActorRef),
    #[error(
        "actor {owner:?} with profile {owner_profile:?} cannot start child profile {child_profile:?}"
    )]
    ProfileEscalation {
        owner: ActorRef,
        owner_profile: ActorEffectProfile,
        child_profile: ActorEffectProfile,
    },
    #[error("actor {0:?} already has an installed mailbox receiver")]
    ReceiverAlreadyInstalled(ActorRef),
    #[error("actor {0:?} already has an installed shutdown hook")]
    ShutdownAlreadyInstalled(ActorRef),
    #[error("actor {0:?} has no installed mailbox receiver")]
    ReceiverMissing(ActorRef),
    #[error("actor {actor:?} already has an active {active:?} turn")]
    Busy {
        actor: ActorRef,
        active: ActorTurnKind,
    },
    #[error("actor {actor:?} is parked on {obligation:?}")]
    Parked {
        actor: ActorRef,
        obligation: ParkedObligation,
    },
    #[error("startup token belongs to another actor registry")]
    ForeignStartup,
    #[error("startup token for {capability:?} cannot drive agent session {session:?}")]
    StartupSessionMismatch {
        session: ActorRef,
        capability: ActorRef,
    },
    #[error("actor session {session:?} cannot consume a turn lease for {lease:?}")]
    AgentSessionTurnMismatch { session: ActorRef, lease: ActorRef },
    #[error("actor {actor:?} cannot enter an agent session from a {kind:?} turn")]
    AgentSessionTurnKind {
        actor: ActorRef,
        kind: ActorTurnKind,
    },
    #[error("actor {actor:?} turn transition expected {expected:?}, but registry held {active:?}")]
    TurnTransitionMismatch {
        actor: ActorRef,
        expected: ActorTurnKind,
        active: Option<ActorTurnKind>,
    },
    #[error("the actor runtime wake receiver has already been claimed")]
    WakeReceiverClaimed,
}

/// Thread-safe ownership and lifecycle registry. It intentionally does not
/// own a machine session: turn admission is a guard above the existing runtime
/// checkout mechanism.
#[derive(Clone)]
pub struct ActorRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    ids: MonotonicIdIssuer,
    message_ids: MonotonicIdIssuer,
    call_ids: MonotonicIdIssuer,
    wait_ids: MonotonicIdIssuer,
    state: Mutex<RegistryState>,
}

#[derive(Default)]
struct RegistryState {
    actors: HashMap<ActorId, ActorEntry>,
    calls: HashMap<CallId, CallEntry>,
    waits: HashMap<WaitId, WaitEntry>,
    next_stream_sequence: u64,
    events: Vec<ActorEventRecord>,
    wake_sender: Option<UnboundedSender<ActorRuntimeWake>>,
    wake_receiver_claimed: bool,
}

struct ActorEntry {
    reference: ActorRef,
    descriptor: ActorDescriptor,
    owner: Option<ActorRef>,
    children: BTreeSet<ActorRef>,
    lifecycle: ActorLifecycle,
    active_turn: Option<ActorTurnKind>,
    mailbox: VecDeque<QueuedMessage>,
    receiver: Option<InstalledReceiver>,
    shutdown: Option<RootCustody>,
    parked: Option<ParkedObligation>,
    terminal: Option<ActorTerminal>,
    agent_session: Option<Arc<Mutex<AgentSessionState>>>,
    next_event_sequence: u64,
}

pub(crate) struct ActorCleanup {
    pub(crate) actor: ActorRef,
    pub(crate) context: ActorSessionContext,
    pub(crate) terminal: ActorTerminal,
    pub(crate) shutdown: Option<RootCustody>,
}

/// Atomic custody transfer for every resident resource covered by one
/// terminal subtree transition.
pub(crate) struct ActorCleanupBatch {
    actors: Vec<ActorCleanup>,
}

impl ActorCleanupBatch {
    fn new(actors: Vec<ActorCleanup>) -> Self {
        Self { actors }
    }

    pub(crate) fn into_actors(self) -> impl DoubleEndedIterator<Item = ActorCleanup> {
        self.actors.into_iter()
    }
}

struct QueuedMessage {
    id: MessageId,
    sender: ActorRef,
    value: MailboxValue,
    call: Option<CallId>,
}

struct CallEntry {
    caller: ActorRef,
    target: ActorRef,
    state: CallState,
}

struct WaitEntry {
    waiter: ActorRef,
    target: ActorRef,
}

enum CallState {
    Queued,
    Delivered,
    Replied(MailboxValue),
    Failed(CallFailure),
}

/// One accepted mailbox message transferred to the target actor.
#[derive(Debug)]
pub enum MailboxDelivery {
    Cast(CastDelivery),
    Call(CallDelivery),
}

/// One-way delivery holding the target actor's exclusive mailbox turn.
pub struct CastDelivery {
    id: MessageId,
    sender: ActorRef,
    value: Option<MailboxValue>,
    _lease: TurnLease,
}

impl std::fmt::Debug for CastDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CastDelivery")
            .field("id", &self.id)
            .field("sender", &self.sender)
            .finish_non_exhaustive()
    }
}

/// Linear reply obligation for one dequeued synchronous call. Dropping it
/// without replying settles the caller with `DeliveryAbandoned`.
pub struct CallDelivery {
    registry: ActorRegistry,
    id: MessageId,
    call: CallId,
    caller: ActorRef,
    target: ActorRef,
    value: Option<MailboxValue>,
    _lease: TurnLease,
    settled: bool,
}

impl std::fmt::Debug for CallDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallDelivery")
            .field("id", &self.id)
            .field("call", &self.call)
            .field("caller", &self.caller)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl ActorRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                ids: MonotonicIdIssuer::new("actor"),
                message_ids: MonotonicIdIssuer::new("actor-message"),
                call_ids: MonotonicIdIssuer::new("actor-call"),
                wait_ids: MonotonicIdIssuer::new("actor-wait"),
                state: Mutex::new(RegistryState::default()),
            }),
        }
    }

    /// Claim the sole ephemeral runtime-wake stream for this registry.
    /// Publication is disabled until this method is called, so registries
    /// without a production host never accumulate an unread queue.
    pub fn take_runtime_wakes(&self) -> Result<ActorRuntimeWakes, ActorRegistryError> {
        let mut state = self.inner.state.lock();
        if state.wake_receiver_claimed {
            return Err(ActorRegistryError::WakeReceiverClaimed);
        }
        let (sender, receiver) = mpsc::unbounded_channel();
        state.wake_sender = Some(sender);
        state.wake_receiver_claimed = true;
        Ok(ActorRuntimeWakes {
            receiver,
            ready: HashSet::new(),
        })
    }

    /// Allocate an initializing actor. The returned token is deliberately not
    /// an `ActorRef`; callers publish a usable reference only after authored
    /// startup and installation have completed.
    pub fn begin_start(
        &self,
        owner: Option<ActorRef>,
        descriptor: ActorDescriptor,
        initiator: StartInitiator,
    ) -> Result<StartingActor, ActorRegistryError> {
        let mut state = self.inner.state.lock();
        if let Some(owner) = owner {
            let owner_entry = entry(&state, owner)?;
            match owner_entry.lifecycle {
                ActorLifecycle::Initializing => {
                    return Err(ActorRegistryError::Initializing(owner));
                }
                ActorLifecycle::Exited => return Err(ActorRegistryError::Exited(owner)),
                ActorLifecycle::Ready => {}
            }
            if !owner_entry
                .descriptor
                .profile
                .permits_child(descriptor.profile)
            {
                return Err(ActorRegistryError::ProfileEscalation {
                    owner,
                    owner_profile: owner_entry.descriptor.profile,
                    child_profile: descriptor.profile,
                });
            }
        }

        let reference = ActorRef::first(ActorId(self.inner.ids.next_raw()));

        let created = ActorEvent::Created {
            owner,
            label: descriptor.label.clone(),
            profile: descriptor.profile,
            effect_stack: descriptor.effect_names.clone(),
        };
        state.actors.insert(
            reference.id,
            ActorEntry {
                reference,
                descriptor,
                owner,
                children: BTreeSet::new(),
                lifecycle: ActorLifecycle::Initializing,
                active_turn: None,
                mailbox: VecDeque::new(),
                receiver: None,
                shutdown: None,
                parked: None,
                terminal: None,
                agent_session: None,
                next_event_sequence: 0,
            },
        );
        if let Some(owner) = owner {
            entry_mut(&mut state, owner)?.children.insert(reference);
        }
        record(
            &mut state,
            reference,
            EventCausality {
                owner,
                ..EventCausality::default()
            },
            created,
        )?;
        record(
            &mut state,
            reference,
            EventCausality::default(),
            ActorEvent::Started { initiator },
        )?;

        Ok(StartingActor {
            actor: reference,
            registry: Arc::downgrade(&self.inner),
            armed: true,
        })
    }

    /// Linearize readiness and reveal the exact-incarnation reference.
    pub fn publish_ready(
        &self,
        mut starting: StartingActor,
    ) -> Result<ActorRef, ActorRegistryError> {
        self.publish_ready_borrowed(&mut starting)
    }

    /// Publish while leaving the private startup capability with its
    /// structured owner on failure, so that owner can run awaited cleanup.
    pub(crate) fn publish_ready_borrowed(
        &self,
        starting: &mut StartingActor,
    ) -> Result<ActorRef, ActorRegistryError> {
        self.validate_starting(starting)?;
        let mut state = self.inner.state.lock();
        let actor = starting.actor;
        match entry(&state, actor)?.lifecycle {
            ActorLifecycle::Initializing => {}
            ActorLifecycle::Ready => return Err(ActorRegistryError::AlreadyReady(actor)),
            ActorLifecycle::Exited => return Err(ActorRegistryError::Exited(actor)),
        }
        if let Some(active) = entry(&state, actor)?.active_turn {
            return Err(ActorRegistryError::Busy { actor, active });
        }
        entry_mut(&mut state, actor)?.lifecycle = ActorLifecycle::Ready;
        record(
            &mut state,
            actor,
            EventCausality::default(),
            ActorEvent::Ready,
        )?;
        wake(&mut state, ActorRuntimeWake::ActorReady { actor });
        starting.armed = false;
        Ok(actor)
    }

    /// Attach to the initializing actor's sole model session without exposing
    /// its callable reference before readiness.
    pub fn startup_agent_session(
        &self,
        starting: &StartingActor,
    ) -> Result<ActorAgentSession, ActorRegistryError> {
        self.validate_starting(starting)?;
        ActorAgentSession::attach(self.clone(), starting.actor)
    }

    /// Install the first receiver before readiness publication. The private
    /// startup capability proves this is the exact unpublished incarnation.
    pub(crate) fn install_starting_receiver(
        &self,
        starting: &StartingActor,
        receiver: InstalledReceiver,
    ) -> Result<(), ActorRegistryError> {
        self.validate_starting(starting)?;
        let mut state = self.inner.state.lock();
        let actor = entry_mut(&mut state, starting.actor)?;
        if actor.lifecycle != ActorLifecycle::Initializing {
            return Err(match actor.lifecycle {
                ActorLifecycle::Ready => ActorRegistryError::AlreadyReady(starting.actor),
                ActorLifecycle::Exited => ActorRegistryError::Exited(starting.actor),
                ActorLifecycle::Initializing => unreachable!(),
            });
        }
        if actor.receiver.is_some() {
            return Err(ActorRegistryError::ReceiverAlreadyInstalled(starting.actor));
        }
        actor.receiver = Some(receiver);
        Ok(())
    }

    /// Install the cooperative shutdown hook before readiness publication.
    pub(crate) fn install_starting_shutdown(
        &self,
        starting: &StartingActor,
        shutdown: RootCustody,
    ) -> Result<(), ActorRegistryError> {
        self.validate_starting(starting)?;
        let mut state = self.inner.state.lock();
        let actor = entry_mut(&mut state, starting.actor)?;
        if actor.lifecycle != ActorLifecycle::Initializing {
            return Err(match actor.lifecycle {
                ActorLifecycle::Ready => ActorRegistryError::AlreadyReady(starting.actor),
                ActorLifecycle::Exited => ActorRegistryError::Exited(starting.actor),
                ActorLifecycle::Initializing => unreachable!(),
            });
        }
        if actor.shutdown.is_some() {
            return Err(ActorRegistryError::ShutdownAlreadyInstalled(starting.actor));
        }
        actor.shutdown = Some(shutdown);
        Ok(())
    }

    pub(crate) fn take_receiver(
        &self,
        actor: ActorRef,
    ) -> Result<InstalledReceiver, ActorRegistryError> {
        entry_mut(&mut self.inner.state.lock(), actor)?
            .receiver
            .take()
            .ok_or(ActorRegistryError::ReceiverMissing(actor))
    }

    /// Commit the stable receiver reached by an authored Haskell turn and
    /// release that exact admission in the same registry critical section.
    pub(crate) fn install_resident_receiver(
        &self,
        mut turn: TurnLease,
        receiver: InstalledReceiver,
    ) -> Result<(), ActorRegistryError> {
        let actor_ref = turn.actor;
        if turn.kind != ActorTurnKind::Haskell {
            return Err(ActorRegistryError::TurnTransitionMismatch {
                actor: actor_ref,
                expected: ActorTurnKind::Haskell,
                active: Some(turn.kind),
            });
        }
        let mut state = self.inner.state.lock();
        let actor = entry_mut(&mut state, actor_ref)?;
        if actor.active_turn != Some(ActorTurnKind::Haskell) {
            return Err(ActorRegistryError::TurnTransitionMismatch {
                actor: actor_ref,
                expected: ActorTurnKind::Haskell,
                active: actor.active_turn,
            });
        }
        if actor.receiver.is_some() {
            return Err(ActorRegistryError::ReceiverAlreadyInstalled(actor_ref));
        }
        actor.receiver = Some(receiver);
        actor.active_turn = None;
        turn.released = true;
        if !actor.mailbox.is_empty() {
            wake(
                &mut state,
                ActorRuntimeWake::MailboxReady { actor: actor_ref },
            );
        }
        Ok(())
    }

    /// Atomically publish a call reply (when present) and commit the callee's
    /// next stable state. The delivery retains its mailbox turn lease across
    /// the transition; this method is the sole resident-handler settlement
    /// boundary.
    pub(crate) fn settle_resident_delivery(
        &self,
        actor: ActorRef,
        delivery: &mut MailboxDelivery,
        reply: Option<MailboxValue>,
        next: InstalledActorState,
    ) -> Result<Option<ActorCleanupBatch>, MailboxFailure> {
        let mut state = self.inner.state.lock();
        require_ready(&state, actor)?;
        let actor_entry = entry(&state, actor)?;
        if actor_entry.active_turn != Some(ActorTurnKind::Mailbox) {
            return Err(ActorRegistryError::TurnTransitionMismatch {
                actor,
                expected: ActorTurnKind::Mailbox,
                active: actor_entry.active_turn,
            }
            .into());
        }
        if actor_entry.receiver.is_some() {
            return Err(ActorRegistryError::ReceiverAlreadyInstalled(actor).into());
        }

        match (&mut *delivery, reply) {
            (MailboxDelivery::Call(call), Some(value)) if call.target == actor => {
                match reply_call_in(&mut state, call.call, value) {
                    Ok(()) => {}
                    // The caller may cancel after dequeue while this actor is
                    // running its handler. The work still happened: release
                    // the now-undeliverable reply and commit the callee's next
                    // state rather than converting caller cancellation into
                    // callee failure.
                    Err(MailboxFailure::UnknownCall(id)) if id == call.call => {}
                    Err(error) => return Err(error),
                }
                call.settled = true;
            }
            (MailboxDelivery::Cast(cast), None) if cast._lease.actor() == actor => {}
            (MailboxDelivery::Call(_), None) => {
                return Err(MailboxFailure::SettlementShape(
                    "a call handler did not produce a reply",
                ));
            }
            (MailboxDelivery::Cast(_), Some(_)) => {
                return Err(MailboxFailure::SettlementShape(
                    "a cast handler produced a reply",
                ));
            }
            _ => {
                return Err(MailboxFailure::SettlementShape(
                    "delivery belongs to another actor",
                ));
            }
        }

        match next {
            InstalledActorState::Receiving(receiver) => {
                let actor_entry = entry_mut(&mut state, actor)?;
                actor_entry.receiver = Some(receiver);
                if !actor_entry.mailbox.is_empty() {
                    wake(&mut state, ActorRuntimeWake::MailboxReady { actor });
                }
                Ok(None)
            }
            InstalledActorState::Completed(terminal) => {
                finish_subtree_for_cleanup(&mut state, actor, terminal)
                    .map(Some)
                    .map_err(Into::into)
            }
        }
    }

    /// Terminate an actor whose startup failed before reference publication.
    /// The terminal fact remains observable in the journal, but no callable
    /// handle is returned to the starter.
    pub fn abort_start(
        &self,
        starting: StartingActor,
        terminal: ActorTerminal,
    ) -> Result<(), ActorRegistryError> {
        self.abort_start_for_cleanup(starting, terminal).map(drop)
    }

    pub(crate) fn abort_start_for_cleanup(
        &self,
        mut starting: StartingActor,
        terminal: ActorTerminal,
    ) -> Result<ActorCleanupBatch, ActorRegistryError> {
        self.validate_starting(&starting)?;
        let mut state = self.inner.state.lock();
        let actor = starting.actor;
        match entry(&state, actor)?.lifecycle {
            ActorLifecycle::Initializing => {}
            ActorLifecycle::Ready => {
                return Err(ActorRegistryError::AlreadyReady(starting.actor));
            }
            ActorLifecycle::Exited => {
                return Err(ActorRegistryError::Exited(starting.actor));
            }
        }
        let cleanup = finish_subtree_for_cleanup(&mut state, actor, terminal)?;
        starting.armed = false;
        Ok(cleanup)
    }

    /// Admit one serialized actor turn. Dropping the lease restores admission
    /// even during unwinding; machine ownership is still separately fenced by
    /// the runtime checkout API.
    pub fn begin_turn(
        &self,
        actor: ActorRef,
        kind: ActorTurnKind,
    ) -> Result<TurnLease, ActorRegistryError> {
        let mut state = self.inner.state.lock();
        let actor_entry = entry_mut(&mut state, actor)?;
        match actor_entry.lifecycle {
            ActorLifecycle::Initializing => return Err(ActorRegistryError::Initializing(actor)),
            ActorLifecycle::Exited => return Err(ActorRegistryError::Exited(actor)),
            ActorLifecycle::Ready => {}
        }
        if let Some(active) = actor_entry.active_turn {
            return Err(ActorRegistryError::Busy { actor, active });
        }
        if let Some(obligation) = actor_entry.parked {
            return Err(ActorRegistryError::Parked { actor, obligation });
        }
        let placement = actor_entry.descriptor.placement;
        let effect_policy = actor_entry.descriptor.effect_policy;
        let live_payload = actor_entry.descriptor.live_payload;
        let source_imports = actor_entry.descriptor.source_imports.clone();
        actor_entry.active_turn = Some(kind);
        Ok(TurnLease {
            actor,
            kind,
            placement,
            effect_policy,
            live_payload,
            source_imports,
            registry: Arc::downgrade(&self.inner),
            released: false,
        })
    }

    /// Admit the initializing actor's sole agent session through its private
    /// startup capability. Ordinary actor references remain unusable until
    /// readiness; this is the only pre-publication turn-admission path.
    pub(crate) fn begin_startup_agent_session(
        &self,
        starting: &StartingActor,
    ) -> Result<TurnLease, ActorRegistryError> {
        self.validate_starting(starting)?;
        let mut state = self.inner.state.lock();
        let actor = starting.actor;
        let actor_entry = entry_mut(&mut state, actor)?;
        match actor_entry.lifecycle {
            ActorLifecycle::Initializing => {}
            ActorLifecycle::Ready => return Err(ActorRegistryError::AlreadyReady(actor)),
            ActorLifecycle::Exited => return Err(ActorRegistryError::Exited(actor)),
        }
        if let Some(active) = actor_entry.active_turn {
            return Err(ActorRegistryError::Busy { actor, active });
        }
        actor_entry.active_turn = Some(ActorTurnKind::AgentSession);
        Ok(TurnLease {
            actor,
            kind: ActorTurnKind::AgentSession,
            placement: actor_entry.descriptor.placement,
            effect_policy: actor_entry.descriptor.effect_policy,
            live_payload: actor_entry.descriptor.live_payload,
            source_imports: actor_entry.descriptor.source_imports.clone(),
            registry: Arc::downgrade(&self.inner),
            released: false,
        })
    }

    /// Resolve the immutable machine/scoping context registered for one exact
    /// actor incarnation.
    pub fn session_context(
        &self,
        actor: ActorRef,
    ) -> Result<ActorSessionContext, ActorRegistryError> {
        let state = self.inner.state.lock();
        let actor_entry = entry(&state, actor)?;
        Ok(session_context_for(actor, actor_entry))
    }

    /// Return the immutable startup descriptor retained for this exact
    /// incarnation. Operational code must read effect-stack identity here,
    /// never reconstruct it from observability events.
    pub fn descriptor(&self, actor: ActorRef) -> Result<ActorDescriptor, ActorRegistryError> {
        Ok(entry(&self.inner.state.lock(), actor)?.descriptor.clone())
    }

    /// Authorize one nominal operation against the immutable profile of the
    /// exact principal currently entering the effect machine.
    ///
    /// Initializing actors are admitted because their authored initialization
    /// runs in the selected profile before reference publication. Unknown,
    /// stale, and exited principals never fall through to a concrete handler.
    pub fn authorize_effect(
        &self,
        principal: PrincipalId,
        operation: ActorOperationClass,
    ) -> Result<(), crate::ActorEffectRefusal> {
        if principal == PrincipalId::SYSTEM {
            return Err(crate::ActorEffectRefusal::SystemPrincipal);
        }
        let actor = ActorRef {
            id: ActorId(principal.identity),
            incarnation: crate::Incarnation(principal.incarnation),
        };
        let state = self.inner.state.lock();
        let Some(actor_entry) = state.actors.get(&actor.id) else {
            return Err(crate::ActorEffectRefusal::Unknown { principal });
        };
        if actor_entry.reference != actor {
            return Err(crate::ActorEffectRefusal::Stale {
                given: actor,
                current: actor_entry.reference,
            });
        }
        if actor_entry.lifecycle == ActorLifecycle::Exited {
            return Err(crate::ActorEffectRefusal::Exited { actor });
        }
        let profile = actor_entry.descriptor.profile;
        let allowed = match (profile, operation) {
            (ActorEffectProfile::ReadWrite, _) | (_, ActorOperationClass::FsRead) => true,
            (ActorEffectProfile::ReadOnly, ActorOperationClass::FsWrite) => false,
        };
        allowed
            .then_some(())
            .ok_or(crate::ActorEffectRefusal::ProfileDenied {
                actor,
                profile,
                operation,
            })
    }

    pub(crate) fn attach_agent_session(
        &self,
        actor: ActorRef,
    ) -> Result<Arc<Mutex<AgentSessionState>>, ActorRegistryError> {
        let mut state = self.inner.state.lock();
        let actor_entry = entry_mut(&mut state, actor)?;
        if actor_entry.lifecycle == ActorLifecycle::Exited {
            return Err(ActorRegistryError::Exited(actor));
        }
        Ok(Arc::clone(actor_entry.agent_session.get_or_insert_with(
            || Arc::new(Mutex::new(AgentSessionState::new())),
        )))
    }

    /// Accept a one-way message into the exact target incarnation's mailbox.
    /// Success means ownership has transferred to the mailbox.
    pub fn cast(
        &self,
        caller: ActorRef,
        target: ActorRef,
        value: MailboxValue,
    ) -> Result<MessageId, MailboxFailure> {
        let id = MessageId(self.inner.message_ids.next_raw());
        let mut state = self.inner.state.lock();
        validate_delivery(&state, caller, target, &value)?;
        let target_entry = entry_mut(&mut state, target)?;
        let mailbox_was_empty = target_entry.mailbox.is_empty();
        target_entry.mailbox.push_back(QueuedMessage {
            id,
            sender: caller,
            value,
            call: None,
        });
        record(
            &mut state,
            target,
            EventCausality {
                owner: Some(caller),
                operation: Some(format!("message:{}", id.0)),
                ..EventCausality::default()
            },
            ActorEvent::MailboxAccepted {
                message: id,
                sender: caller,
                kind: MailboxMessageKind::Cast,
            },
        )?;
        if mailbox_was_empty {
            wake(&mut state, ActorRuntimeWake::MailboxReady { actor: target });
        }
        Ok(id)
    }

    /// Accept one synchronous request and create its single reply obligation.
    pub fn call(
        &self,
        caller: ActorRef,
        target: ActorRef,
        value: MailboxValue,
    ) -> Result<CallTicket, MailboxFailure> {
        let id = MessageId(self.inner.message_ids.next_raw());
        let call = CallId(self.inner.call_ids.next_raw());
        let mut state = self.inner.state.lock();
        validate_delivery(&state, caller, target, &value)?;
        if let Some(obligation) = entry(&state, caller)?.parked {
            return Err(ActorRegistryError::Parked {
                actor: caller,
                obligation,
            }
            .into());
        }
        if creates_obligation_cycle(&state, caller, target) {
            return Err(MailboxFailure::CallCycle { caller, target });
        }
        state.calls.insert(
            call,
            CallEntry {
                caller,
                target,
                state: CallState::Queued,
            },
        );
        entry_mut(&mut state, caller)?.parked = Some(ParkedObligation::Call(call));
        let target_entry = entry_mut(&mut state, target)?;
        let mailbox_was_empty = target_entry.mailbox.is_empty();
        target_entry.mailbox.push_back(QueuedMessage {
            id,
            sender: caller,
            value,
            call: Some(call),
        });
        record(
            &mut state,
            target,
            EventCausality {
                owner: Some(caller),
                operation: Some(format!("call:{}", call.0)),
                ..EventCausality::default()
            },
            ActorEvent::MailboxAccepted {
                message: id,
                sender: caller,
                kind: MailboxMessageKind::Call,
            },
        )?;
        if mailbox_was_empty {
            wake(&mut state, ActorRuntimeWake::MailboxReady { actor: target });
        }
        Ok(CallTicket {
            id: call,
            caller,
            target,
            registry: self.clone(),
            settled: false,
        })
    }

    /// Transfer the oldest accepted message to its target actor.
    pub fn dequeue(&self, target: ActorRef) -> Result<Option<MailboxDelivery>, MailboxFailure> {
        let lease = self.begin_turn(target, ActorTurnKind::Mailbox)?;
        let mut state = self.inner.state.lock();
        let Some(message) = entry_mut(&mut state, target)?.mailbox.pop_front() else {
            return Ok(None);
        };
        record(
            &mut state,
            target,
            EventCausality {
                owner: Some(message.sender),
                operation: message.call.map(|call| format!("call:{}", call.0)),
                ..EventCausality::default()
            },
            ActorEvent::MailboxDequeued {
                message: message.id,
            },
        )?;
        let Some(call) = message.call else {
            return Ok(Some(MailboxDelivery::Cast(CastDelivery {
                id: message.id,
                sender: message.sender,
                value: Some(message.value),
                _lease: lease,
            })));
        };
        let Some(entry) = state.calls.get_mut(&call) else {
            return Err(MailboxFailure::UnknownCall(call));
        };
        entry.state = CallState::Delivered;
        Ok(Some(MailboxDelivery::Call(CallDelivery {
            registry: self.clone(),
            id: message.id,
            call,
            caller: message.sender,
            target,
            value: Some(message.value),
            _lease: lease,
            settled: false,
        })))
    }

    /// Poll and consume a settled synchronous call. Pending calls retain their
    /// ticket; replies and failures settle exactly once.
    pub(crate) fn poll_call(&self, ticket: &mut CallTicket) -> Result<CallStatus, MailboxFailure> {
        let mut state = self.inner.state.lock();
        let Some(call) = state.calls.get(&ticket.id) else {
            return Err(MailboxFailure::UnknownCall(ticket.id));
        };
        if call.caller != ticket.caller || call.target != ticket.target {
            return Err(MailboxFailure::UnknownCall(ticket.id));
        }
        match &call.state {
            CallState::Queued | CallState::Delivered => Ok(CallStatus::Pending),
            CallState::Replied(_) | CallState::Failed(_) => {
                let call = state
                    .calls
                    .remove(&ticket.id)
                    .ok_or(MailboxFailure::UnknownCall(ticket.id))?;
                clear_parked(&mut state, ticket.caller, ParkedObligation::Call(ticket.id));
                ticket.settled = true;
                match call.state {
                    CallState::Replied(value) => Ok(CallStatus::Reply(value)),
                    CallState::Failed(failure) => Ok(CallStatus::Failed(failure)),
                    CallState::Queued | CallState::Delivered => unreachable!(),
                }
            }
        }
    }

    pub(crate) fn cancel_call(&self, ticket: &CallTicket) {
        let mut state = self.inner.state.lock();
        let Some(call) = state.calls.get(&ticket.id) else {
            return;
        };
        if call.caller != ticket.caller || call.target != ticket.target {
            return;
        }
        let unsettled = matches!(call.state, CallState::Queued | CallState::Delivered);
        if let Ok(target) = entry_mut(&mut state, ticket.target) {
            target
                .mailbox
                .retain(|message| message.call != Some(ticket.id));
        }
        state.calls.remove(&ticket.id);
        clear_parked(&mut state, ticket.caller, ParkedObligation::Call(ticket.id));
        if unsettled {
            let _ = record(
                &mut state,
                ticket.target,
                EventCausality {
                    owner: Some(ticket.caller),
                    operation: Some(format!("call:{}", ticket.id.0)),
                    ..EventCausality::default()
                },
                ActorEvent::CallSettled {
                    call: ticket.id,
                    disposition: CallDisposition::CallerCancelled,
                },
            );
        }
    }

    /// Nonblocking observation of one exact incarnation's retained terminal
    /// metadata. Haskell `awaitExit` parks when this returns `Pending`.
    pub fn observe_exit(&self, actor: ActorRef) -> Result<ExitObservation, ActorRegistryError> {
        let state = self.inner.state.lock();
        let entry = entry(&state, actor)?;
        Ok(match &entry.terminal {
            Some(terminal) => ExitObservation::Exited(terminal.clone()),
            None => ExitObservation::Pending,
        })
    }

    /// Register one parked wait on an exact target incarnation. Registration
    /// succeeds even after target exit so publication/termination races still
    /// observe the immutable terminal record.
    pub fn register_wait(
        &self,
        waiter: ActorRef,
        target: ActorRef,
    ) -> Result<WaitTicket, WaitError> {
        let wait = WaitId(self.inner.wait_ids.next_raw());
        let mut state = self.inner.state.lock();
        require_ready(&state, waiter)?;
        let waiter_session = entry(&state, waiter)?.descriptor.placement.session;
        let target_session = entry(&state, target)?.descriptor.placement.session;
        if waiter_session != target_session {
            return Err(WaitError::MachineBoundary {
                waiter,
                waiter_session,
                target,
                target_session,
            });
        }
        if let Some(obligation) = entry(&state, waiter)?.parked {
            return Err(ActorRegistryError::Parked {
                actor: waiter,
                obligation,
            }
            .into());
        }
        if creates_obligation_cycle(&state, waiter, target) {
            return Err(WaitError::WaitCycle { waiter, target });
        }
        state.waits.insert(wait, WaitEntry { waiter, target });
        entry_mut(&mut state, waiter)?.parked = Some(ParkedObligation::Wait(wait));
        record(
            &mut state,
            target,
            EventCausality {
                owner: Some(waiter),
                operation: Some(format!("wait:{}", wait.0)),
                ..EventCausality::default()
            },
            ActorEvent::WaitRegistered { wait, waiter },
        )?;
        if entry(&state, target)?.terminal.is_some() {
            wake(&mut state, ActorRuntimeWake::WaitReady { waiter, wait });
        }
        Ok(WaitTicket {
            id: wait,
            waiter,
            target,
            registry: self.clone(),
            settled: false,
        })
    }

    pub(crate) fn poll_wait(&self, ticket: &mut WaitTicket) -> Result<ExitObservation, WaitError> {
        let mut state = self.inner.state.lock();
        let Some(wait) = state.waits.get(&ticket.id) else {
            return Err(WaitError::UnknownWait(ticket.id));
        };
        if wait.waiter != ticket.waiter || wait.target != ticket.target {
            return Err(WaitError::UnknownWait(ticket.id));
        }
        let Some(terminal) = entry(&state, ticket.target)?.terminal.clone() else {
            return Ok(ExitObservation::Pending);
        };
        state.waits.remove(&ticket.id);
        clear_parked(&mut state, ticket.waiter, ParkedObligation::Wait(ticket.id));
        ticket.settled = true;
        record(
            &mut state,
            ticket.target,
            EventCausality {
                owner: Some(ticket.waiter),
                operation: Some(format!("wait:{}", ticket.id.0)),
                ..EventCausality::default()
            },
            ActorEvent::WaitSettled {
                wait: ticket.id,
                disposition: WaitDisposition::Observed,
            },
        )?;
        Ok(ExitObservation::Exited(terminal))
    }

    pub(crate) fn cancel_wait(&self, ticket: &WaitTicket) {
        let mut state = self.inner.state.lock();
        let Some(wait) = state.waits.get(&ticket.id) else {
            return;
        };
        if wait.waiter != ticket.waiter || wait.target != ticket.target {
            return;
        }
        state.waits.remove(&ticket.id);
        clear_parked(&mut state, ticket.waiter, ParkedObligation::Wait(ticket.id));
        let _ = record(
            &mut state,
            ticket.target,
            EventCausality {
                owner: Some(ticket.waiter),
                operation: Some(format!("wait:{}", ticket.id.0)),
                ..EventCausality::default()
            },
            ActorEvent::WaitSettled {
                wait: ticket.id,
                disposition: WaitDisposition::WaiterCancelled,
            },
        );
    }

    /// Retain a terminal outcome and recursively cancel descendants in the
    /// registry. Ordinary child failure never changes its owner.
    ///
    /// Resident execution owners must use [`crate::ResidentActorLifecycle`]
    /// so the same transition also drives machine-realm cleanup.
    pub fn finish(
        &self,
        actor: ActorRef,
        terminal: ActorTerminal,
    ) -> Result<(), ActorRegistryError> {
        self.finish_for_cleanup(actor, terminal).map(drop)
    }

    /// Linearize subtree termination and capture every resident resource
    /// context covered by that same transition. Cleanup must not discover the
    /// tree in a separate pass: a concurrently admitted child could otherwise
    /// become terminal without its realm entering the cleanup set.
    pub(crate) fn finish_for_cleanup(
        &self,
        actor: ActorRef,
        terminal: ActorTerminal,
    ) -> Result<ActorCleanupBatch, ActorRegistryError> {
        let mut state = self.inner.state.lock();
        finish_subtree_for_cleanup(&mut state, actor, terminal)
    }

    pub fn lifecycle(&self, actor: ActorRef) -> Result<ActorLifecycle, ActorRegistryError> {
        Ok(entry(&self.inner.state.lock(), actor)?.lifecycle)
    }

    pub fn children(&self, actor: ActorRef) -> Result<Vec<ActorRef>, ActorRegistryError> {
        Ok(entry(&self.inner.state.lock(), actor)?
            .children
            .iter()
            .copied()
            .collect())
    }

    pub fn owner(&self, actor: ActorRef) -> Result<Option<ActorRef>, ActorRegistryError> {
        Ok(entry(&self.inner.state.lock(), actor)?.owner)
    }

    pub fn events(&self) -> Vec<ActorEventRecord> {
        self.inner.state.lock().events.clone()
    }

    pub(crate) fn record_shutdown_hook_failed(
        &self,
        actor: ActorRef,
        summary: String,
    ) -> Result<(), ActorRegistryError> {
        let mut state = self.inner.state.lock();
        if entry(&state, actor)?.lifecycle != ActorLifecycle::Exited {
            return Err(ActorRegistryError::NotExited(actor));
        }
        record(
            &mut state,
            actor,
            EventCausality::default(),
            ActorEvent::ShutdownHookFailed { summary },
        )
    }

    pub(crate) fn record_event(
        &self,
        actor: ActorRef,
        causality: EventCausality,
        event: ActorEvent,
    ) -> Result<(), ActorRegistryError> {
        let mut state = self.inner.state.lock();
        let actor_entry = entry(&state, actor)?;
        match actor_entry.lifecycle {
            ActorLifecycle::Initializing
                if actor_entry.active_turn == Some(ActorTurnKind::AgentSession) => {}
            ActorLifecycle::Initializing => return Err(ActorRegistryError::Initializing(actor)),
            ActorLifecycle::Exited => return Err(ActorRegistryError::Exited(actor)),
            ActorLifecycle::Ready => {}
        }
        record(&mut state, actor, causality, event)
    }

    fn validate_starting(&self, starting: &StartingActor) -> Result<(), ActorRegistryError> {
        let Some(registry) = starting.registry.upgrade() else {
            return Err(ActorRegistryError::ForeignStartup);
        };
        if Arc::ptr_eq(&registry, &self.inner) {
            Ok(())
        } else {
            Err(ActorRegistryError::ForeignStartup)
        }
    }

    fn reply_call(&self, call: CallId, value: MailboxValue) -> Result<(), MailboxFailure> {
        let mut state = self.inner.state.lock();
        reply_call_in(&mut state, call, value)
    }

    fn abandon_call(&self, call: CallId, target: ActorRef) {
        let mut state = self.inner.state.lock();
        let Some(entry) = state.calls.get_mut(&call) else {
            return;
        };
        if entry.target != target
            || !matches!(entry.state, CallState::Queued | CallState::Delivered)
        {
            return;
        }
        let caller = entry.caller;
        entry.state = CallState::Failed(CallFailure::DeliveryAbandoned(target));
        let _ = record(
            &mut state,
            target,
            EventCausality {
                owner: Some(caller),
                operation: Some(format!("call:{}", call.0)),
                ..EventCausality::default()
            },
            ActorEvent::CallSettled {
                call,
                disposition: CallDisposition::DeliveryAbandoned,
            },
        );
        wake(&mut state, ActorRuntimeWake::CallReady { caller, call });
    }
}

impl CallDelivery {
    #[must_use]
    pub fn id(&self) -> MessageId {
        self.id
    }

    #[must_use]
    pub fn call(&self) -> CallId {
        self.call
    }

    #[must_use]
    pub(crate) fn caller(&self) -> ActorRef {
        self.caller
    }

    #[must_use = "the request root must be mounted or deliberately dropped"]
    pub fn take_value(&mut self) -> Option<MailboxValue> {
        self.value.take()
    }

    /// Settle this call with one live result. The callee must be the exact
    /// target incarnation and the result must remain on the same machine.
    pub fn reply(mut self, value: MailboxValue) -> Result<(), MailboxFailure> {
        self.registry.reply_call(self.call, value)?;
        self.settled = true;
        Ok(())
    }
}

impl CastDelivery {
    #[must_use]
    pub fn id(&self) -> MessageId {
        self.id
    }

    #[must_use]
    pub fn sender(&self) -> ActorRef {
        self.sender
    }

    /// Transfer the live request root to the handler while this delivery
    /// continues holding the target actor's mailbox turn lease.
    #[must_use = "the request root must be mounted or deliberately dropped"]
    pub fn take_value(&mut self) -> Option<MailboxValue> {
        self.value.take()
    }
}

impl Drop for CallDelivery {
    fn drop(&mut self) {
        if !self.settled {
            self.registry.abandon_call(self.call, self.target);
        }
    }
}

impl Default for ActorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for StartingActor {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let mut state = registry.state.lock();
        if matches!(
            entry(&state, self.actor).map(|entry| entry.lifecycle),
            Ok(ActorLifecycle::Initializing)
        ) {
            let _ = exit_subtree(
                &mut state,
                self.actor,
                ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "startup capability dropped before readiness".into(),
                },
            );
        }
    }
}

/// Releases actor turn admission on drop.
pub struct TurnLease {
    actor: ActorRef,
    kind: ActorTurnKind,
    placement: ActorPlacement,
    effect_policy: EffectRunPolicy,
    live_payload: LivePayloadPolicy,
    source_imports: ActorSourceImports,
    registry: Weak<RegistryInner>,
    released: bool,
}

impl TurnLease {
    #[must_use]
    pub fn actor(&self) -> ActorRef {
        self.actor
    }

    #[must_use]
    pub fn kind(&self) -> ActorTurnKind {
        self.kind
    }

    #[must_use]
    pub fn session_context(&self) -> ActorSessionContext {
        ActorSessionContext {
            actor: self.actor,
            placement: self.placement,
            effect_policy: self.effect_policy,
            live_payload: self.live_payload,
            source_imports: self.source_imports.clone(),
        }
    }

    pub fn release(mut self) {
        self.release_inner();
    }

    /// Atomically change the phase of one already-admitted logical actor
    /// turn. This is the only path from an authored Haskell suspension into
    /// its resident agent session: admission is never dropped between the two
    /// phases, so another mailbox or lifecycle turn cannot enter the gap.
    pub(crate) fn transition(mut self, next: ActorTurnKind) -> Result<Self, ActorRegistryError> {
        let Some(registry) = self.registry.upgrade() else {
            return Err(ActorRegistryError::Unknown(self.actor));
        };
        let mut state = registry.state.lock();
        let actor = entry_mut(&mut state, self.actor)?;
        if actor.active_turn != Some(self.kind) {
            return Err(ActorRegistryError::TurnTransitionMismatch {
                actor: self.actor,
                expected: self.kind,
                active: actor.active_turn,
            });
        }
        actor.active_turn = Some(next);
        self.kind = next;
        drop(state);
        Ok(self)
    }

    fn release_inner(&mut self) {
        if self.released {
            return;
        }
        if let Some(registry) = self.registry.upgrade() {
            let mut state = registry.state.lock();
            if let Ok(actor) = entry_mut(&mut state, self.actor) {
                if actor.active_turn == Some(self.kind) {
                    actor.active_turn = None;
                }
            }
        }
        self.released = true;
    }
}

impl Drop for TurnLease {
    fn drop(&mut self) {
        self.release_inner();
    }
}

fn require_ready(state: &RegistryState, actor: ActorRef) -> Result<(), ActorRegistryError> {
    match entry(state, actor)?.lifecycle {
        ActorLifecycle::Initializing => Err(ActorRegistryError::Initializing(actor)),
        ActorLifecycle::Exited => Err(ActorRegistryError::Exited(actor)),
        ActorLifecycle::Ready => Ok(()),
    }
}

fn validate_delivery(
    state: &RegistryState,
    caller: ActorRef,
    target: ActorRef,
    value: &MailboxValue,
) -> Result<(), MailboxFailure> {
    require_ready(state, caller)?;
    require_ready(state, target)?;
    let caller_session = entry(state, caller)?.descriptor.placement.session;
    let target_session = entry(state, target)?.descriptor.placement.session;
    if caller_session != target_session {
        return Err(MailboxFailure::ActorMachineBoundary {
            caller,
            caller_session,
            target,
            target_session,
        });
    }
    if value.session() != target_session {
        return Err(MailboxFailure::MachineBoundary {
            actor: target,
            actor_session: target_session,
            value: value.session(),
        });
    }
    Ok(())
}

fn creates_obligation_cycle(state: &RegistryState, caller: ActorRef, target: ActorRef) -> bool {
    let mut cursor = Some(target);
    let mut visited = BTreeSet::new();
    while let Some(actor) = cursor {
        if actor == caller {
            return true;
        }
        if !visited.insert(actor) {
            return true;
        }
        cursor = entry(state, actor)
            .ok()
            .and_then(|entry| match entry.parked {
                Some(ParkedObligation::Call(call)) => state.calls.get(&call).and_then(|call| {
                    matches!(call.state, CallState::Queued | CallState::Delivered)
                        .then_some(call.target)
                }),
                Some(ParkedObligation::Wait(wait)) => {
                    state.waits.get(&wait).map(|wait| wait.target)
                }
                None => None,
            });
    }
    false
}

fn clear_parked(state: &mut RegistryState, actor: ActorRef, obligation: ParkedObligation) {
    if let Ok(entry) = entry_mut(state, actor) {
        if entry.parked == Some(obligation) {
            entry.parked = None;
        }
    }
}

fn owned_subtree_refs(
    state: &RegistryState,
    root: ActorRef,
) -> Result<Vec<ActorRef>, ActorRegistryError> {
    let mut pending = vec![root];
    let mut actors = Vec::new();
    while let Some(actor) = pending.pop() {
        let actor_entry = entry(state, actor)?;
        pending.extend(actor_entry.children.iter().copied());
        actors.push(actor);
    }
    Ok(actors)
}

fn cleanup_for(
    state: &mut RegistryState,
    actor: ActorRef,
) -> Result<ActorCleanup, ActorRegistryError> {
    let actor_entry = entry_mut(state, actor)?;
    Ok(ActorCleanup {
        actor,
        context: session_context_for(actor, actor_entry),
        terminal: actor_entry
            .terminal
            .clone()
            .ok_or(ActorRegistryError::Exited(actor))?,
        shutdown: actor_entry.shutdown.take(),
    })
}

/// Linearize one terminal subtree transition and transfer custody of every
/// resident resource covered by it. All runtime-owned termination paths use
/// this helper so a recursively exited child cannot be omitted from cleanup.
fn finish_subtree_for_cleanup(
    state: &mut RegistryState,
    actor: ActorRef,
    terminal: ActorTerminal,
) -> Result<ActorCleanupBatch, ActorRegistryError> {
    entry(state, actor)?;
    let actors = owned_subtree_refs(state, actor)?;
    exit_subtree(state, actor, terminal)?;
    actors
        .into_iter()
        .map(|actor| cleanup_for(state, actor))
        .collect::<Result<Vec<_>, _>>()
        .map(ActorCleanupBatch::new)
}

fn session_context_for(actor: ActorRef, entry: &ActorEntry) -> ActorSessionContext {
    ActorSessionContext {
        actor,
        placement: entry.descriptor.placement,
        effect_policy: entry.descriptor.effect_policy,
        live_payload: entry.descriptor.live_payload,
        source_imports: entry.descriptor.source_imports.clone(),
    }
}

fn entry(state: &RegistryState, actor: ActorRef) -> Result<&ActorEntry, ActorRegistryError> {
    let Some(found) = state.actors.get(&actor.id) else {
        return Err(ActorRegistryError::Unknown(actor));
    };
    if found.reference != actor {
        return Err(ActorRegistryError::Stale {
            given: actor,
            current: found.reference,
        });
    }
    Ok(found)
}

fn entry_mut(
    state: &mut RegistryState,
    actor: ActorRef,
) -> Result<&mut ActorEntry, ActorRegistryError> {
    let Some(found) = state.actors.get_mut(&actor.id) else {
        return Err(ActorRegistryError::Unknown(actor));
    };
    if found.reference != actor {
        return Err(ActorRegistryError::Stale {
            given: actor,
            current: found.reference,
        });
    }
    Ok(found)
}

fn record(
    state: &mut RegistryState,
    actor: ActorRef,
    causality: EventCausality,
    event: ActorEvent,
) -> Result<(), ActorRegistryError> {
    let stream_sequence = state.next_stream_sequence;
    state.next_stream_sequence += 1;
    let actor_entry = entry_mut(state, actor)?;
    let actor_sequence = actor_entry.next_event_sequence;
    actor_entry.next_event_sequence += 1;
    state.events.push(ActorEventRecord {
        stream_sequence,
        actor_sequence,
        actor,
        causality,
        event,
    });
    Ok(())
}

fn wake(state: &mut RegistryState, item: ActorRuntimeWake) {
    let disconnected = state
        .wake_sender
        .as_ref()
        .is_some_and(|sender| sender.send(item).is_err());
    if disconnected {
        state.wake_sender = None;
    }
}

fn exit_subtree(
    state: &mut RegistryState,
    actor: ActorRef,
    terminal: ActorTerminal,
) -> Result<(), ActorRegistryError> {
    if entry(state, actor)?.lifecycle == ActorLifecycle::Exited {
        return Ok(());
    }
    let was_published = entry(state, actor)?.lifecycle == ActorLifecycle::Ready;
    let children: Vec<_> = entry(state, actor)?.children.iter().copied().collect();
    settle_actor_obligations(state, actor);
    {
        let actor_entry = entry_mut(state, actor)?;
        actor_entry.lifecycle = ActorLifecycle::Exited;
        actor_entry.active_turn = None;
        actor_entry.terminal = Some(terminal.clone());
    }
    let owner_observing = owner_observes_exit(state, actor);
    record(
        state,
        actor,
        EventCausality::default(),
        ActorEvent::Exited {
            kind: terminal.kind,
            summary: terminal.summary,
            owner_observing,
        },
    )?;
    if was_published {
        wake(state, ActorRuntimeWake::ActorExited { actor });
    }
    let ready_waits: Vec<_> = state
        .waits
        .iter()
        .filter_map(|(wait, entry)| (entry.target == actor).then_some((entry.waiter, *wait)))
        .collect();
    for (waiter, wait) in ready_waits {
        wake(state, ActorRuntimeWake::WaitReady { waiter, wait });
    }
    for child in children {
        exit_subtree(
            state,
            child,
            ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: format!("owner {actor:?} exited"),
            },
        )?;
    }
    Ok(())
}

fn owner_observes_exit(state: &RegistryState, actor: ActorRef) -> bool {
    let Some(owner) = entry(state, actor).ok().and_then(|entry| entry.owner) else {
        return false;
    };
    let waiting = state
        .waits
        .values()
        .any(|wait| wait.waiter == owner && wait.target == actor);
    let calling = entry(state, owner)
        .ok()
        .and_then(|entry| match entry.parked {
            Some(ParkedObligation::Call(call)) => state.calls.get(&call),
            Some(ParkedObligation::Wait(_)) | None => None,
        })
        .is_some_and(|call| call.target == actor);
    waiting || calling
}

fn settle_actor_obligations(state: &mut RegistryState, actor: ActorRef) {
    let queued = entry_mut(state, actor)
        .map(|entry| std::mem::take(&mut entry.mailbox))
        .unwrap_or_default();
    for message in queued {
        if let Some(call) = message.call {
            settle_call_failure(
                state,
                call,
                CallFailure::TargetExited(actor),
                CallDisposition::TargetExited,
                actor,
            );
        }
        // Dropping the envelope releases an undelivered live request root.
        drop(message);
    }

    let outbound: Vec<_> = state
        .calls
        .iter()
        .filter_map(|(id, call)| {
            (call.caller == actor).then_some((
                *id,
                call.target,
                matches!(call.state, CallState::Queued | CallState::Delivered),
            ))
        })
        .collect();
    for (call, target, unsettled) in outbound {
        if let Ok(target_entry) = entry_mut(state, target) {
            target_entry
                .mailbox
                .retain(|message| message.call != Some(call));
        }
        state.calls.remove(&call);
        if unsettled {
            let _ = record(
                state,
                target,
                EventCausality {
                    owner: Some(actor),
                    operation: Some(format!("call:{}", call.0)),
                    ..EventCausality::default()
                },
                ActorEvent::CallSettled {
                    call,
                    disposition: CallDisposition::CallerExited,
                },
            );
        }
    }

    let inbound: Vec<_> = state
        .calls
        .iter()
        .filter_map(|(id, call)| {
            (call.target == actor && matches!(call.state, CallState::Queued | CallState::Delivered))
                .then_some(*id)
        })
        .collect();
    for call in inbound {
        settle_call_failure(
            state,
            call,
            CallFailure::TargetExited(actor),
            CallDisposition::TargetExited,
            actor,
        );
    }

    let waits: Vec<_> = state
        .waits
        .iter()
        .filter_map(|(id, wait)| (wait.waiter == actor).then_some((*id, wait.target)))
        .collect();
    for (wait, target) in waits {
        state.waits.remove(&wait);
        let _ = record(
            state,
            target,
            EventCausality {
                owner: Some(actor),
                operation: Some(format!("wait:{}", wait.0)),
                ..EventCausality::default()
            },
            ActorEvent::WaitSettled {
                wait,
                disposition: WaitDisposition::WaiterExited,
            },
        );
    }
}

fn settle_call_failure(
    state: &mut RegistryState,
    call: CallId,
    failure: CallFailure,
    disposition: CallDisposition,
    event_actor: ActorRef,
) {
    let Some(entry) = state.calls.get_mut(&call) else {
        return;
    };
    if !matches!(entry.state, CallState::Queued | CallState::Delivered) {
        return;
    }
    let caller = entry.caller;
    entry.state = CallState::Failed(failure);
    let _ = record(
        state,
        event_actor,
        EventCausality {
            owner: Some(caller),
            operation: Some(format!("call:{}", call.0)),
            ..EventCausality::default()
        },
        ActorEvent::CallSettled { call, disposition },
    );
    wake(state, ActorRuntimeWake::CallReady { caller, call });
}

fn reply_call_in(
    state: &mut RegistryState,
    call: CallId,
    value: MailboxValue,
) -> Result<(), MailboxFailure> {
    let Some(call_entry) = state.calls.get(&call) else {
        return Err(MailboxFailure::UnknownCall(call));
    };
    if !matches!(call_entry.state, CallState::Delivered) {
        return Err(MailboxFailure::UnknownCall(call));
    }
    let target = call_entry.target;
    let caller = call_entry.caller;
    require_ready(state, target)?;
    let target_session = entry(state, target)?.descriptor.placement.session;
    if value.session() != target_session {
        return Err(MailboxFailure::MachineBoundary {
            actor: target,
            actor_session: target_session,
            value: value.session(),
        });
    }
    state
        .calls
        .get_mut(&call)
        .ok_or(MailboxFailure::UnknownCall(call))?
        .state = CallState::Replied(value);
    record(
        state,
        target,
        EventCausality {
            owner: Some(caller),
            operation: Some(format!("call:{}", call.0)),
            ..EventCausality::default()
        },
        ActorEvent::CallSettled {
            call,
            disposition: CallDisposition::Replied,
        },
    )?;
    wake(state, ActorRuntimeWake::CallReady { caller, call });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tidepool_codegen::{scope::ScopeId, suspension::RealmId};
    use tidepool_repr::SessionId;

    fn descriptor(label: &str) -> ActorDescriptor {
        ActorDescriptor::new(
            label,
            ["Deliberate"],
            ActorPlacement {
                session: SessionId(1),
                resource_scope: RealmId::ROOT,
                lexical_scope: ScopeId::ROOT,
            },
        )
    }

    fn ready_root(registry: &ActorRegistry) -> ActorRef {
        ready_in(registry, None, "root", SessionId(1))
    }

    fn assert_wake(wakes: &mut ActorRuntimeWakes, wake: ActorRuntimeWake) {
        wakes.drain_available();
        assert!(wakes.take(wake), "missing runtime wake {wake:?}");
    }

    #[test]
    fn runtime_wake_receiver_is_unique_and_mailbox_bursts_are_level_triggered() {
        let registry = ActorRegistry::new();
        let mut wakes = registry.take_runtime_wakes().expect("claim wake stream");
        assert!(matches!(
            registry.take_runtime_wakes(),
            Err(ActorRegistryError::WakeReceiverClaimed)
        ));
        let caller = ready_root(&registry);
        let target = ready_in(&registry, None, "target", SessionId(1));
        assert_wake(&mut wakes, ActorRuntimeWake::ActorReady { actor: caller });
        assert_wake(&mut wakes, ActorRuntimeWake::ActorReady { actor: target });

        registry
            .cast(
                caller,
                target,
                MailboxValue::probe(SessionId(1), Arc::new(AtomicUsize::new(0))),
            )
            .expect("accept cast");
        registry
            .cast(
                caller,
                target,
                MailboxValue::probe(SessionId(1), Arc::new(AtomicUsize::new(0))),
            )
            .expect("accept second cast");

        assert_wake(&mut wakes, ActorRuntimeWake::MailboxReady { actor: target });
        assert!(wakes.is_empty());

        wakes.record(ActorRuntimeWake::MailboxReady { actor: target });
        wakes.record(ActorRuntimeWake::MailboxReady { actor: target });
        assert_wake(&mut wakes, ActorRuntimeWake::MailboxReady { actor: target });
        assert!(wakes.is_empty());
    }

    #[test]
    fn call_and_wait_wakes_name_the_exact_parked_obligation() {
        let registry = ActorRegistry::new();
        let mut wakes = registry.take_runtime_wakes().expect("claim wake stream");
        let caller = ready_root(&registry);
        let target = ready_in(&registry, None, "target", SessionId(1));
        assert_wake(&mut wakes, ActorRuntimeWake::ActorReady { actor: caller });
        assert_wake(&mut wakes, ActorRuntimeWake::ActorReady { actor: target });

        let call = registry
            .call(
                caller,
                target,
                MailboxValue::probe(SessionId(1), Arc::new(AtomicUsize::new(0))),
            )
            .expect("accept call");
        assert_wake(&mut wakes, ActorRuntimeWake::MailboxReady { actor: target });
        let call_id = call.id();
        let MailboxDelivery::Call(delivery) = registry
            .dequeue(target)
            .expect("dequeue call")
            .expect("call delivery")
        else {
            panic!("expected call delivery");
        };
        delivery
            .reply(MailboxValue::probe(
                SessionId(1),
                Arc::new(AtomicUsize::new(0)),
            ))
            .expect("reply");
        // Settlement may win the race with host-side parked-continuation
        // installation. Absorb it now and consume it only after unrelated
        // registry work proves it remains buffered.
        wakes.drain_available();
        drop(call);

        let waiter = ready_in(&registry, None, "waiter", SessionId(1));
        assert_wake(&mut wakes, ActorRuntimeWake::ActorReady { actor: waiter });
        let wait = registry
            .register_wait(waiter, target)
            .expect("register wait");
        assert_wake(
            &mut wakes,
            ActorRuntimeWake::CallReady {
                caller,
                call: call_id,
            },
        );
        let wait_id = wait.id();
        registry
            .finish(
                target,
                ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "done".into(),
                },
            )
            .expect("finish target");
        assert_wake(&mut wakes, ActorRuntimeWake::ActorExited { actor: target });
        assert_wake(
            &mut wakes,
            ActorRuntimeWake::WaitReady {
                waiter,
                wait: wait_id,
            },
        );
    }

    #[test]
    fn wait_registered_after_exit_is_woken_without_a_scan() {
        let registry = ActorRegistry::new();
        let mut wakes = registry.take_runtime_wakes().expect("claim wake stream");
        let waiter = ready_root(&registry);
        let target = ready_in(&registry, None, "target", SessionId(1));
        assert_wake(&mut wakes, ActorRuntimeWake::ActorReady { actor: waiter });
        assert_wake(&mut wakes, ActorRuntimeWake::ActorReady { actor: target });
        registry
            .finish(
                target,
                ActorTerminal {
                    kind: ActorExitKind::Failed,
                    summary: "failed".into(),
                },
            )
            .expect("finish target");
        assert_wake(&mut wakes, ActorRuntimeWake::ActorExited { actor: target });

        let wait = registry
            .register_wait(waiter, target)
            .expect("register retained wait");
        assert_wake(
            &mut wakes,
            ActorRuntimeWake::WaitReady {
                waiter,
                wait: wait.id(),
            },
        );
    }

    fn ready_in(
        registry: &ActorRegistry,
        owner: Option<ActorRef>,
        label: &str,
        session: SessionId,
    ) -> ActorRef {
        let starting = registry
            .begin_start(
                owner,
                ActorDescriptor::new(
                    label,
                    ["Deliberate"],
                    ActorPlacement {
                        session,
                        resource_scope: RealmId::ROOT,
                        lexical_scope: ScopeId::ROOT,
                    },
                ),
                StartInitiator::Runtime,
            )
            .expect("begin actor startup");
        registry.publish_ready(starting).expect("publish actor")
    }

    fn probe(session: SessionId) -> (MailboxValue, Arc<AtomicUsize>) {
        let dropped = Arc::new(AtomicUsize::new(0));
        (MailboxValue::probe(session, Arc::clone(&dropped)), dropped)
    }

    #[test]
    fn reference_is_published_only_after_readiness() {
        let registry = ActorRegistry::new();
        let starting = registry
            .begin_start(None, descriptor("root"), StartInitiator::Runtime)
            .expect("begin startup");
        let actor = registry.publish_ready(starting).expect("publish ready");
        assert_eq!(registry.lifecycle(actor), Ok(ActorLifecycle::Ready));
        assert!(matches!(
            registry.events().last().map(|record| &record.event),
            Some(ActorEvent::Ready)
        ));
    }

    #[test]
    fn named_profiles_attenuate_before_child_identity_allocation() {
        let registry = ActorRegistry::new();
        let read_only_start = registry
            .begin_start(
                None,
                descriptor("reader").with_profile(ActorEffectProfile::ReadOnly),
                StartInitiator::Runtime,
            )
            .expect("start read-only root");
        let reader = registry
            .publish_ready(read_only_start)
            .expect("publish reader");

        assert!(matches!(
            registry.begin_start(
                Some(reader),
                descriptor("forbidden writer").with_profile(ActorEffectProfile::ReadWrite),
                StartInitiator::Policy,
            ),
            Err(ActorRegistryError::ProfileEscalation {
                owner,
                owner_profile: ActorEffectProfile::ReadOnly,
                child_profile: ActorEffectProfile::ReadWrite,
            }) if owner == reader
        ));

        let read_only_child = registry
            .begin_start(
                Some(reader),
                descriptor("reader child").with_profile(ActorEffectProfile::ReadOnly),
                StartInitiator::Policy,
            )
            .expect("read-only may attenuate to read-only");
        assert_eq!(
            read_only_child.actor().id.0,
            reader.id.0 + 1,
            "a rejected escalation must not consume an actor identity"
        );
        let read_only_child = registry
            .publish_ready(read_only_child)
            .expect("publish reader child");
        assert_eq!(
            registry
                .descriptor(read_only_child)
                .expect("child descriptor")
                .profile(),
            ActorEffectProfile::ReadOnly
        );

        let writer_start = registry
            .begin_start(
                None,
                descriptor("writer").with_profile(ActorEffectProfile::ReadWrite),
                StartInitiator::Runtime,
            )
            .expect("start writer root");
        let writer = registry
            .publish_ready(writer_start)
            .expect("publish writer");
        for profile in [ActorEffectProfile::ReadWrite, ActorEffectProfile::ReadOnly] {
            let child = registry
                .begin_start(
                    Some(writer),
                    descriptor("permitted writer child").with_profile(profile),
                    StartInitiator::Policy,
                )
                .expect("read-write may preserve or attenuate");
            registry.publish_ready(child).expect("publish writer child");
        }
    }

    #[test]
    fn startup_descriptor_remains_authoritative_after_readiness_and_exit() {
        let registry = ActorRegistry::new();
        let expected = descriptor("reviewer");
        let starting = registry
            .begin_start(None, expected.clone(), StartInitiator::Runtime)
            .expect("begin startup");
        let actor = registry.publish_ready(starting).expect("publish ready");
        assert_eq!(registry.descriptor(actor), Ok(expected.clone()));

        registry
            .finish(
                actor,
                ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "done".into(),
                },
            )
            .expect("finish actor");
        assert_eq!(registry.descriptor(actor), Ok(expected));
    }

    #[test]
    fn one_actor_never_admits_two_turns() {
        let registry = ActorRegistry::new();
        let actor = ready_root(&registry);
        let lease = registry
            .begin_turn(actor, ActorTurnKind::AgentSession)
            .expect("first turn");
        assert_eq!(
            registry.begin_turn(actor, ActorTurnKind::Haskell).err(),
            Some(ActorRegistryError::Busy {
                actor,
                active: ActorTurnKind::AgentSession,
            })
        );
        drop(lease);
        registry
            .begin_turn(actor, ActorTurnKind::Haskell)
            .expect("turn after release");
    }

    #[test]
    fn owner_exit_cancels_subtree_but_child_exit_does_not_kill_owner() {
        let registry = ActorRegistry::new();
        let root = ready_root(&registry);
        let child_starting = registry
            .begin_start(Some(root), descriptor("child"), StartInitiator::Runtime)
            .expect("begin child");
        let child = registry
            .publish_ready(child_starting)
            .expect("publish child");

        registry
            .finish(
                child,
                ActorTerminal {
                    kind: ActorExitKind::Failed,
                    summary: "child failed".into(),
                },
            )
            .expect("finish child");
        assert_eq!(registry.lifecycle(root), Ok(ActorLifecycle::Ready));
        assert_eq!(
            registry.observe_exit(child),
            Ok(ExitObservation::Exited(ActorTerminal {
                kind: ActorExitKind::Failed,
                summary: "child failed".into(),
            }))
        );

        let sibling_starting = registry
            .begin_start(Some(root), descriptor("sibling"), StartInitiator::Runtime)
            .expect("begin sibling");
        let sibling = registry
            .publish_ready(sibling_starting)
            .expect("publish sibling");
        let cleanup = registry
            .finish_for_cleanup(
                root,
                ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "done".into(),
                },
            )
            .expect("finish root");
        assert_eq!(
            cleanup
                .actors
                .iter()
                .map(|cleanup| {
                    assert_eq!(cleanup.actor, cleanup.context.actor);
                    cleanup.actor
                })
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([root, child, sibling]),
            "the terminal transition must atomically return every owned realm, including an already-terminal child"
        );
        assert_eq!(registry.lifecycle(sibling), Ok(ActorLifecycle::Exited));
        assert_eq!(
            registry
                .observe_exit(sibling)
                .expect("retained sibling exit"),
            ExitObservation::Exited(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: format!("owner {root:?} exited"),
            })
        );
    }

    #[test]
    fn terminal_result_is_repeatable() {
        let registry = ActorRegistry::new();
        let actor = ready_root(&registry);
        let terminal = ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "completed".into(),
        };
        registry.finish(actor, terminal.clone()).expect("finish");
        assert_eq!(
            registry.observe_exit(actor),
            Ok(ExitObservation::Exited(terminal.clone()))
        );
        assert_eq!(
            registry.observe_exit(actor),
            Ok(ExitObservation::Exited(terminal))
        );
    }

    #[test]
    fn multiple_waiters_observe_one_retained_exit_and_suppress_owner_advisory() {
        let registry = ActorRegistry::new();
        let owner = ready_root(&registry);
        let observer = ready_in(&registry, None, "observer", SessionId(1));
        let child = ready_in(&registry, Some(owner), "child", SessionId(1));
        let mut owner_wait = registry.register_wait(owner, child).expect("owner wait");
        let mut observer_wait = registry
            .register_wait(observer, child)
            .expect("observer wait");

        assert!(matches!(owner_wait.poll(), Ok(ExitObservation::Pending)));
        assert!(matches!(
            registry.begin_turn(owner, ActorTurnKind::AgentSession),
            Err(ActorRegistryError::Parked {
                actor,
                obligation: ParkedObligation::Wait(wait),
            }) if actor == owner && wait == owner_wait.id()
        ));

        let terminal = ActorTerminal {
            kind: ActorExitKind::Failed,
            summary: "reviewer failed".into(),
        };
        registry
            .finish(child, terminal.clone())
            .expect("finish child");
        assert!(matches!(
            registry.events().last().map(|record| &record.event),
            Some(ActorEvent::Exited {
                owner_observing: true,
                ..
            })
        ));
        assert_eq!(
            owner_wait.poll(),
            Ok(ExitObservation::Exited(terminal.clone()))
        );
        assert_eq!(
            observer_wait.poll(),
            Ok(ExitObservation::Exited(terminal.clone()))
        );
        registry
            .begin_turn(owner, ActorTurnKind::Haskell)
            .expect("observed wait restored owner admission");
        assert_eq!(
            registry.observe_exit(child),
            Ok(ExitObservation::Exited(terminal))
        );
    }

    #[test]
    fn wait_after_exit_is_immediate_and_ticket_drop_restores_admission() {
        let registry = ActorRegistry::new();
        let waiter = ready_root(&registry);
        let target = ready_in(&registry, None, "target", SessionId(1));
        let terminal = ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: "done".into(),
        };
        registry
            .finish(target, terminal.clone())
            .expect("finish target");
        let mut completed = registry.register_wait(waiter, target).expect("late wait");
        assert_eq!(completed.poll(), Ok(ExitObservation::Exited(terminal)));

        let live = ready_in(&registry, None, "live", SessionId(1));
        let wait = registry.register_wait(waiter, live).expect("live wait");
        drop(wait);
        registry
            .begin_turn(waiter, ActorTurnKind::Haskell)
            .expect("wait cancellation restored admission");
    }

    #[test]
    fn waiter_exit_unregisters_without_consuming_the_targets_future_exit() {
        let registry = ActorRegistry::new();
        let waiter = ready_root(&registry);
        let target = ready_in(&registry, None, "target", SessionId(1));
        let mut wait = registry
            .register_wait(waiter, target)
            .expect("register wait");
        registry
            .finish(
                waiter,
                ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "waiter cancelled".into(),
                },
            )
            .expect("finish waiter");
        assert!(matches!(
            wait.poll(),
            Err(WaitError::UnknownWait(id)) if id == wait.id()
        ));
        registry
            .finish(
                target,
                ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "later".into(),
                },
            )
            .expect("finish target");
        assert!(matches!(
            registry.observe_exit(target),
            Ok(ExitObservation::Exited(_))
        ));
    }

    #[test]
    fn waits_reject_foreign_machines_and_share_the_parked_obligation_with_calls() {
        let registry = ActorRegistry::new();
        let waiter = ready_root(&registry);
        let target = ready_in(&registry, None, "target", SessionId(1));
        let foreign = ready_in(&registry, None, "foreign", SessionId(2));
        assert!(matches!(
            registry.register_wait(waiter, foreign),
            Err(WaitError::MachineBoundary { .. })
        ));

        let wait = registry
            .register_wait(waiter, target)
            .expect("register wait");
        let (request, dropped) = probe(SessionId(1));
        assert!(matches!(
            registry.call(waiter, target, request),
            Err(MailboxFailure::Registry(ActorRegistryError::Parked {
                actor,
                obligation: ParkedObligation::Wait(_),
            })) if actor == waiter
        ));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        drop(wait);

        let call = registry
            .call(waiter, target, probe(SessionId(1)).0)
            .expect("call");
        assert!(matches!(
            registry.register_wait(waiter, target),
            Err(WaitError::Registry(ActorRegistryError::Parked {
                actor,
                obligation: ParkedObligation::Call(_),
            })) if actor == waiter
        ));
        drop(call);
    }

    #[test]
    fn wait_registration_hands_off_an_active_turn_without_an_admission_gap() {
        let registry = ActorRegistry::new();
        let waiter = ready_root(&registry);
        let target = ready_in(&registry, None, "target", SessionId(1));
        let turn = registry
            .begin_turn(waiter, ActorTurnKind::Haskell)
            .expect("admit authored turn");

        let wait = registry
            .register_wait(waiter, target)
            .expect("park before releasing authored turn");
        assert!(matches!(
            registry.begin_turn(waiter, ActorTurnKind::Haskell),
            Err(ActorRegistryError::Busy {
                actor,
                active: ActorTurnKind::Haskell,
            }) if actor == waiter
        ));

        drop(turn);
        assert!(matches!(
            registry.begin_turn(waiter, ActorTurnKind::Haskell),
            Err(ActorRegistryError::Parked {
                actor,
                obligation: ParkedObligation::Wait(id),
            }) if actor == waiter && id == wait.id()
        ));
        drop(wait);
        registry
            .begin_turn(waiter, ActorTurnKind::Haskell)
            .expect("wait cancellation restores admission");
    }

    #[test]
    fn waits_and_calls_reject_cycles_across_both_obligation_kinds() {
        let registry = ActorRegistry::new();
        let first = ready_root(&registry);
        let second = ready_in(&registry, None, "second", SessionId(1));

        assert!(matches!(
            registry.register_wait(first, first),
            Err(WaitError::WaitCycle { waiter, target })
                if waiter == first && target == first
        ));

        let first_wait = registry
            .register_wait(first, second)
            .expect("first waits on second");
        assert!(matches!(
            registry.call(second, first, probe(SessionId(1)).0),
            Err(MailboxFailure::CallCycle { caller, target })
                if caller == second && target == first
        ));
        drop(first_wait);

        let second_call = registry
            .call(second, first, probe(SessionId(1)).0)
            .expect("second calls first");
        assert!(matches!(
            registry.register_wait(first, second),
            Err(WaitError::WaitCycle { waiter, target })
                if waiter == first && target == second
        ));
        drop(second_call);
    }

    #[test]
    fn casts_are_fifo_and_mailbox_owns_each_root_after_acceptance() {
        let registry = ActorRegistry::new();
        let caller = ready_root(&registry);
        let target = ready_in(&registry, Some(caller), "target", SessionId(1));
        let (first, first_dropped) = probe(SessionId(1));
        let (second, second_dropped) = probe(SessionId(1));

        let first_id = registry.cast(caller, target, first).expect("first cast");
        let second_id = registry.cast(caller, target, second).expect("second cast");
        assert_eq!(first_dropped.load(Ordering::SeqCst), 0);
        assert_eq!(second_dropped.load(Ordering::SeqCst), 0);

        let MailboxDelivery::Cast(mut delivery) = registry
            .dequeue(target)
            .expect("dequeue")
            .expect("first message")
        else {
            panic!("expected cast");
        };
        assert_eq!(delivery.id(), first_id);
        assert!(matches!(
            registry.begin_turn(target, ActorTurnKind::Haskell),
            Err(ActorRegistryError::Busy {
                actor,
                active: ActorTurnKind::Mailbox,
            }) if actor == target
        ));
        drop(delivery.take_value());
        drop(delivery);
        let MailboxDelivery::Cast(mut delivery) = registry
            .dequeue(target)
            .expect("dequeue")
            .expect("second message")
        else {
            panic!("expected cast");
        };
        assert_eq!(delivery.id(), second_id);
        drop(delivery.take_value());
        drop(delivery);
        assert_eq!(first_dropped.load(Ordering::SeqCst), 1);
        assert_eq!(second_dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn call_reply_is_linear_and_roots_move_through_the_obligation() {
        let registry = ActorRegistry::new();
        let caller = ready_root(&registry);
        let target = ready_in(&registry, Some(caller), "target", SessionId(1));
        let (request, request_dropped) = probe(SessionId(1));
        let mut ticket = registry.call(caller, target, request).expect("call");
        assert!(matches!(ticket.poll(), Ok(CallStatus::Pending)));

        let MailboxDelivery::Call(mut delivery) = registry
            .dequeue(target)
            .expect("dequeue")
            .expect("call delivery")
        else {
            panic!("expected call");
        };
        let request = delivery.take_value().expect("request root");
        drop(request);
        assert_eq!(request_dropped.load(Ordering::SeqCst), 1);

        let (reply, reply_dropped) = probe(SessionId(1));
        delivery.reply(reply).expect("reply");
        assert_eq!(reply_dropped.load(Ordering::SeqCst), 0);
        assert!(matches!(
            registry.begin_turn(caller, ActorTurnKind::Haskell),
            Err(ActorRegistryError::Parked {
                actor,
                obligation: ParkedObligation::Call(call),
            }) if actor == caller && call == ticket.id()
        ));
        let CallStatus::Reply(reply) = ticket.poll().expect("poll reply") else {
            panic!("expected reply");
        };
        drop(reply);
        assert_eq!(reply_dropped.load(Ordering::SeqCst), 1);
        assert!(matches!(
            ticket.poll(),
            Err(MailboxFailure::UnknownCall(id)) if id == ticket.id()
        ));
    }

    #[test]
    fn abandoning_a_dequeued_call_settles_failure_once() {
        let registry = ActorRegistry::new();
        let caller = ready_root(&registry);
        let target = ready_in(&registry, Some(caller), "target", SessionId(1));
        let (request, dropped) = probe(SessionId(1));
        let mut ticket = registry.call(caller, target, request).expect("call");
        let delivery = registry
            .dequeue(target)
            .expect("dequeue")
            .expect("delivery");
        drop(delivery);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(matches!(
            ticket.poll(),
            Ok(CallStatus::Failed(CallFailure::DeliveryAbandoned(actor))) if actor == target
        ));
    }

    #[test]
    fn dropping_call_ticket_cancels_queued_and_delivered_obligations() {
        let registry = ActorRegistry::new();
        let caller = ready_root(&registry);
        let target = ready_in(&registry, Some(caller), "target", SessionId(1));

        let (queued, queued_dropped) = probe(SessionId(1));
        let queued_ticket = registry.call(caller, target, queued).expect("queued call");
        drop(queued_ticket);
        assert_eq!(queued_dropped.load(Ordering::SeqCst), 1);
        assert!(registry.dequeue(target).expect("dequeue").is_none());
        registry
            .begin_turn(caller, ActorTurnKind::Haskell)
            .expect("caller released after cancellation");

        let (delivered, delivered_dropped) = probe(SessionId(1));
        let delivered_ticket = registry
            .call(caller, target, delivered)
            .expect("delivered call");
        let MailboxDelivery::Call(mut delivery) = registry
            .dequeue(target)
            .expect("dequeue")
            .expect("delivery")
        else {
            panic!("expected call");
        };
        drop(delivery.take_value());
        drop(delivered_ticket);
        let (late_reply, late_reply_dropped) = probe(SessionId(1));
        assert!(matches!(
            delivery.reply(late_reply),
            Err(MailboxFailure::UnknownCall(_))
        ));
        assert_eq!(delivered_dropped.load(Ordering::SeqCst), 1);
        assert_eq!(late_reply_dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancelled_caller_does_not_fail_a_resident_callee_that_already_ran() {
        let registry = ActorRegistry::new();
        let caller = ready_root(&registry);
        let target = ready_in(&registry, Some(caller), "target", SessionId(1));
        let ticket = registry
            .call(caller, target, probe(SessionId(1)).0)
            .expect("call");
        let mut delivery = registry
            .dequeue(target)
            .expect("dequeue")
            .expect("delivery");
        let MailboxDelivery::Call(call) = &mut delivery else {
            panic!("expected call");
        };
        drop(call.take_value());

        // Cancellation removes the call while the target still owns its
        // admitted mailbox turn, exactly the race a resident handler sees.
        drop(ticket);
        let (reply, reply_dropped) = probe(SessionId(1));
        registry
            .settle_resident_delivery(
                target,
                &mut delivery,
                Some(reply),
                InstalledActorState::Completed(ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "completed after caller cancellation".into(),
                }),
            )
            .expect("callee commits independently of caller cancellation");

        assert_eq!(reply_dropped.load(Ordering::SeqCst), 1);
        assert_eq!(registry.lifecycle(target), Ok(ActorLifecycle::Exited));
        assert!(matches!(
            registry.observe_exit(target),
            Ok(ExitObservation::Exited(ActorTerminal {
                kind: ActorExitKind::Completed,
                ..
            }))
        ));
    }

    #[test]
    fn resident_completion_transfers_cleanup_for_the_entire_owned_subtree() {
        let registry = ActorRegistry::new();
        let caller = ready_root(&registry);
        let target = ready_in(&registry, None, "target", SessionId(1));
        let child = ready_in(&registry, Some(target), "child", SessionId(1));
        let mut ticket = registry
            .call(caller, target, probe(SessionId(1)).0)
            .expect("call");
        let mut delivery = registry
            .dequeue(target)
            .expect("dequeue")
            .expect("delivery");
        let MailboxDelivery::Call(call) = &mut delivery else {
            panic!("expected call");
        };
        drop(call.take_value());

        let cleanup = registry
            .settle_resident_delivery(
                target,
                &mut delivery,
                Some(probe(SessionId(1)).0),
                InstalledActorState::Completed(ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "handler completed".into(),
                }),
            )
            .expect("settle completed resident actor")
            .expect("completed actor transfers a cleanup batch");

        assert_eq!(
            cleanup
                .actors
                .iter()
                .map(|cleanup| cleanup.actor)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([target, child]),
            "the terminal transition must transfer every recursively exited realm"
        );
        assert!(matches!(
            registry.observe_exit(child),
            Ok(ExitObservation::Exited(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                ..
            }))
        ));
        let CallStatus::Reply(reply) = ticket.poll().expect("poll reply") else {
            panic!("reply must remain observable");
        };
        drop(reply);
    }

    #[test]
    fn target_exit_before_reply_fails_a_delivered_call_and_releases_its_request() {
        let registry = ActorRegistry::new();
        let caller = ready_root(&registry);
        let target = ready_in(&registry, Some(caller), "target", SessionId(1));
        let (request, request_dropped) = probe(SessionId(1));
        let mut ticket = registry.call(caller, target, request).expect("call");
        let delivery = registry
            .dequeue(target)
            .expect("dequeue")
            .expect("delivery");

        registry
            .finish(
                target,
                ActorTerminal {
                    kind: ActorExitKind::Failed,
                    summary: "failed while handling".into(),
                },
            )
            .expect("finish target");
        assert!(matches!(
            ticket.poll(),
            Ok(CallStatus::Failed(CallFailure::TargetExited(actor))) if actor == target
        ));
        assert_eq!(
            request_dropped.load(Ordering::SeqCst),
            0,
            "the admitted delivery retains its request until its handler releases custody"
        );
        drop(delivery);
        assert_eq!(request_dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn reply_published_before_target_exit_remains_observable() {
        let registry = ActorRegistry::new();
        let caller = ready_root(&registry);
        let target = ready_in(&registry, Some(caller), "target", SessionId(1));
        let mut ticket = registry
            .call(caller, target, probe(SessionId(1)).0)
            .expect("call");
        let MailboxDelivery::Call(mut delivery) = registry
            .dequeue(target)
            .expect("dequeue")
            .expect("delivery")
        else {
            panic!("expected call");
        };
        drop(delivery.take_value());
        let (reply, reply_dropped) = probe(SessionId(1));
        delivery.reply(reply).expect("publish reply");

        registry
            .finish(
                target,
                ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "completed after replying".into(),
                },
            )
            .expect("finish target");
        let CallStatus::Reply(reply) = ticket.poll().expect("poll published reply") else {
            panic!("reply must win its earlier linearization point");
        };
        assert_eq!(reply_dropped.load(Ordering::SeqCst), 0);
        drop(reply);
        assert_eq!(reply_dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dropping_an_unconsumed_reply_releases_it_without_double_settlement() {
        let registry = ActorRegistry::new();
        let caller = ready_root(&registry);
        let target = ready_in(&registry, Some(caller), "target", SessionId(1));
        let ticket = registry
            .call(caller, target, probe(SessionId(1)).0)
            .expect("call");
        let MailboxDelivery::Call(delivery) = registry
            .dequeue(target)
            .expect("dequeue")
            .expect("delivery")
        else {
            panic!("expected call");
        };
        let call = ticket.id();
        let (reply, reply_dropped) = probe(SessionId(1));
        delivery.reply(reply).expect("reply");
        drop(ticket);
        assert_eq!(reply_dropped.load(Ordering::SeqCst), 1);
        let settlements = registry
            .events()
            .into_iter()
            .filter(|record| {
                matches!(record.event, ActorEvent::CallSettled { call: found, .. } if found == call)
            })
            .count();
        assert_eq!(settlements, 1);
    }

    #[test]
    fn target_exit_drops_queued_request_and_settles_the_exact_call() {
        let registry = ActorRegistry::new();
        let caller = ready_root(&registry);
        let target = ready_in(&registry, Some(caller), "target", SessionId(1));
        let (request, dropped) = probe(SessionId(1));
        let mut ticket = registry.call(caller, target, request).expect("call");
        registry
            .finish(
                target,
                ActorTerminal {
                    kind: ActorExitKind::Failed,
                    summary: "boom".into(),
                },
            )
            .expect("finish target");
        assert!(matches!(
            registry.events().last().map(|record| &record.event),
            Some(ActorEvent::Exited {
                owner_observing: true,
                ..
            })
        ));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(matches!(
            ticket.poll(),
            Ok(CallStatus::Failed(CallFailure::TargetExited(actor))) if actor == target
        ));
    }

    #[test]
    fn caller_exit_retracts_its_queued_request() {
        let registry = ActorRegistry::new();
        let caller = ready_root(&registry);
        let target = ready_in(&registry, None, "target", SessionId(1));
        let (request, dropped) = probe(SessionId(1));
        let mut ticket = registry.call(caller, target, request).expect("call");
        registry
            .finish(
                caller,
                ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "cancelled".into(),
                },
            )
            .expect("finish caller");
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(registry.dequeue(target).expect("dequeue").is_none());
        assert!(matches!(
            ticket.poll(),
            Err(MailboxFailure::UnknownCall(id)) if id == ticket.id()
        ));
    }

    #[test]
    fn owner_exit_recursively_settles_descendant_mailboxes_and_waits_once() {
        let registry = ActorRegistry::new();
        let owner = ready_root(&registry);
        let child = ready_in(&registry, Some(owner), "child", SessionId(1));
        let grandchild = ready_in(&registry, Some(child), "grandchild", SessionId(1));
        let observer = ready_in(&registry, None, "observer", SessionId(1));

        let (child_request, child_request_dropped) = probe(SessionId(1));
        let mut child_call = registry
            .call(observer, child, child_request)
            .expect("queue call to child");
        let child_call_id = child_call.id();
        let (grandchild_request, grandchild_request_dropped) = probe(SessionId(1));
        let grandchild_call = registry
            .call(owner, grandchild, grandchild_request)
            .expect("queue owner call to grandchild");
        let grandchild_call_id = grandchild_call.id();
        let mut grandchild_wait = registry
            .register_wait(child, grandchild)
            .expect("child waits on grandchild");
        let grandchild_wait_id = grandchild_wait.id();

        registry
            .finish(
                owner,
                ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "owner stopped".into(),
                },
            )
            .expect("finish owned subtree");

        assert_eq!(child_request_dropped.load(Ordering::SeqCst), 1);
        assert_eq!(grandchild_request_dropped.load(Ordering::SeqCst), 1);
        assert!(matches!(
            child_call.poll(),
            Ok(CallStatus::Failed(CallFailure::TargetExited(actor))) if actor == child
        ));
        assert!(matches!(
            grandchild_wait.poll(),
            Err(WaitError::UnknownWait(id)) if id == grandchild_wait_id
        ));
        drop(grandchild_call);

        let events = registry.events();
        for actor in [owner, child, grandchild] {
            assert_eq!(
                events
                    .iter()
                    .filter(|record| {
                        record.actor == actor && matches!(record.event, ActorEvent::Exited { .. })
                    })
                    .count(),
                1,
                "each descendant has one terminal linearization"
            );
        }
        for call in [child_call_id, grandchild_call_id] {
            assert_eq!(
                events
                    .iter()
                    .filter(|record| {
                        matches!(
                            record.event,
                            ActorEvent::CallSettled { call: found, .. } if found == call
                        )
                    })
                    .count(),
                1,
                "each call obligation settles once"
            );
        }
        assert_eq!(
            events
                .iter()
                .filter(|record| {
                    matches!(
                        record.event,
                        ActorEvent::WaitSettled { wait, .. } if wait == grandchild_wait_id
                    )
                })
                .count(),
            1,
            "the descendant wait unregisters once"
        );
    }

    #[test]
    fn stale_exact_incarnation_is_rejected_by_every_lifecycle_entry_point() {
        let registry = ActorRegistry::new();
        let actor = ready_root(&registry);
        let target = ready_in(&registry, None, "target", SessionId(1));
        let stale = ActorRef {
            id: target.id,
            incarnation: crate::Incarnation(target.incarnation.0 + 1),
        };

        let (request, request_dropped) = probe(SessionId(1));
        assert!(matches!(
            registry.call(actor, stale, request),
            Err(MailboxFailure::Registry(ActorRegistryError::Stale { given, current }))
                if given == stale && current == target
        ));
        assert_eq!(request_dropped.load(Ordering::SeqCst), 1);
        assert!(matches!(
            registry.register_wait(actor, stale),
            Err(WaitError::Registry(ActorRegistryError::Stale { given, current }))
                if given == stale && current == target
        ));
        assert!(matches!(
            registry.begin_turn(stale, ActorTurnKind::Haskell),
            Err(ActorRegistryError::Stale { given, current })
                if given == stale && current == target
        ));
        assert!(matches!(
            registry.finish(
                stale,
                ActorTerminal {
                    kind: ActorExitKind::Cancelled,
                    summary: "stale cancellation".into(),
                }
            ),
            Err(ActorRegistryError::Stale { given, current })
                if given == stale && current == target
        ));
        assert_eq!(registry.lifecycle(target), Ok(ActorLifecycle::Ready));
    }

    #[test]
    fn cross_machine_delivery_and_synchronous_cycles_are_rejected() {
        let registry = ActorRegistry::new();
        let a = ready_in(&registry, None, "a", SessionId(1));
        let b = ready_in(&registry, None, "b", SessionId(1));
        let c = ready_in(&registry, None, "c", SessionId(1));
        let foreign = ready_in(&registry, None, "foreign", SessionId(2));

        let (wrong_machine, dropped) = probe(SessionId(1));
        assert!(matches!(
            registry.cast(a, foreign, wrong_machine),
            Err(MailboxFailure::ActorMachineBoundary { .. })
        ));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);

        let (wrong_value, wrong_value_dropped) = probe(SessionId(2));
        assert!(matches!(
            registry.cast(a, b, wrong_value),
            Err(MailboxFailure::MachineBoundary { actor, .. }) if actor == b
        ));
        assert_eq!(wrong_value_dropped.load(Ordering::SeqCst), 1);

        let stale = ActorRef {
            id: b.id,
            incarnation: crate::Incarnation(2),
        };
        let (stale_value, stale_value_dropped) = probe(SessionId(1));
        assert!(matches!(
            registry.cast(a, stale, stale_value),
            Err(MailboxFailure::Registry(ActorRegistryError::Stale { given, .. }))
                if given == stale
        ));
        assert_eq!(stale_value_dropped.load(Ordering::SeqCst), 1);
        assert!(matches!(
            registry.observe_exit(stale),
            Err(ActorRegistryError::Stale { given, .. }) if given == stale
        ));

        let _a_to_b = registry.call(a, b, probe(SessionId(1)).0).expect("a -> b");
        let _b_to_c = registry.call(b, c, probe(SessionId(1)).0).expect("b -> c");
        let (cycle_value, cycle_dropped) = probe(SessionId(1));
        assert!(matches!(
            registry.call(c, a, cycle_value),
            Err(MailboxFailure::CallCycle { caller, target }) if caller == c && target == a
        ));
        assert_eq!(cycle_dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_start_never_publishes_a_callable_reference() {
        let registry = ActorRegistry::new();
        let starting = registry
            .begin_start(None, descriptor("broken"), StartInitiator::Runtime)
            .expect("begin startup");
        registry
            .abort_start(
                starting,
                ActorTerminal {
                    kind: ActorExitKind::Failed,
                    summary: "initializer failed".into(),
                },
            )
            .expect("abort startup");
        assert!(matches!(
            registry.events().last().map(|record| &record.event),
            Some(ActorEvent::Exited {
                kind: ActorExitKind::Failed,
                ..
            })
        ));
    }

    #[test]
    fn unpublished_exit_is_journaled_without_an_unroutable_runtime_wake() {
        let registry = ActorRegistry::new();
        let mut wakes = registry.take_runtime_wakes().expect("claim runtime wakes");
        let starting = registry
            .begin_start(None, descriptor("broken"), StartInitiator::Runtime)
            .expect("begin startup");
        registry
            .abort_start(
                starting,
                ActorTerminal {
                    kind: ActorExitKind::Failed,
                    summary: "initializer failed".into(),
                },
            )
            .expect("abort startup");

        wakes.drain_available();
        assert!(wakes.is_empty());
        assert!(registry
            .events()
            .iter()
            .any(|record| matches!(record.event, ActorEvent::Exited { .. })));
    }

    #[test]
    fn dropped_startup_capability_cannot_leak_initializing_actor() {
        let registry = ActorRegistry::new();
        let starting = registry
            .begin_start(None, descriptor("abandoned"), StartInitiator::Runtime)
            .expect("begin startup");
        drop(starting);
        assert!(matches!(
            registry.events().last().map(|record| &record.event),
            Some(ActorEvent::Exited {
                kind: ActorExitKind::Cancelled,
                summary,
                ..
            }) if summary.contains("startup capability dropped")
        ));
    }
}
