//! One run owns actor membership, identity claims and preparation admission.
use super::*;
use tokio::sync::oneshot;
mod preparation;
use preparation::{PreparationMessage, PreparationSupervisor, RootDelivery};
pub(crate) use preparation::{
    RootAdmissionOutcome, RootOperation, RootOperationResult, RootPreparationTicket,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirectoryError {
    #[error("run control is unavailable")]
    Unavailable,
    #[error("actor forest admission is closed")]
    Closed,
    #[error("logical actor {actor:?} is already claimed")]
    IdentityConflict { actor: crate::ActorId },
    #[error("directory operation does not own actor {actor:?}")]
    Unauthorized { actor: ActorRef },
    #[error("actor {actor:?} is already registered by another executor")]
    AlreadyRegistered { actor: ActorRef },
}

/// Registration and preparation share this phase, rather than separate gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunPhase {
    Running,
    RetiringPreparations,
    RetiringResources,
    Retired,
}

pub(super) struct RunState {
    phase: RunPhase,
    entries: HashMap<ActorRef, DirectoryEntry>,
    sessions: HashMap<ActorRef, crate::ActorSessionContext>,
    identities: DirectoryIdentities,
    resources: HashMap<ractor::ActorId, ResourceChild>,
    preparation_supervisor: Option<RactorRef<PreparationMessage>>,
    preparations: HashMap<u64, RootPreparationRecord>,
    next_preparation: u64,
    preparation_waiters: Vec<oneshot::Sender<crate::CleanupComponentOutcome>>,
    reservations: HashMap<ActorRef, ReservationRecord>,
}
impl Default for RunState {
    fn default() -> Self {
        Self {
            phase: RunPhase::Running,
            entries: HashMap::new(),
            sessions: HashMap::new(),
            identities: DirectoryIdentities::default(),
            resources: HashMap::new(),
            preparation_supervisor: None,
            preparations: HashMap::new(),
            next_preparation: 0,
            preparation_waiters: Vec::new(),
            reservations: HashMap::new(),
        }
    }
}

pub(crate) struct ActorReservation {
    identity: ActorRef,
    lifecycle: crate::kernel::LifecycleHandle,
    terminal: RetainedActorExit,
    owner: RactorRef<RunControlMessage>,
    armed: bool,
}
impl ActorReservation {
    pub(crate) fn identity(&self) -> ActorRef {
        self.identity
    }
    pub(crate) fn lifecycle(&self) -> &crate::kernel::LifecycleHandle {
        &self.lifecycle
    }
    pub(crate) fn terminal(&self) -> &RetainedActorExit {
        &self.terminal
    }
}
impl Drop for ActorReservation {
    fn drop(&mut self) {
        if self.armed {
            self.terminal.request_shutdown(ActorTerminal::new(
                ActorExitKind::Cancelled,
                "actor reservation abandoned",
            ));
            self.lifecycle.close_abandoned();
            let _ = self
                .owner
                .send_message(RunControlMessage::AbandonReservation {
                    actor: self.identity,
                });
        }
    }
}
enum ReservationPhase {
    Creating,
    Ready,
    Registered,
    Abandoned,
}
struct ReservationRecord {
    phase: ReservationPhase,
    terminal: RetainedActorExit,
    lifecycle: Option<crate::kernel::LifecycleHandle>,
    reply: Option<oneshot::Sender<Result<ActorReservation, DirectoryError>>>,
}

