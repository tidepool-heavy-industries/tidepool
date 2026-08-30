use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use tidepool_repr::MonotonicIdIssuer;

use crate::agent_session::AgentSessionState;
use crate::{
    ActorAgentSession, ActorEvent, ActorEventRecord, ActorExitKind, ActorId, ActorPlacement,
    ActorRef, ActorSessionContext, CallDisposition, CallFailure, CallId, CallStatus, CallTicket,
    EventCausality, ExitObservation, MailboxFailure, MailboxMessageKind, MailboxValue, MessageId,
    ParkedObligation, StartInitiator, WaitDisposition, WaitError, WaitId, WaitTicket,
};

/// Immutable attributes selected before an actor begins initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorDescriptor {
    pub label: String,
    pub effect_stack: Vec<String>,
    pub placement: ActorPlacement,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorLifecycle {
    Initializing,
    Ready,
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorTerminal {
    pub kind: ActorExitKind,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorTurnKind {
    Haskell,
    Provider,
    Advisory,
    Mailbox,
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
}

struct ActorEntry {
    reference: ActorRef,
    placement: ActorPlacement,
    owner: Option<ActorRef>,
    children: BTreeSet<ActorRef>,
    lifecycle: ActorLifecycle,
    active_turn: Option<ActorTurnKind>,
    mailbox: VecDeque<QueuedMessage>,
    parked: Option<ParkedObligation>,
    terminal: Option<ActorTerminal>,
    agent_session: Option<Arc<Mutex<AgentSessionState>>>,
    next_event_sequence: u64,
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

    /// Allocate an initializing actor. The returned token is deliberately not
    /// an `ActorRef`; callers publish a usable reference only after authored
    /// startup and installation have completed.
    pub fn begin_start(
        &self,
        owner: Option<ActorRef>,
        descriptor: ActorDescriptor,
        initiator: StartInitiator,
    ) -> Result<StartingActor, ActorRegistryError> {
        let reference = ActorRef::first(ActorId(self.inner.ids.next_raw()));
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
        }

        state.actors.insert(
            reference.id,
            ActorEntry {
                reference,
                placement: descriptor.placement,
                owner,
                children: BTreeSet::new(),
                lifecycle: ActorLifecycle::Initializing,
                active_turn: None,
                mailbox: VecDeque::new(),
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
            ActorEvent::Created {
                owner,
                label: descriptor.label,
                effect_stack: descriptor.effect_stack,
            },
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
        self.validate_starting(&starting)?;
        let mut state = self.inner.state.lock();
        let actor = starting.actor;
        match entry(&state, actor)?.lifecycle {
            ActorLifecycle::Initializing => {}
            ActorLifecycle::Ready => return Err(ActorRegistryError::AlreadyReady(actor)),
            ActorLifecycle::Exited => return Err(ActorRegistryError::Exited(actor)),
        }
        entry_mut(&mut state, actor)?.lifecycle = ActorLifecycle::Ready;
        record(
            &mut state,
            actor,
            EventCausality::default(),
            ActorEvent::Ready,
        )?;
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

    /// Terminate an actor whose startup failed before reference publication.
    /// The terminal fact remains observable in the journal, but no callable
    /// handle is returned to the starter.
    pub fn abort_start(
        &self,
        mut starting: StartingActor,
        terminal: ActorTerminal,
    ) -> Result<(), ActorRegistryError> {
        self.validate_starting(&starting)?;
        let mut state = self.inner.state.lock();
        let result = match entry(&state, starting.actor)?.lifecycle {
            ActorLifecycle::Initializing => exit_subtree(&mut state, starting.actor, terminal),
            ActorLifecycle::Ready => Err(ActorRegistryError::AlreadyReady(starting.actor)),
            ActorLifecycle::Exited => Err(ActorRegistryError::Exited(starting.actor)),
        };
        if result.is_ok() {
            starting.armed = false;
        }
        result
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
        let placement = actor_entry.placement;
        actor_entry.active_turn = Some(kind);
        Ok(TurnLease {
            actor,
            kind,
            placement,
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
        Ok(ActorSessionContext {
            actor,
            placement: actor_entry.placement,
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
        entry_mut(&mut state, target)?
            .mailbox
            .push_back(QueuedMessage {
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
        entry_mut(&mut state, target)?
            .mailbox
            .push_back(QueuedMessage {
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
        let waiter_session = entry(&state, waiter)?.placement.session;
        let target_session = entry(&state, target)?.placement.session;
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

    /// Retain a terminal outcome and recursively cancel descendants. Ordinary
    /// child failure never changes its owner.
    pub fn finish(
        &self,
        actor: ActorRef,
        terminal: ActorTerminal,
    ) -> Result<(), ActorRegistryError> {
        let mut state = self.inner.state.lock();
        entry(&state, actor)?;
        exit_subtree(&mut state, actor, terminal)
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

    pub(crate) fn record_event(
        &self,
        actor: ActorRef,
        causality: EventCausality,
        event: ActorEvent,
    ) -> Result<(), ActorRegistryError> {
        let mut state = self.inner.state.lock();
        match entry(&state, actor)?.lifecycle {
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
        let Some(call_entry) = state.calls.get(&call) else {
            return Err(MailboxFailure::UnknownCall(call));
        };
        if !matches!(call_entry.state, CallState::Delivered) {
            return Err(MailboxFailure::UnknownCall(call));
        }
        let target = call_entry.target;
        let caller = call_entry.caller;
        require_ready(&state, target)?;
        let target_session = entry(&state, target)?.placement.session;
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
            &mut state,
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
        Ok(())
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
    registry: Weak<RegistryInner>,
    released: bool,
}

impl TurnLease {
    #[must_use]
    pub fn session_context(&self) -> ActorSessionContext {
        ActorSessionContext {
            actor: self.actor,
            placement: self.placement,
        }
    }

    pub fn release(mut self) {
        self.release_inner();
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
    let caller_session = entry(state, caller)?.placement.session;
    let target_session = entry(state, target)?.placement.session;
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

fn exit_subtree(
    state: &mut RegistryState,
    actor: ActorRef,
    terminal: ActorTerminal,
) -> Result<(), ActorRegistryError> {
    if entry(state, actor)?.lifecycle == ActorLifecycle::Exited {
        return Ok(());
    }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tidepool_codegen::{scope::ScopeId, suspension::RealmId};
    use tidepool_repr::SessionId;

    fn descriptor(label: &str) -> ActorDescriptor {
        ActorDescriptor {
            label: label.into(),
            effect_stack: vec!["Deliberate".into()],
            placement: ActorPlacement {
                session: SessionId(1),
                resource_scope: RealmId::ROOT,
                lexical_scope: ScopeId::ROOT,
            },
        }
    }

    fn ready_root(registry: &ActorRegistry) -> ActorRef {
        ready_in(registry, None, "root", SessionId(1))
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
                ActorDescriptor {
                    placement: ActorPlacement {
                        session,
                        ..descriptor(label).placement
                    },
                    ..descriptor(label)
                },
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
    fn one_actor_never_admits_two_turns() {
        let registry = ActorRegistry::new();
        let actor = ready_root(&registry);
        let lease = registry
            .begin_turn(actor, ActorTurnKind::Provider)
            .expect("first turn");
        assert_eq!(
            registry.begin_turn(actor, ActorTurnKind::Haskell).err(),
            Some(ActorRegistryError::Busy {
                actor,
                active: ActorTurnKind::Provider,
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
        registry
            .finish(
                root,
                ActorTerminal {
                    kind: ActorExitKind::Completed,
                    summary: "done".into(),
                },
            )
            .expect("finish root");
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
            registry.begin_turn(owner, ActorTurnKind::Provider),
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
