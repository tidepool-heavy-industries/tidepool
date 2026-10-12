//! One run owns actor membership, identity claims and preparation admission.
use super::*;
use tokio::sync::oneshot;
#[cfg(test)]
mod cluster_tests;
mod preparation;
mod startup;
pub(crate) use startup::{
    RootStartupAdmission, RootStartupRecoveryAuthority, StartupPredecessorAuthority,
};
mod transfers;
use preparation::{PreparationMessage, PreparationSupervisor, RootDelivery};
pub(crate) use preparation::{
    RootAdmissionOutcome, RootOperation, RootOperationResult, RootPreparationTicket,
};
use transfers::{ResourceTransfer, WorkerTransfer};
pub(crate) use transfers::{ResourceTransferReservation, WorkerTransferReservation};

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
    resources: HashMap<ractor::ActorId, OwnedResource>,
    preparation_supervisor: Option<RactorRef<PreparationMessage>>,
    preparations: HashMap<u64, RootPreparationRecord>,
    next_preparation: u64,
    preparation_waiters: Vec<oneshot::Sender<crate::CleanupComponentOutcome>>,
    reservations: HashMap<ActorRef, ReservationRecord>,
    resource_retirement: ResourceRetirement,
    resource_waiters: Vec<oneshot::Sender<crate::CleanupComponentOutcome>>,
    actor_resource_retirements: HashMap<
        ActorRef,
        (
            ResourceRetirement,
            Vec<oneshot::Sender<crate::CleanupComponentOutcome>>,
        ),
    >,
    child_cleanup: HashMap<ActorRef, crate::CleanupComponentOutcome>,
    next_transfer: u64,
    worker_transfers: HashMap<u64, WorkerTransfer>,
    resource_transfers: HashMap<u64, ResourceTransfer>,
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
            resource_retirement: ResourceRetirement::Available,
            resource_waiters: Vec::new(),
            actor_resource_retirements: HashMap::new(),
            child_cleanup: HashMap::new(),
            next_transfer: 0,
            worker_transfers: HashMap::new(),
            resource_transfers: HashMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ResourceLifetime {
    Actor(ActorRef),
    Run,
}
enum ResourceOwner {
    Owned(ResourceLifetime),
    Transfer { id: u64, source: ResourceLifetime },
    Retired { owner: ResourceLifetime },
}
struct OwnedResource {
    resource: ResourceChild,
    issued_to: ActorRef,
    owner: ResourceOwner,
}