enum PreparationPhase {
    Running,
    Accepted,
    Settled(crate::CleanupComponentOutcome),
}
struct RootPreparationRecord {
    phase: PreparationPhase,
    compiler: RetainedActorExit,
    placement: Option<crate::ActorPlacement>,
}
impl RunState {
    fn settle_preparation_waiters(&mut self) {
        if self
            .preparations
            .values()
            .any(|record| !matches!(record.phase, PreparationPhase::Settled(_)))
        {
            return;
        }
        let failures = self
            .preparations
            .values()
            .filter_map(|record| match &record.phase {
                PreparationPhase::Settled(outcome)
                    if outcome != &crate::CleanupComponentOutcome::Confirmed =>
                {
                    Some(outcome.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let outcome = if failures.is_empty() {
            crate::CleanupComponentOutcome::Confirmed
        } else {
            crate::CleanupComponentOutcome::Unconfirmed(format!(
                "root preparation cleanup: {failures:?}"
            ))
        };
        for waiter in self.preparation_waiters.drain(..) {
            let _ = waiter.send(outcome.clone());
        }
    }
}

pub(super) enum RunControlMessage {
    ReservationReady {
        actor: ActorRef,
        lifecycle: Result<crate::kernel::LifecycleHandle, String>,
    },
    AbandonReservation {
        actor: ActorRef,
    },
    AttachPreparationSupervisor {
        supervisor: RactorRef<PreparationMessage>,
        reply: oneshot::Sender<()>,
    },
    BeginPreparation {
        operation: RootOperation,
        reply: oneshot::Sender<Result<oneshot::Receiver<RootDelivery>, DirectoryError>>,
    },
    AcceptPreparation {
        id: u64,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    PlacementPrepared {
        id: u64,
        placement: crate::ActorPlacement,
    },
    PlacementReleased {
        id: u64,
        placement: crate::ActorPlacement,
    },
    PreparationSettled {
        id: u64,
        outcome: crate::CleanupComponentOutcome,
    },
    ObservePreparations {
        reply: oneshot::Sender<crate::CleanupComponentOutcome>,
    },
    Reserve {
        incarnation: crate::Incarnation,
        reply: oneshot::Sender<Result<ActorReservation, DirectoryError>>,
    },
    ClaimExact {
        actor: ActorRef,
        reply: oneshot::Sender<Result<ActorReservation, DirectoryError>>,
    },
    Fence {
        actors: Vec<crate::ActorId>,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    Register {
        reservation: ActorRef,
        actor: LocalActorRef,
        context: std::sync::Weak<KernelContext>,
        ownership: SpawnOwnership,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    Resolve {
        actor: ActorRef,
        reply: oneshot::Sender<Option<LocalActorRef>>,
    },
    RuntimeIdentity {
        actor: ractor::ActorId,
        reply: oneshot::Sender<Option<ActorRef>>,
    },
    Context {
        actor: ActorRef,
        reply: oneshot::Sender<Option<Arc<KernelContext>>>,
    },
    Session {
        actor: ActorRef,
        reply: oneshot::Sender<Option<crate::ActorSessionContext>>,
    },
    InstallSession {
        context: crate::ActorSessionContext,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    Forget {
        actor: ActorRef,
        reply: oneshot::Sender<bool>,
    },
    Seal {
        reply: oneshot::Sender<SealedActorMembership>,
    },
}

fn begin_reservation(
    state: &mut RunState,
    actor: ActorRef,
    reply: oneshot::Sender<Result<ActorReservation, DirectoryError>>,
) {
    let terminal = RetainedActorExit::new();
    state.reservations.insert(
        actor,
        ReservationRecord {
            phase: ReservationPhase::Creating,
            terminal: terminal.clone(),
            lifecycle: None,
            reply: Some(reply),
        },
    );
    if state
        .preparation_supervisor
        .as_ref()
        .is_none_or(|supervisor| {
            supervisor
                .send_message(PreparationMessage::ReserveActor {
                    identity: actor,
                    terminal,
                })
                .is_err()
        })
    {
        if let Some(record) = state.reservations.get_mut(&actor) {
            record.phase = ReservationPhase::Abandoned;
            if let Some(reply) = record.reply.take() {
                let _ = reply.send(Err(DirectoryError::Unavailable));
            }
        }
    }
}

pub(super) struct RunControl;
impl Actor for RunControl {
    type Msg = RunControlMessage;
    type State = RunState;
    type Arguments = ();
    async fn pre_start(
        &self,
        _myself: RactorRef<Self::Msg>,
        (): (),
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(RunState::default())
    }
    async fn handle(
        &self,
        myself: RactorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            RunControlMessage::AttachPreparationSupervisor { supervisor, reply } => {
                state.preparation_supervisor = Some(supervisor);
                let _ = reply.send(());
            }
            RunControlMessage::BeginPreparation { operation, reply } => {
                let Some(supervisor) = state.preparation_supervisor.as_ref() else {
                    let _ = reply.send(Err(DirectoryError::Unavailable));
                    return Ok(());
                };
                if state.phase != RunPhase::Running {
                    let _ = reply.send(Err(DirectoryError::Closed));
                    return Ok(());
                }
                let id = state.next_preparation;
                state.next_preparation = state
                    .next_preparation
                    .checked_add(1)
                    .ok_or("root preparation identity exhausted")?;
                let compiler = RetainedActorExit::new();
                state.preparations.insert(
                    id,
                    RootPreparationRecord {
                        phase: PreparationPhase::Running,
                        compiler: compiler.clone(),
                        placement: None,
                    },
                );
                let (delivery, receiver) = oneshot::channel();
                if supervisor
                    .send_message(PreparationMessage::Start {
                        id,
                        operation,
                        compiler,
                        reply: delivery,
                    })
                    .is_err()
                {
                    state
                        .preparations
                        .get_mut(&id)
                        .expect("admitted preparation")
                        .phase =
                        PreparationPhase::Settled(crate::CleanupComponentOutcome::Unconfirmed(
                            "preparation supervisor unavailable".into(),
                        ));
                    let _ = reply.send(Err(DirectoryError::Unavailable));
                } else {
                    let _ = reply.send(Ok(receiver));
                }
            }
            RunControlMessage::AcceptPreparation { id, reply } => {
                let result = if state.phase != RunPhase::Running {
                    Err(DirectoryError::Closed)
                } else if let Some(record) = state.preparations.get_mut(&id) {
                    match record.phase {
                        PreparationPhase::Running => {
                            record.phase = PreparationPhase::Accepted;
                            Ok(())
                        }
                        _ => Err(DirectoryError::Unavailable),
                    }
                } else {
                    Err(DirectoryError::Unavailable)
                };
                let _ = reply.send(result);
            }
            RunControlMessage::PlacementPrepared { id, placement } => {
                if let Some(record) = state.preparations.get_mut(&id) {
                    record.placement = Some(placement);
                }
            }
            RunControlMessage::PlacementReleased { id, placement } => {
                if let Some(record) = state.preparations.get_mut(&id) {
                    if record.placement == Some(placement) {
                        record.placement = None;
                    }
                }
            }
            RunControlMessage::PreparationSettled { id, outcome } => {
                if let Some(record) = state.preparations.get_mut(&id) {
                    record.phase = PreparationPhase::Settled(outcome);
                }
                state.settle_preparation_waiters();
            }
            RunControlMessage::ObservePreparations { reply } => {
                state.preparation_waiters.push(reply);
                state.settle_preparation_waiters();
            }
            RunControlMessage::ReservationReady { actor, lifecycle } => {
                if let Some(record) = state.reservations.get_mut(&actor) {
                    match lifecycle {
                        Ok(lifecycle) => {
                            record.lifecycle = Some(lifecycle.clone());
                            if state.phase != RunPhase::Running
                                || matches!(record.phase, ReservationPhase::Abandoned)
                            {
                                lifecycle.close_abandoned();
                                record.phase = ReservationPhase::Abandoned;
                                if let Some(reply) = record.reply.take() {
                                    let _ = reply.send(Err(DirectoryError::Closed));
                                }
                            } else {
                                record.phase = ReservationPhase::Ready;
                                if let Some(reply) = record.reply.take() {
                                    let _ = reply.send(Ok(ActorReservation {
                                        identity: actor,
                                        terminal: record.terminal.clone(),
                                        lifecycle,
                                        owner: myself.clone(),
                                        armed: true,
                                    }));
                                }
                            }
                        }
                        Err(_) => {
                            record.phase = ReservationPhase::Abandoned;
                            if let Some(reply) = record.reply.take() {
                                let _ = reply.send(Err(DirectoryError::Unavailable));
                            }
                        }
                    }
                } else if let Ok(lifecycle) = lifecycle {
                    lifecycle.close_abandoned();
                }
            }
            RunControlMessage::AbandonReservation { actor } => {
                if let Some(record) = state.reservations.get_mut(&actor) {
                    record.phase = ReservationPhase::Abandoned;
                    if let Some(lifecycle) = &record.lifecycle {
                        lifecycle.close_abandoned();
                    }
                }
            }
            RunControlMessage::Reserve { incarnation, reply } => {
                if state.phase != RunPhase::Running {
                    let _ = reply.send(Err(DirectoryError::Closed));
                } else {
                    let actor = loop {
                        let id = crate::ActorId(NEXT_ACTOR_ID.fetch_add(1, Ordering::Relaxed));
                        if !state.identities.fenced.contains(&id)
                            && !state.identities.claimed.contains_key(&id)
                        {
                            break ActorRef { id, incarnation };
                        }
                    };
                    state.identities.claimed.insert(actor.id, actor);
                    begin_reservation(state, actor, reply);
                }
            }
            RunControlMessage::ClaimExact { actor, reply } => {
                if state.phase != RunPhase::Running {
                    let _ = reply.send(Err(DirectoryError::Closed));
                } else if state
                    .identities
                    .claimed
                    .get(&actor.id)
                    .is_some_and(|previous| {
                        actor.incarnation <= previous.incarnation
                            || state
                                .entries
                                .get(previous)
                                .is_none_or(|entry| entry.actor.terminal().get().is_none())
                    })
                {
                    let _ = reply.send(Err(DirectoryError::IdentityConflict { actor: actor.id }));
                } else {
                    advance_actor_ids_past(actor.id);
                    state.identities.fenced.remove(&actor.id);
                    state.identities.claimed.insert(actor.id, actor);
                    begin_reservation(state, actor, reply);
                }
            }
            RunControlMessage::Fence { actors, reply } => {
                let result = if state.phase != RunPhase::Running {
                    Err(DirectoryError::Closed)
                } else if let Some(actor) = actors
                    .iter()
                    .find(|actor| state.identities.claimed.contains_key(actor))
                {
                    Err(DirectoryError::IdentityConflict { actor: *actor })
                } else {
                    for actor in actors {
                        state.identities.fenced.insert(actor);
                        advance_actor_ids_past(actor);
                    }
                    Ok(())
                };
                let _ = reply.send(result);
            }
            RunControlMessage::Register {
                reservation,
                actor,
                context,
                ownership,
                reply,
            } => {
                let identity = actor.identity();
                let result = if state.phase != RunPhase::Running {
                    Err(DirectoryError::Closed)
                } else if reservation != identity
                    || state.reservations.get(&reservation).is_none_or(|record| {
                        !matches!(
                            record.phase,
                            ReservationPhase::Ready | ReservationPhase::Registered
                        )
                    })
                    || state.identities.claimed.get(&identity.id) != Some(&identity)
                {
                    Err(DirectoryError::Unauthorized { actor: identity })
                } else if let Some(existing) = state.entries.get(&identity) {
                    if existing.actor.address().get_id() == actor.address().get_id() {
                        Ok(())
                    } else {
                        Err(DirectoryError::AlreadyRegistered { actor: identity })
                    }
                } else {
                    state
                        .identities
                        .runtime
                        .insert(actor.address().get_id(), identity);
                    state
                        .reservations
                        .get_mut(&identity)
                        .expect("registration reservation")
                        .phase = ReservationPhase::Registered;
                    state.entries.insert(
                        identity,
                        DirectoryEntry {
                            actor,
                            context,
                            spawn_ownership: ownership,
                        },
                    );
                    Ok(())
                };
                let _ = reply.send(result);
            }
            RunControlMessage::Resolve { actor, reply } => {
                let _ = reply.send(state.entries.get(&actor).map(|entry| entry.actor.clone()));
            }
            RunControlMessage::RuntimeIdentity { actor, reply } => {
                let _ = reply.send(state.identities.runtime.get(&actor).copied());
            }
            RunControlMessage::Context { actor, reply } => {
                let _ = reply.send(
                    state
                        .entries
                        .get(&actor)
                        .and_then(|entry| entry.context.upgrade()),
                );
            }
            RunControlMessage::Session { actor, reply } => {
                let _ = reply.send(state.sessions.get(&actor).cloned());
            }
            RunControlMessage::InstallSession { context, reply } => {
                let result = if state.phase != RunPhase::Running {
                    Err(DirectoryError::Closed)
                } else if state.entries.contains_key(&context.actor) {
                    state.sessions.insert(context.actor, context);
                    Ok(())
                } else {
                    Err(DirectoryError::Unauthorized {
                        actor: context.actor,
                    })
                };
                let _ = reply.send(result);
            }
            RunControlMessage::Forget { actor, reply } => {
                let terminal = state
                    .entries
                    .get(&actor)
                    .is_some_and(|entry| entry.actor.terminal().get().is_some());
                if terminal {
                    if let Some(entry) = state.entries.remove(&actor) {
                        state
                            .identities
                            .runtime
                            .remove(&entry.actor.address().get_id());
                    }
                    state.sessions.remove(&actor);
                }
                let _ = reply.send(terminal);
            }
            RunControlMessage::Seal { reply } => {
                if state.phase == RunPhase::Running {
                    state.phase = RunPhase::RetiringPreparations;
                    for record in state.reservations.values_mut() {
                        if !matches!(record.phase, ReservationPhase::Registered) {
                            record.terminal.request_shutdown(ActorTerminal::new(
                                ActorExitKind::Cancelled,
                                "forest reservation shutdown",
                            ));
                            if let Some(lifecycle) = &record.lifecycle {
                                lifecycle.close_abandoned();
                            }
                            record.phase = ReservationPhase::Abandoned;
                            if let Some(reply) = record.reply.take() {
                                let _ = reply.send(Err(DirectoryError::Closed));
                            }
                        }
                    }
                    for (id, record) in &state.preparations {
                        if !matches!(record.phase, PreparationPhase::Settled(_)) {
                            record.compiler.request_shutdown(ActorTerminal::new(
                                ActorExitKind::Cancelled,
                                "forest preparation shutdown",
                            ));
                            if let Some(supervisor) = &state.preparation_supervisor {
                                let _ =
                                    supervisor.send_message(PreparationMessage::Cancel { id: *id });
                            }
                        }
                    }
                }
                let _ = reply.send(SealedActorMembership {
                    actors: state
                        .entries
                        .values()
                        .map(|entry| (entry.actor.clone(), entry.spawn_ownership))
                        .collect(),
                });
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
        if matches!(
            event,
            SupervisionEvent::ActorFailed(_, _) | SupervisionEvent::ActorTerminated(_, _, _)
        ) {
            for record in state.preparations.values_mut() {
                if !matches!(record.phase, PreparationPhase::Settled(_)) {
                    record.phase =
                        PreparationPhase::Settled(crate::CleanupComponentOutcome::Unconfirmed(
                            "preparation supervisor exited before root settlement".into(),
                        ));
                }
            }
            state.settle_preparation_waiters();
        }
        Ok(())
    }
}

/// Exact run-control endpoint. Startup is completed before this handle is issued.
#[derive(Clone)]
pub struct RunControlClient {
    owner: RactorRef<RunControlMessage>,
}
impl RunControlClient {
    pub async fn start() -> Result<(Self, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr> {
        let (owner, task) = Actor::spawn(None, RunControl, ()).await?;
        let (supervisor, supervisor_task) =
            Actor::spawn_linked(None, PreparationSupervisor, owner.clone(), owner.get_cell())
                .await?;
        drop(supervisor_task);
        let client = Self { owner };
        client
            .ask(|reply| RunControlMessage::AttachPreparationSupervisor { supervisor, reply })
            .await
            .map_err(|error| ractor::SpawnErr::StartupFailed(error.into()))?;
        Ok((client, task))
    }
    async fn ask<T: Send + 'static>(
        &self,
        message: impl FnOnce(oneshot::Sender<T>) -> RunControlMessage,
    ) -> Result<T, DirectoryError> {
        let (reply, receiver) = oneshot::channel();
        self.owner
            .send_message(message(reply))
            .map_err(|_| DirectoryError::Unavailable)?;
        receiver.await.map_err(|_| DirectoryError::Unavailable)
    }
    pub(crate) async fn prepare_root(
        &self,
        operation: RootOperation,
    ) -> Result<RootOperationResult, DirectoryError> {
        let receiver = self
            .ask(|reply| RunControlMessage::BeginPreparation { operation, reply })
            .await??;
        let delivery = receiver.await.map_err(|_| DirectoryError::Unavailable)?;
        self.ask(|reply| RunControlMessage::AcceptPreparation {
            id: delivery.id,
            reply,
        })
        .await??;
        Ok(delivery.accept())
    }
    pub(crate) async fn settle_preparations(
        &self,
        timeout: Duration,
    ) -> crate::CleanupComponentOutcome {
        match tokio::time::timeout(
            timeout,
            self.ask(|reply| RunControlMessage::ObservePreparations { reply }),
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => crate::CleanupComponentOutcome::Unconfirmed(error.to_string()),
            Err(_) => crate::CleanupComponentOutcome::Unconfirmed(
                "root preparation cleanup remains in flight".into(),
            ),
        }
    }
    pub(super) async fn reserve(
        &self,
        incarnation: crate::Incarnation,
    ) -> Result<ActorReservation, DirectoryError> {
        self.ask(|reply| RunControlMessage::Reserve { incarnation, reply })
            .await?
    }
    pub(super) async fn claim_exact(
        &self,
        actor: ActorRef,
    ) -> Result<ActorReservation, DirectoryError> {
        self.ask(|reply| RunControlMessage::ClaimExact { actor, reply })
            .await?
    }
    pub(crate) async fn fence_logical_ids(
        &self,
        actors: impl IntoIterator<Item = crate::ActorId>,
    ) -> Result<(), DirectoryError> {
        let actors = actors.into_iter().collect();
        self.ask(|reply| RunControlMessage::Fence { actors, reply })
            .await?
    }
    pub(super) async fn runtime_identity(
        &self,
        actor: ractor::ActorId,
    ) -> Result<Option<ActorRef>, DirectoryError> {
        self.ask(|reply| RunControlMessage::RuntimeIdentity { actor, reply })
            .await
    }
    pub async fn resolve(&self, actor: ActorRef) -> Result<Option<LocalActorRef>, DirectoryError> {
        self.ask(|reply| RunControlMessage::Resolve { actor, reply })
            .await
    }
    pub async fn session_context(
        &self,
        actor: ActorRef,
    ) -> Result<Option<crate::ActorSessionContext>, DirectoryError> {
        self.ask(|reply| RunControlMessage::Session { actor, reply })
            .await
    }
    pub(super) async fn context(
        &self,
        actor: ActorRef,
    ) -> Result<Option<Arc<KernelContext>>, DirectoryError> {
        self.ask(|reply| RunControlMessage::Context { actor, reply })
            .await
    }
    pub(super) async fn install_session_context(
        &self,
        context: crate::ActorSessionContext,
    ) -> Result<(), DirectoryError> {
        self.ask(|reply| RunControlMessage::InstallSession { context, reply })
            .await?
    }
    pub(crate) async fn register_executor(
        &self,
        mut reservation: ActorReservation,
        actor: LocalActorRef,
        context: std::sync::Weak<KernelContext>,
        ownership: SpawnOwnership,
    ) -> Result<(), DirectoryError> {
        self.ask(|reply| RunControlMessage::Register {
            reservation: reservation.identity,
            actor,
            context,
            ownership,
            reply,
        })
        .await??;
        reservation.armed = false;
        Ok(())
    }
    pub(crate) async fn seal(&self) -> Result<SealedActorMembership, DirectoryError> {
        self.ask(|reply| RunControlMessage::Seal { reply }).await
    }
    pub(super) async fn forget_terminal(&self, actor: ActorRef) -> Result<bool, DirectoryError> {
        self.ask(|reply| RunControlMessage::Forget { actor, reply })
            .await
    }
}