#[derive(Clone)]
pub(crate) struct ReplacementIngressGrant {
    predecessor: ActorRef,
    successor: ActorRef,
    successor_handle: crate::kernel::LifecycleHandle,
}
impl ReplacementIngressGrant {
    pub(crate) fn predecessor(&self) -> ActorRef {
        self.predecessor
    }
    pub(crate) fn successor(&self) -> ActorRef {
        self.successor
    }
    pub(crate) fn successor_handle(&self) -> &crate::kernel::LifecycleHandle {
        &self.successor_handle
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
enum ReservationPurpose {
    Root,
    Worker { parent: ActorRef },
    Replacement { predecessor: ActorRef },
}
enum ReservationPhase {
    Creating,
    Ready,
    Registered,
    Abandoned,
}
struct ReservationRecord {
    phase: ReservationPhase,
    purpose: ReservationPurpose,
    startup: Option<crate::RootStartupIntent>,
    terminal: RetainedActorExit,
    lifecycle: Option<crate::kernel::LifecycleHandle>,
    reply: Option<oneshot::Sender<Result<ActorReservation, DirectoryError>>>,
}

enum ResourceRetirement {
    Available,
    Requested,
    Running,
    Settled(crate::CleanupComponentOutcome),
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
    fn start_pending_actor_resources(&mut self) {
        let actors = self
            .actor_resource_retirements
            .iter()
            .filter_map(|(actor, (phase, _))| {
                if matches!(phase, ResourceRetirement::Requested) {
                    Some(*actor)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for actor in actors {
            self.start_actor_resource_retirement(actor);
        }
    }
    fn start_actor_resource_retirement(&mut self, actor: ActorRef) {
        if self.resource_transfers.values().any(|transfer| {
            transfer.source == ResourceLifetime::Actor(actor)
                || transfer.destination == ResourceLifetime::Actor(actor)
        }) {
            return;
        }
        let Some((phase, _)) = self.actor_resource_retirements.get_mut(&actor) else {
            return;
        };
        if !matches!(phase, ResourceRetirement::Requested) {
            return;
        }
        *phase = ResourceRetirement::Running;
        let owner = ResourceLifetime::Actor(actor);
        let resources = self
            .resources
            .iter()
            .filter_map(|(id, record)| {
                if matches!(record.owner, ResourceOwner::Owned(selected) if selected == owner) {
                    Some((*id, record.resource.clone()))
                } else {
                    None
                }
            })
            .collect();
        if self
            .preparation_supervisor
            .as_ref()
            .is_none_or(|supervisor| {
                supervisor
                    .send_message(PreparationMessage::RetireResources { owner, resources })
                    .is_err()
            })
        {
            self.finish_resources(
                owner,
                crate::CleanupComponentOutcome::Unconfirmed(
                    "actor resource supervisor unavailable".into(),
                ),
            );
        }
    }
    fn finish_resources(
        &mut self,
        owner: ResourceLifetime,
        outcome: crate::CleanupComponentOutcome,
    ) {
        match owner {
            ResourceLifetime::Run => self.finish_resource_retirement(outcome),
            ResourceLifetime::Actor(actor) => {
                if let Some((phase, waiters)) = self.actor_resource_retirements.get_mut(&actor) {
                    *phase = ResourceRetirement::Settled(outcome.clone());
                    for waiter in waiters.drain(..) {
                        let _ = waiter.send(outcome.clone());
                    }
                }
            }
        }
    }
    fn start_resource_retirement(&mut self) {
        if !self.worker_transfers.is_empty() || !self.resource_transfers.is_empty() {
            return;
        }
        if !matches!(self.resource_retirement, ResourceRetirement::Requested)
            || self
                .preparations
                .values()
                .any(|record| !matches!(record.phase, PreparationPhase::Settled(_)))
        {
            return;
        }
        self.phase = RunPhase::RetiringResources;
        self.resource_retirement = ResourceRetirement::Running;
        let resources = self
            .resources
            .iter()
            .filter_map(|(id, resource)| {
                if matches!(resource.owner, ResourceOwner::Owned(ResourceLifetime::Run)) {
                    Some((*id, resource.resource.clone()))
                } else {
                    None
                }
            })
            .collect();
        if self
            .preparation_supervisor
            .as_ref()
            .is_none_or(|supervisor| {
                supervisor
                    .send_message(PreparationMessage::RetireResources {
                        owner: ResourceLifetime::Run,
                        resources,
                    })
                    .is_err()
            })
        {
            self.finish_resource_retirement(crate::CleanupComponentOutcome::Unconfirmed(
                "run resource supervisor unavailable".into(),
            ));
        }
    }
    fn finish_resource_retirement(&mut self, outcome: crate::CleanupComponentOutcome) {
        self.resource_retirement = ResourceRetirement::Settled(outcome.clone());
        if outcome == crate::CleanupComponentOutcome::Confirmed {
            self.phase = RunPhase::Retired;
        }
        for waiter in self.resource_waiters.drain(..) {
            let _ = waiter.send(outcome.clone());
        }
    }
    fn settle_preparation_waiters(&mut self) {
        if !self.worker_transfers.is_empty() || !self.resource_transfers.is_empty() {
            return;
        }
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
    SelectStartup {
        actor: ActorRef,
        intent: crate::RootStartupIntent,
        recovery: Option<Arc<RootStartupRecoveryAuthority>>,
        reply: oneshot::Sender<Result<StartupPredecessorAuthority, DirectoryError>>,
    },
    PreparationLost {
        id: u64,
    },
    ReserveWorker {
        parent: ActorRef,
        incarnation: crate::Incarnation,
        reply: oneshot::Sender<Result<ActorReservation, DirectoryError>>,
    },
    ReserveSuccessor {
        predecessor: ActorRef,
        incarnation: crate::Incarnation,
        reply: oneshot::Sender<Result<ActorReservation, DirectoryError>>,
    },
    ReplacementIngressGrant {
        predecessor: ActorRef,
        successor: ActorRef,
        reply: oneshot::Sender<Result<ReplacementIngressGrant, DirectoryError>>,
    },
    Children {
        actor: ActorRef,
        reply: oneshot::Sender<Vec<LocalActorRef>>,
    },
    ForgetChild {
        parent: ActorRef,
        child: ActorRef,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    TransferChildren {
        predecessor: ActorRef,
        successor: ActorRef,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    RetainChildCleanup {
        actor: ActorRef,
        outcome: crate::CleanupComponentOutcome,
    },
    ChildCleanup {
        actor: ActorRef,
        reply: oneshot::Sender<crate::CleanupComponentOutcome>,
    },
    RegisterResource {
        parent: ActorRef,
        preparation: u64,
        resource: ResourceChild,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    OwnedResource {
        parent: ActorRef,
        id: ractor::ActorId,
        reply: oneshot::Sender<Option<ResourceChild>>,
    },
    ForgetResource {
        parent: ActorRef,
        id: ractor::ActorId,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    RetireActorResources {
        actor: ActorRef,
        reply: oneshot::Sender<crate::CleanupComponentOutcome>,
    },
    ReserveWorkerTransfer {
        parent: ActorRef,
        child: LocalActorRef,
        destination_run: bool,
        reply: oneshot::Sender<Result<WorkerTransferReservation, DirectoryError>>,
    },
    CommitWorkerTransfer {
        id: u64,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    AbortWorkerTransfer {
        id: u64,
    },
    ReserveResourceTransfer {
        parent: ActorRef,
        source_run: bool,
        destination_run: bool,
        cells: Vec<ractor::ActorCell>,
        reply: oneshot::Sender<Result<ResourceTransferReservation, DirectoryError>>,
    },
    CommitResourceTransfer {
        id: u64,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    AbortResourceTransfer {
        id: u64,
    },
    IndependentRoots {
        reply: oneshot::Sender<Vec<LocalActorRef>>,
    },
    RetireRunResources {
        reply: oneshot::Sender<crate::CleanupComponentOutcome>,
    },
    RunResourcesSettled {
        owner: ResourceLifetime,
        outcomes: Vec<(ractor::ActorId, crate::CleanupComponentOutcome)>,
    },
    RunResourceWorkerFailed {
        owner: ResourceLifetime,
        detail: String,
    },
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

fn reserve_actor(
    state: &mut RunState,
    parent: Option<ActorRef>,
    incarnation: crate::Incarnation,
    purpose: ReservationPurpose,
    reply: oneshot::Sender<Result<ActorReservation, DirectoryError>>,
) {
    if state.phase != RunPhase::Running {
        let _ = reply.send(Err(DirectoryError::Closed));
        return;
    }
    if let Some(parent) = parent {
        if !state.entries.contains_key(&parent) {
            let _ = reply.send(Err(DirectoryError::Unauthorized { actor: parent }));
            return;
        }
    }
    let actor = loop {
        let id = crate::ActorId(NEXT_ACTOR_ID.fetch_add(1, Ordering::Relaxed));
        if !state.identities.fenced.contains(&id) && !state.identities.claimed.contains_key(&id) {
            break ActorRef { id, incarnation };
        }
    };
    state.identities.claimed.insert(actor.id, actor);
    begin_reservation(state, actor, purpose, reply);
}

fn begin_reservation(
    state: &mut RunState,
    actor: ActorRef,
    purpose: ReservationPurpose,
    reply: oneshot::Sender<Result<ActorReservation, DirectoryError>>,
) {
    let terminal = RetainedActorExit::new();
    state.reservations.insert(
        actor,
        ReservationRecord {
            phase: ReservationPhase::Creating,
            purpose,
            startup: None,
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
            RunControlMessage::SelectStartup {
                actor,
                intent,
                recovery,
                reply,
            } => {
                let result = (|| {
                    if !matches!(state.phase, RunPhase::Running) {
                        return Err(DirectoryError::Closed);
                    }
                    let record = state
                        .reservations
                        .get(&actor)
                        .ok_or(DirectoryError::Unauthorized { actor })?;
                    if !matches!(record.phase, ReservationPhase::Ready)
                        || !matches!(record.purpose, ReservationPurpose::Root)
                        || record.startup.is_some()
                    {
                        return Err(DirectoryError::Unauthorized { actor });
                    }
                    if intent.predecessor.is_some()
                        && state.reservations.values().any(|record| {
                            record
                                .startup
                                .as_ref()
                                .is_some_and(|selected| selected.predecessor == intent.predecessor)
                        })
                    {
                        return Err(DirectoryError::Unauthorized { actor });
                    }
                    let predecessor = match intent.predecessor {
                        None => StartupPredecessorAuthority::Initial,
                        Some(previous) => match state
                            .reservations
                            .get(&previous)
                            .and_then(|record| record.lifecycle.clone())
                        {
                            Some(owner) => StartupPredecessorAuthority::Live(owner),
                            None => StartupPredecessorAuthority::Recovered(
                                recovery.ok_or(DirectoryError::Unauthorized { actor })?,
                            ),
                        },
                    };
                    state
                        .reservations
                        .get_mut(&actor)
                        .expect("validated reservation")
                        .startup = Some(intent);
                    Ok(predecessor)
                })();
                let _ = reply.send(result);
            }
            RunControlMessage::PreparationLost { id } => {
                if let Some(record) = state.preparations.get_mut(&id) {
                    record.phase = PreparationPhase::Settled(
                        crate::CleanupComponentOutcome::Unconfirmed(format!(
                            "preparation worker lost before delivery; retained placement {:?}",
                            record.placement
                        )),
                    );
                }
                state.settle_preparation_waiters();
            }
            RunControlMessage::ReserveWorker {
                parent,
                incarnation,
                reply,
            } => {
                reserve_actor(
                    state,
                    Some(parent),
                    incarnation,
                    ReservationPurpose::Worker { parent },
                    reply,
                );
            }
            RunControlMessage::ReserveSuccessor {
                predecessor,
                incarnation,
                reply,
            } => {
                reserve_actor(
                    state,
                    Some(predecessor),
                    incarnation,
                    ReservationPurpose::Replacement { predecessor },
                    reply,
                );
            }
            RunControlMessage::ReplacementIngressGrant {
                predecessor,
                successor,
                reply,
            } => {
                let result = if state.phase != RunPhase::Running {
                    Err(DirectoryError::Closed)
                } else if !state.entries.contains_key(&predecessor)
                    || !state.entries.contains_key(&successor)
                    || !state.reservations.get(&successor).is_some_and(|record| matches!(record.purpose, ReservationPurpose::Replacement { predecessor: issued } if issued == predecessor))
                {
                    Err(DirectoryError::Unauthorized { actor: successor })
                } else if let Some(record) = state.reservations.get(&successor) {
                    if matches!(record.purpose, ReservationPurpose::Replacement { predecessor: issued } if issued == predecessor)
                    {
                        match record.lifecycle.clone() {
                            Some(successor_handle) => Ok(ReplacementIngressGrant {
                                predecessor,
                                successor,
                                successor_handle,
                            }),
                            None => Err(DirectoryError::Unavailable),
                        }
                    } else {
                        Err(DirectoryError::Unauthorized { actor: successor })
                    }
                } else {
                    Err(DirectoryError::Unauthorized { actor: successor })
                };
                let _ = reply.send(result);
            }
            RunControlMessage::Children { actor, reply } => {
                let _ = reply.send(
                    state
                        .entries
                        .values()
                        .filter(|entry| entry.spawn_ownership == SpawnOwnership::Supervised(actor))
                        .map(|entry| entry.actor.clone())
                        .collect(),
                );
            }
            RunControlMessage::ForgetChild {
                parent,
                child,
                reply,
            } => {
                let result = if let Some(entry) = state.entries.get_mut(&child) {
                    if entry.spawn_ownership == SpawnOwnership::Supervised(parent) {
                        entry.spawn_ownership = SpawnOwnership::Independent;
                        Ok(())
                    } else {
                        Err(DirectoryError::Unauthorized { actor: child })
                    }
                } else {
                    Err(DirectoryError::Unauthorized { actor: child })
                };
                let _ = reply.send(result);
            }
            RunControlMessage::TransferChildren {
                predecessor,
                successor,
                reply,
            } => {
                let result = if !state.entries.contains_key(&predecessor)
                    || !state.entries.contains_key(&successor)
                {
                    Err(DirectoryError::Unauthorized { actor: successor })
                } else {
                    let source = state.entries[&predecessor].actor.address().get_cell();
                    let target = state.entries[&successor].actor.address().get_cell();
                    for entry in state.entries.values_mut() {
                        if entry.spawn_ownership == SpawnOwnership::Supervised(predecessor) {
                            entry.actor.address().get_cell().link(target.clone());
                            entry.spawn_ownership = SpawnOwnership::Supervised(successor);
                        }
                    }
                    for resource in state.resources.values_mut() {
                        if matches!(resource.owner, ResourceOwner::Owned(ResourceLifetime::Actor(owner)) if owner == predecessor)
                        {
                            resource.resource.cell.link(target.clone());
                            resource.owner =
                                ResourceOwner::Owned(ResourceLifetime::Actor(successor));
                        }
                    }
                    if let Some(outcome) = state.child_cleanup.remove(&predecessor) {
                        let prior = state
                            .child_cleanup
                            .remove(&successor)
                            .unwrap_or(crate::CleanupComponentOutcome::Confirmed);
                        state
                            .child_cleanup
                            .insert(successor, combine_cleanup(prior, outcome));
                    }
                    let _ = source;
                    Ok(())
                };
                let _ = reply.send(result);
            }
            RunControlMessage::RetainChildCleanup { actor, outcome } => {
                let prior = state
                    .child_cleanup
                    .remove(&actor)
                    .unwrap_or(crate::CleanupComponentOutcome::Confirmed);
                state
                    .child_cleanup
                    .insert(actor, combine_cleanup(prior, outcome));
            }
            RunControlMessage::ChildCleanup { actor, reply } => {
                let _ = reply.send(
                    state
                        .child_cleanup
                        .get(&actor)
                        .cloned()
                        .unwrap_or(crate::CleanupComponentOutcome::Confirmed),
                );
            }
            RunControlMessage::RegisterResource {
                parent,
                preparation,
                resource,
                reply,
            } => {
                let result = if !state.entries.contains_key(&parent)
                    || state
                        .preparations
                        .get(&preparation)
                        .is_none_or(|record| matches!(record.phase, PreparationPhase::Settled(_)))
                {
                    Err(DirectoryError::Unauthorized { actor: parent })
                } else if state.resources.contains_key(&resource.cell.get_id()) {
                    Err(DirectoryError::Unauthorized { actor: parent })
                } else {
                    state.resources.insert(
                        resource.cell.get_id(),
                        OwnedResource {
                            resource,
                            issued_to: parent,
                            owner: ResourceOwner::Owned(ResourceLifetime::Actor(parent)),
                        },
                    );
                    Ok(())
                };
                let _ = reply.send(result);
            }
            RunControlMessage::OwnedResource { parent, id, reply } => {
                let _ = reply.send(state.resources.get(&id).and_then(|owned| if matches!(owned.owner, ResourceOwner::Owned(ResourceLifetime::Actor(owner)) if owner == parent) { Some(owned.resource.clone()) } else { None }));
            }
            RunControlMessage::ForgetResource { parent, id, reply } => {
                let result = if let Some(owned) = state.resources.get_mut(&id) {
                    if matches!(owned.owner, ResourceOwner::Owned(ResourceLifetime::Actor(owner)) if owner == parent)
                    {
                        owned.owner = ResourceOwner::Retired {
                            owner: ResourceLifetime::Actor(parent),
                        };
                        Ok(())
                    } else {
                        Err(DirectoryError::Unauthorized { actor: parent })
                    }
                } else {
                    Err(DirectoryError::Unavailable)
                };
                let _ = reply.send(result);
            }
            RunControlMessage::RetireActorResources { actor, reply } => {
                let entry = state
                    .actor_resource_retirements
                    .entry(actor)
                    .or_insert_with(|| (ResourceRetirement::Requested, Vec::new()));
                if let ResourceRetirement::Settled(outcome) = &entry.0 {
                    let _ = reply.send(outcome.clone());
                } else {
                    entry.1.push(reply);
                    state.start_actor_resource_retirement(actor);
                }
            }
            RunControlMessage::ReserveWorkerTransfer {
                parent,
                child,
                destination_run,
                reply,
            } => {
                let identity = child.identity();
                let result =
                    if state.phase != RunPhase::Running {
                        Err(DirectoryError::Closed)
                    } else if state.entries.get(&identity).is_none_or(|entry| {
                        entry.actor.address().get_id() != child.address().get_id()
                    }) || !state.entries.contains_key(&parent)
                    {
                        Err(DirectoryError::Unauthorized { actor: identity })
                    } else if child.terminal().get().is_some() {
                        Err(DirectoryError::Unauthorized { actor: identity })
                    } else if state
                        .worker_transfers
                        .values()
                        .any(|transfer| transfer.child == identity)
                    {
                        Err(DirectoryError::IdentityConflict { actor: identity.id })
                    } else {
                        let id = state.next_transfer;
                        state.next_transfer = state
                            .next_transfer
                            .checked_add(1)
                            .ok_or("run transfer identity exhausted")?;
                        state.worker_transfers.insert(
                            id,
                            WorkerTransfer {
                                parent,
                                child: identity,
                                destination_run,
                            },
                        );
                        Ok(WorkerTransferReservation {
                            id,
                            owner: myself.clone(),
                            armed: true,
                        })
                    };
                let _ = reply.send(result);
            }
            RunControlMessage::CommitWorkerTransfer { id, reply } => {
                let result = if let Some(transfer) = state.worker_transfers.remove(&id) {
                    if let Some(entry) = state.entries.get_mut(&transfer.child) {
                        entry.spawn_ownership = if transfer.destination_run {
                            SpawnOwnership::Independent
                        } else {
                            SpawnOwnership::Supervised(transfer.parent)
                        };
                        Ok(())
                    } else {
                        Err(DirectoryError::Unauthorized {
                            actor: transfer.child,
                        })
                    }
                } else {
                    Err(DirectoryError::Unavailable)
                };
                let _ = reply.send(result);
                state.settle_preparation_waiters();
                state.start_resource_retirement();
            }
            RunControlMessage::AbortWorkerTransfer { id } => {
                state.worker_transfers.remove(&id);
                state.settle_preparation_waiters();
                state.start_resource_retirement();
            }
            RunControlMessage::ReserveResourceTransfer {
                parent,
                source_run,
                destination_run,
                cells,
                reply,
            } => {
                let source = if source_run {
                    ResourceLifetime::Run
                } else {
                    ResourceLifetime::Actor(parent)
                };
                let destination = if destination_run {
                    ResourceLifetime::Run
                } else {
                    ResourceLifetime::Actor(parent)
                };
                let result = if state.phase != RunPhase::Running { Err(DirectoryError::Closed) }
                else if !state.entries.contains_key(&parent) || cells.iter().any(|cell| state.resources.get(&cell.get_id()).is_none_or(|owned| !matches!(owned.owner, ResourceOwner::Owned(owner) | ResourceOwner::Retired { owner } if owner == source))) { Err(DirectoryError::Unauthorized { actor: parent }) }
                else {
                    let id = state.next_transfer;
                    state.next_transfer = state.next_transfer.checked_add(1).ok_or("run transfer identity exhausted")?;
                    let mut resources = Vec::new();
                    for cell in cells {
                        let resource = state.resources.get_mut(&cell.get_id()).expect("selected resource");
                        if matches!(resource.owner, ResourceOwner::Owned(_)) { resource.owner = ResourceOwner::Transfer { id, source }; resources.push(cell.get_id()); }
                    }
                    state.resource_transfers.insert(id, ResourceTransfer { source, destination, resources });
                    Ok(ResourceTransferReservation { id, owner: myself.clone(), armed: true })
                };
                let _ = reply.send(result);
            }
            RunControlMessage::CommitResourceTransfer { id, reply } => {
                let result = if let Some(transfer) = state.resource_transfers.remove(&id) {
                    for key in transfer.resources {
                        if let Some(resource) = state.resources.get_mut(&key) {
                            if matches!(resource.owner, ResourceOwner::Transfer { id: issued, .. } if issued == id)
                            {
                                let source_cell = match transfer.source {
                                    ResourceLifetime::Actor(actor) => state
                                        .entries
                                        .get(&actor)
                                        .map(|entry| entry.actor.address().get_cell()),
                                    ResourceLifetime::Run => None,
                                };
                                let destination_cell = match transfer.destination {
                                    ResourceLifetime::Actor(actor) => state
                                        .entries
                                        .get(&actor)
                                        .map(|entry| entry.actor.address().get_cell()),
                                    ResourceLifetime::Run => None,
                                };
                                if let Some(source) = source_cell {
                                    resource.resource.cell.unlink(source);
                                }
                                if let Some(destination) = destination_cell {
                                    resource.resource.cell.link(destination);
                                }
                                resource.owner = ResourceOwner::Owned(transfer.destination);
                            }
                        }
                    }
                    Ok(())
                } else {
                    Err(DirectoryError::Unavailable)
                };
                let _ = reply.send(result);
                state.settle_preparation_waiters();
                state.start_resource_retirement();
                state.start_pending_actor_resources();
            }
            RunControlMessage::AbortResourceTransfer { id } => {
                if let Some(transfer) = state.resource_transfers.remove(&id) {
                    for key in transfer.resources {
                        if let Some(resource) = state.resources.get_mut(&key) {
                            if matches!(resource.owner, ResourceOwner::Transfer { id: issued, .. } if issued == id)
                            {
                                resource.owner = ResourceOwner::Owned(transfer.source);
                            }
                        }
                    }
                }
                state.settle_preparation_waiters();
                state.start_resource_retirement();
                state.start_pending_actor_resources();
            }
            RunControlMessage::IndependentRoots { reply } => {
                let _ = reply.send(
                    state
                        .entries
                        .values()
                        .filter(|entry| entry.spawn_ownership.is_independent())
                        .map(|entry| entry.actor.clone())
                        .collect(),
                );
            }
            RunControlMessage::RetireRunResources { reply } => match &state.resource_retirement {
                ResourceRetirement::Settled(outcome) => {
                    let _ = reply.send(outcome.clone());
                }
                _ => {
                    state.resource_waiters.push(reply);
                    if matches!(state.resource_retirement, ResourceRetirement::Available) {
                        state.resource_retirement = ResourceRetirement::Requested;
                    }
                    state.start_resource_retirement();
                }
            },
            RunControlMessage::RunResourcesSettled { owner, outcomes } => {
                let mut combined = crate::CleanupComponentOutcome::Confirmed;
                for (id, outcome) in outcomes {
                    if outcome == crate::CleanupComponentOutcome::Confirmed {
                        if let Some(resource) = state.resources.get_mut(&id) {
                            if matches!(resource.owner, ResourceOwner::Owned(selected) if selected == owner)
                            {
                                resource.owner = ResourceOwner::Retired { owner };
                            }
                        }
                    }
                    combined = combine_cleanup(combined, outcome);
                }
                state.finish_resources(owner, combined);
            }
            RunControlMessage::RunResourceWorkerFailed { owner, detail } => {
                state.finish_resources(owner, crate::CleanupComponentOutcome::Unconfirmed(detail));
            }
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
                state.start_resource_retirement();
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
                    begin_reservation(state, actor, ReservationPurpose::Root, reply);
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
                    begin_reservation(state, actor, ReservationPurpose::Root, reply);
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
                        if let SpawnOwnership::Supervised(parent) = entry.spawn_ownership {
                            let outcome = match entry.actor.terminal().cleanup() {
                                Some(cleanup)
                                    if cleanup.actor == actor && cleanup.is_confirmed() =>
                                {
                                    crate::CleanupComponentOutcome::Confirmed
                                }
                                cleanup => crate::CleanupComponentOutcome::Unconfirmed(format!(
                                    "forgotten child {actor} cleanup: {cleanup:?}"
                                )),
                            };
                            let prior = state
                                .child_cleanup
                                .remove(&parent)
                                .unwrap_or(crate::CleanupComponentOutcome::Confirmed);
                            state
                                .child_cleanup
                                .insert(parent, combine_cleanup(prior, outcome));
                        }
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
    pub(crate) async fn authorize_root_startup(
        &self,
        reservation: &ActorReservation,
        intent: crate::RootStartupIntent,
        journal: Arc<crate::ActorRecoveryJournal>,
        recovery: Option<Arc<RootStartupRecoveryAuthority>>,
    ) -> Result<RootStartupAdmission, DirectoryError> {
        let actor = reservation.identity();
        let predecessor = self
            .ask(|reply| RunControlMessage::SelectStartup {
                actor,
                intent: intent.clone(),
                recovery,
                reply,
            })
            .await??;
        let operation: preparation::RootOperation = Box::new(move |_ticket| {
            Box::pin(async move {
                let result = startup::authorize(actor, intent, journal, predecessor)
                    .await
                    .map(preparation::RootAdmissionOutcome::StartupPermit)
                    .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>);
                (result, crate::CleanupComponentOutcome::Confirmed)
            })
        });
        match self.prepare_root(operation).await? {
            Ok(preparation::RootAdmissionOutcome::StartupPermit(permit)) => Ok(permit),
            Err(error) => Err(error
                .downcast::<DirectoryError>()
                .map(|error| *error)
                .unwrap_or(DirectoryError::Unavailable)),
            _ => Err(DirectoryError::Unavailable),
        }
    }
    pub(crate) async fn reserve_worker(
        &self,
        parent: ActorRef,
        incarnation: crate::Incarnation,
    ) -> Result<ActorReservation, DirectoryError> {
        self.ask(|reply| RunControlMessage::ReserveWorker {
            parent,
            incarnation,
            reply,
        })
        .await?
    }
    pub(crate) async fn reserve_successor(
        &self,
        predecessor: ActorRef,
        incarnation: crate::Incarnation,
    ) -> Result<ActorReservation, DirectoryError> {
        self.ask(|reply| RunControlMessage::ReserveSuccessor {
            predecessor,
            incarnation,
            reply,
        })
        .await?
    }
    pub(crate) async fn replacement_ingress_grant(
        &self,
        predecessor: ActorRef,
        successor: ActorRef,
    ) -> Result<ReplacementIngressGrant, DirectoryError> {
        self.ask(|reply| RunControlMessage::ReplacementIngressGrant {
            predecessor,
            successor,
            reply,
        })
        .await?
    }
    pub(crate) async fn children(
        &self,
        actor: ActorRef,
    ) -> Result<Vec<LocalActorRef>, DirectoryError> {
        self.ask(|reply| RunControlMessage::Children { actor, reply })
            .await
    }
    pub(crate) async fn forget_child(
        &self,
        parent: ActorRef,
        child: ActorRef,
    ) -> Result<(), DirectoryError> {
        self.ask(|reply| RunControlMessage::ForgetChild {
            parent,
            child,
            reply,
        })
        .await?
    }
    pub(crate) async fn transfer_children(
        &self,
        predecessor: ActorRef,
        successor: ActorRef,
    ) -> Result<(), DirectoryError> {
        self.ask(|reply| RunControlMessage::TransferChildren {
            predecessor,
            successor,
            reply,
        })
        .await?
    }
    pub(crate) fn retain_child_cleanup(
        &self,
        actor: ActorRef,
        outcome: crate::CleanupComponentOutcome,
    ) {
        let _ = self
            .owner
            .send_message(RunControlMessage::RetainChildCleanup { actor, outcome });
    }
    pub(crate) async fn child_cleanup(&self, actor: ActorRef) -> crate::CleanupComponentOutcome {
        self.ask(|reply| RunControlMessage::ChildCleanup { actor, reply })
            .await
            .unwrap_or_else(|error| crate::CleanupComponentOutcome::Unconfirmed(error.to_string()))
    }
    pub(super) async fn register_resource(
        &self,
        parent: ActorRef,
        preparation: u64,
        resource: ResourceChild,
    ) -> Result<(), DirectoryError> {
        self.ask(|reply| RunControlMessage::RegisterResource {
            parent,
            preparation,
            resource,
            reply,
        })
        .await?
    }
    pub(super) async fn owned_resource(
        &self,
        parent: ActorRef,
        id: ractor::ActorId,
    ) -> Result<Option<ResourceChild>, DirectoryError> {
        self.ask(|reply| RunControlMessage::OwnedResource { parent, id, reply })
            .await
    }
    pub(crate) async fn forget_resource(
        &self,
        parent: ActorRef,
        id: ractor::ActorId,
    ) -> Result<(), DirectoryError> {
        self.ask(|reply| RunControlMessage::ForgetResource { parent, id, reply })
            .await?
    }
    pub(crate) async fn retire_actor_resources(
        &self,
        actor: ActorRef,
        timeout: Duration,
    ) -> crate::CleanupComponentOutcome {
        match tokio::time::timeout(
            timeout,
            self.ask(|reply| RunControlMessage::RetireActorResources { actor, reply }),
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => crate::CleanupComponentOutcome::Unconfirmed(error.to_string()),
            Err(_) => crate::CleanupComponentOutcome::Unconfirmed(
                "actor resource retirement remains in flight".into(),
            ),
        }
    }
    pub(crate) async fn reserve_worker_transfer(
        &self,
        parent: ActorRef,
        child: LocalActorRef,
        destination_run: bool,
    ) -> Result<WorkerTransferReservation, DirectoryError> {
        self.ask(|reply| RunControlMessage::ReserveWorkerTransfer {
            parent,
            child,
            destination_run,
            reply,
        })
        .await?
    }
    pub(super) async fn reserve_resource_transfer(
        &self,
        parent: ActorRef,
        cells: Vec<ractor::ActorCell>,
        source_run: bool,
        destination_run: bool,
    ) -> Result<ResourceTransferReservation, DirectoryError> {
        self.ask(|reply| RunControlMessage::ReserveResourceTransfer {
            parent,
            cells,
            source_run,
            destination_run,
            reply,
        })
        .await?
    }
    pub(crate) async fn independent_roots(&self) -> Result<Vec<LocalActorRef>, DirectoryError> {
        self.ask(|reply| RunControlMessage::IndependentRoots { reply })
            .await
    }
    pub(crate) async fn close_run_admission(&self) -> Result<(), DirectoryError> {
        self.seal().await.map(|_| ())
    }
    pub(crate) async fn shutdown_run_resources(&self) -> crate::CleanupComponentOutcome {
        match tokio::time::timeout(
            SHUTDOWN_BUDGET,
            self.ask(|reply| RunControlMessage::RetireRunResources { reply }),
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => crate::CleanupComponentOutcome::Unconfirmed(error.to_string()),
            Err(_) => crate::CleanupComponentOutcome::Unconfirmed(
                "run resource retirement remains in flight".into(),
            ),
        }
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
    pub(crate) async fn reserve(
        &self,
        incarnation: crate::Incarnation,
    ) -> Result<ActorReservation, DirectoryError> {
        self.ask(|reply| RunControlMessage::Reserve { incarnation, reply })
            .await?
    }
    pub(crate) async fn claim_exact(
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
