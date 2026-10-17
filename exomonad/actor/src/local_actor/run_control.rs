//! One run owns actor membership, identity claims and preparation admission.
use super::*;
use ractor::ActorStatus;
use std::collections::HashSet;
use tokio::sync::oneshot;
#[cfg(test)]
mod cluster_tests;
#[cfg(test)]
mod finalization_tests;
mod preparation;
mod startup;
pub(crate) use startup::{
    RootStartupAdmission, RootStartupRecoveryAuthority, StartupPredecessorAuthority,
};
mod transfers;
#[cfg(test)]
pub(crate) use preparation::RootPlacementPublicationError;
use preparation::{IssuedRootPlacement, PreparationMessage, PreparationSupervisor, RootDelivery};
pub(crate) use preparation::{
    PublishedRootPlacement, RootAdmissionOutcome, RootOperation, RootOperationResult,
    RootPlacementMint,
};
use transfers::{ResourceTransfer, WorkerTransfer};
pub(crate) use transfers::{
    ResourceTransferReservation, WorkerTransferError, WorkerTransferReservation,
};

pub(super) struct ActorAuthority {
    creator: Option<ActorRef>,
    supervisor_parent: Option<ActorRef>,
    forest_control: bool,
    observation_roots: HashSet<ActorRef>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObservationShareOutcome {
    RecipientUnavailable,
    ScopeUnavailable,
    Unauthorized,
    Shared,
}
fn in_authority_tree(
    entries: &HashMap<ActorRef, DirectoryEntry>,
    owner: ActorRef,
    candidate: ActorRef,
    creation: bool,
) -> bool {
    let mut cursor = candidate;
    for _ in 0..=entries.len() {
        if cursor == owner {
            return true;
        }
        let Some(authority) = entries
            .get(&cursor)
            .and_then(|entry| entry.authority.as_ref())
        else {
            return false;
        };
        let parent = if creation {
            authority.creator.or(authority.supervisor_parent)
        } else {
            authority.supervisor_parent.or(authority.creator)
        };
        let Some(parent) = parent else {
            return false;
        };
        cursor = parent;
    }
    false
}
fn can_control(
    entries: &HashMap<ActorRef, DirectoryEntry>,
    owner: ActorRef,
    target: ActorRef,
) -> bool {
    entries.get(&owner).is_some_and(|entry| {
        entry.actor.terminal().get().is_none()
            && entry
                .authority
                .as_ref()
                .is_some_and(|authority| authority.forest_control)
    }) || in_authority_tree(entries, owner, target, false)
}
fn can_observe(
    entries: &HashMap<ActorRef, DirectoryEntry>,
    owner: ActorRef,
    target: ActorRef,
) -> bool {
    can_control(entries, owner, target)
        || entries
            .get(&owner)
            .and_then(|entry| entry.authority.as_ref())
            .is_some_and(|authority| {
                authority
                    .observation_roots
                    .iter()
                    .any(|root| in_authority_tree(entries, *root, target, true))
            })
}

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
    #[error("resident context differs from actor {actor:?}'s descriptor")]
    ResidentContextMismatch { actor: ActorRef },
    #[error("actor {actor:?} refused confirmed retirement: {cause}")]
    RetirementRefused {
        actor: ActorRef,
        cause: crate::KernelCallFailure,
    },
    #[error("actor {actor:?} retirement did not settle before the deadline")]
    RetirementDeadline { actor: ActorRef },
}

/// Registration and preparation share this phase, rather than separate gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunPhase {
    Running,
    RetiringPreparations,
    RetiringResources,
    Retired,
    Finalizing,
}

pub(super) struct RunState {
    shared_compiler: crate::CompilerPreparationOwner,
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
    recovery_boot: Option<Arc<RootStartupRecoveryAuthority>>,
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
            shared_compiler: crate::CompilerPreparationOwner::new(),
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
            recovery_boot: None,
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

/// Issued only after every sealed run population has confirmed settlement.
/// It authorizes release of one exact retained lifecycle owner.
pub(crate) struct RunRetirementGrant {
    actor: ActorRef,
    lifecycle: crate::kernel::LifecycleHandle,
    never_registered: bool,
}
impl RunRetirementGrant {
    pub(crate) fn actor(&self) -> ActorRef {
        self.actor
    }
    pub(crate) fn lifecycle(&self) -> &crate::kernel::LifecycleHandle {
        &self.lifecycle
    }
    pub(crate) fn never_registered(&self) -> bool {
        self.never_registered
    }
}
pub(super) enum RunFinalizationTargets {
    Retire(Vec<(crate::kernel::LifecycleHandle, Option<ractor::ActorCell>)>),
    Finalize(Vec<RunRetirementGrant>),
}

pub(crate) struct ActorReservation {
    cleanup_only: bool,
    parent_admission: Option<crate::kernel::lifecycle::ActorAdmissionLease>,
    identity: ActorRef,
    lifecycle: crate::kernel::LifecycleHandle,
    terminal: RetainedActorExit,
    owner: RactorRef<RunControlMessage>,
    armed: bool,
}
impl ActorReservation {
    pub(crate) fn cleanup_only(&self) -> bool {
        self.cleanup_only
    }
    pub(crate) fn retain_parent_admission(
        &mut self,
        lease: crate::kernel::lifecycle::ActorAdmissionLease,
    ) {
        assert!(
            self.parent_admission.is_none(),
            "actor reservation already retains its parent admission"
        );
        self.parent_admission = Some(lease);
    }
    pub(crate) fn take_parent_admission(
        &mut self,
    ) -> Option<crate::kernel::lifecycle::ActorAdmissionLease> {
        self.parent_admission.take()
    }
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
    ExistingInput,
    Root,
    Worker { parent: ActorRef },
    Replacement { predecessor: ActorRef },
}
enum ReservationPhase {
    Creating,
    Abandoning,
    Ready,
    Registered,
    Abandoned,
}
struct ReservationRecord {
    phase: ReservationPhase,
    purpose: ReservationPurpose,
    startup: Option<crate::RootStartupIntent>,
    previous_claim: Option<ActorRef>,
    terminal: RetainedActorExit,
    lifecycle: Option<crate::kernel::LifecycleHandle>,
    executor: Option<ractor::ActorCell>,
    reply: Option<oneshot::Sender<Result<ActorReservation, DirectoryError>>>,
}

impl ReservationRecord {
    fn preparation_pending(&self) -> bool {
        matches!(
            self.phase,
            ReservationPhase::Creating | ReservationPhase::Abandoning
        ) || (matches!(self.purpose, ReservationPurpose::ExistingInput)
            && matches!(self.phase, ReservationPhase::Ready))
    }
}

enum ResourceRetirement {
    Available,
    Requested,
    Running,
    Settled(crate::CleanupComponentOutcome),
}
enum PreparationIntent {
    Creation,
    ExistingInput,
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
        if !self.worker_transfers.is_empty()
            || !self.resource_transfers.is_empty()
            || self
                .reservations
                .values()
                .any(|record| record.preparation_pending())
        {
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
    fn finalization_grants(&self) -> Vec<RunRetirementGrant> {
        self.reservations
            .iter()
            .filter_map(|(actor, record)| {
                record
                    .lifecycle
                    .as_ref()
                    .map(|lifecycle| RunRetirementGrant {
                        actor: *actor,
                        lifecycle: lifecycle.clone(),
                        never_registered: matches!(record.phase, ReservationPhase::Abandoned),
                    })
            })
            .collect()
    }
    fn settle_preparation_waiters(&mut self) {
        if !self.worker_transfers.is_empty()
            || !self.resource_transfers.is_empty()
            || self
                .reservations
                .values()
                .any(|record| record.preparation_pending())
        {
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
    SharedCompilerClient {
        reply: oneshot::Sender<(
            crate::CompilerPreparationClient,
            crate::CompilerPreparationCleanup,
        )>,
    },
    FinishRun {
        members: Vec<crate::kernel::lifecycle::SuccessorDrainReceipt>,
        reply: oneshot::Sender<Result<Vec<RunRetirementGrant>, DirectoryError>>,
    },
    FinalizationTargets {
        reply: oneshot::Sender<Result<RunFinalizationTargets, DirectoryError>>,
    },
    CompleteRun {
        reply: oneshot::Sender<Result<Option<ractor::ActorCell>, DirectoryError>>,
    },
    RegisterRecoveryBoot {
        journal: Arc<crate::ActorRecoveryJournal>,
        registration: crate::JournalBootRegistration,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    CreationDescendant {
        owner: ActorRef,
        target: ActorRef,
        reply: oneshot::Sender<bool>,
    },
    RegisterResident {
        actor: ActorRef,
        context: crate::ActorSessionContext,
        descriptor: crate::ActorDescriptor,
        forest_control: bool,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    AuthorizeRelation {
        owner: ActorRef,
        target: ActorRef,
        observe: bool,
        reply: oneshot::Sender<bool>,
    },
    ShareObservation {
        owner: ActorRef,
        recipient: ActorRef,
        scope: ActorRef,
        reply: oneshot::Sender<ObservationShareOutcome>,
    },
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
        publication: crate::resident_actor::WorkerRetentionPublication,
        parent_lease: Option<crate::kernel::lifecycle::ActorAdmissionLease>,
        reply: oneshot::Sender<Result<(), WorkerTransferError>>,
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
        intent: PreparationIntent,
        operation: RootOperation,
        reply: oneshot::Sender<Result<oneshot::Receiver<RootDelivery>, DirectoryError>>,
    },
    AcceptPreparation {
        id: u64,
        reply: oneshot::Sender<Result<(), DirectoryError>>,
    },
    PlacementPrepared(Arc<IssuedRootPlacement>),
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
    ReserveExistingInput {
        incarnation: crate::Incarnation,
        reply: oneshot::Sender<Result<ActorReservation, DirectoryError>>,
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
    begin_reservation_after(state, actor, purpose, None, reply);
}
fn begin_reservation_after(
    state: &mut RunState,
    actor: ActorRef,
    purpose: ReservationPurpose,
    predecessor: Option<crate::kernel::LifecycleHandle>,
    reply: oneshot::Sender<Result<ActorReservation, DirectoryError>>,
) {
    let terminal = RetainedActorExit::new();
    state.reservations.insert(
        actor,
        ReservationRecord {
            phase: ReservationPhase::Creating,
            purpose,
            startup: None,
            previous_claim: predecessor
                .as_ref()
                .map(crate::kernel::LifecycleHandle::identity),
            terminal: terminal.clone(),
            lifecycle: None,
            executor: None,
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
                    predecessor,
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
            RunControlMessage::FinishRun { members, reply } => {
                let exact = members.len()
                    == state
                        .reservations
                        .values()
                        .filter(|record| matches!(record.phase, ReservationPhase::Registered))
                        .count()
                    && members
                        .iter()
                        .map(|member| member.predecessor())
                        .collect::<HashSet<_>>()
                        .len()
                        == members.len()
                    && members.iter().all(|receipt| {
                        state
                            .reservations
                            .get(&receipt.predecessor())
                            .is_some_and(|record| {
                                matches!(record.phase, ReservationPhase::Registered)
                                    && receipt.confirms_actor_retirement()
                                    && record.terminal.get().as_ref() == Some(receipt.terminal())
                                    && record.terminal.cleanup().as_ref() == Some(receipt.cleanup())
                                    && record.executor.as_ref().is_some_and(|executor| {
                                        executor.get_status() == ActorStatus::Stopped
                                    })
                            })
                    });
                let confirmed = state.phase == RunPhase::Retired
                    && state.shared_compiler.cleanup().observation().is_confirmed()
                    && matches!(
                        state.resource_retirement,
                        ResourceRetirement::Settled(crate::CleanupComponentOutcome::Confirmed)
                    )
                    && state.preparations.values().all(|record| {
                        matches!(
                            record.phase,
                            PreparationPhase::Settled(crate::CleanupComponentOutcome::Confirmed)
                        )
                    })
                    && state
                        .reservations
                        .values()
                        .all(|record| !record.preparation_pending())
                    && state.worker_transfers.is_empty()
                    && state.resource_transfers.is_empty()
                    && state
                        .resources
                        .values()
                        .all(|resource| matches!(resource.owner, ResourceOwner::Retired { .. }))
                    && state.child_cleanup.values().all(|outcome| {
                        matches!(outcome, crate::CleanupComponentOutcome::Confirmed)
                    });
                if exact && confirmed {
                    state.phase = RunPhase::Finalizing;
                    let _ = reply.send(Ok(state.finalization_grants()));
                } else {
                    let _ = reply.send(Err(DirectoryError::Unavailable));
                }
            }
            RunControlMessage::FinalizationTargets { reply } => {
                let result = match state.phase {
                    RunPhase::Retired
                        if state.shared_compiler.cleanup().observation().is_confirmed() =>
                    {
                        Ok(RunFinalizationTargets::Retire(
                            state
                                .reservations
                                .values()
                                .filter(|record| {
                                    matches!(record.phase, ReservationPhase::Registered)
                                })
                                .filter_map(|record| {
                                    record.lifecycle.clone().map(|lifecycle| {
                                        let cell = record.executor.clone();
                                        (lifecycle, cell)
                                    })
                                })
                                .collect(),
                        ))
                    }
                    RunPhase::Finalizing => Ok(RunFinalizationTargets::Finalize(
                        state.finalization_grants(),
                    )),
                    _ => Err(DirectoryError::Unavailable),
                };
                let _ = reply.send(result);
            }
            RunControlMessage::CompleteRun { reply } => {
                if state.phase == RunPhase::Finalizing
                    && state.shared_compiler.cleanup().observation().is_confirmed()
                    && state.reservations.values().all(|record| {
                        record
                            .executor
                            .as_ref()
                            .is_none_or(|executor| executor.get_status() == ActorStatus::Stopped)
                            && record
                                .lifecycle
                                .as_ref()
                                .is_none_or(|lifecycle| lifecycle.run_finalized())
                    })
                {
                    let supervisor = state.preparation_supervisor.take().map(|supervisor| {
                        let cell = supervisor.get_cell();
                        supervisor.stop(None);
                        cell
                    });
                    let _ = reply.send(Ok(supervisor));
                    myself.stop(None);
                } else {
                    let _ = reply.send(Err(DirectoryError::Unavailable));
                }
            }
            RunControlMessage::RegisterRecoveryBoot {
                journal,
                registration,
                reply,
            } => {
                let result = if state.phase != RunPhase::Running {
                    Err(DirectoryError::Closed)
                } else if state.recovery_boot.is_some()
                    || !state.reservations.is_empty()
                    || !state.entries.is_empty()
                {
                    Err(DirectoryError::Unavailable)
                } else {
                    startup::RootStartupRecoveryAuthority::from_boot(journal, registration).map(
                        |authority| {
                            state.recovery_boot = Some(Arc::new(authority));
                        },
                    )
                };
                let _ = reply.send(result);
            }
            RunControlMessage::CreationDescendant {
                owner,
                target,
                reply,
            } => {
                let _ = reply.send(in_authority_tree(&state.entries, owner, target, true));
            }
            RunControlMessage::RegisterResident {
                actor,
                context,
                descriptor,
                forest_control,
                reply,
            } => {
                // Issued source roots may select a newer immutable revision than
                // the deployment roots. Retain that exact selection while checking
                // every actor/placement/effect fact against its descriptor.
                let mut expected = descriptor.session_context(actor);
                expected.source_layer = context.source_layer.clone();
                let result = if state.phase != RunPhase::Running {
                    Err(DirectoryError::Closed)
                } else if context != expected {
                    Err(DirectoryError::ResidentContextMismatch { actor })
                } else if let Some(entry) = state.entries.get_mut(&actor) {
                    if entry.authority.is_some() || state.sessions.contains_key(&actor) {
                        Err(DirectoryError::AlreadyRegistered { actor })
                    } else {
                        entry.authority = Some(ActorAuthority {
                            creator: descriptor.creator(),
                            supervisor_parent: descriptor.supervisor_parent(),
                            forest_control,
                            observation_roots: HashSet::new(),
                        });
                        state.sessions.insert(actor, context);
                        Ok(())
                    }
                } else {
                    Err(DirectoryError::Unauthorized { actor })
                };
                let _ = reply.send(result);
            }
            RunControlMessage::AuthorizeRelation {
                owner,
                target,
                observe,
                reply,
            } => {
                let _ = reply.send(if observe {
                    can_observe(&state.entries, owner, target)
                } else {
                    can_control(&state.entries, owner, target)
                });
            }
            RunControlMessage::ShareObservation {
                owner,
                recipient,
                scope,
                reply,
            } => {
                let outcome = if state.entries.get(&recipient).is_none_or(|entry| {
                    entry.actor.terminal().get().is_some() || entry.authority.is_none()
                }) {
                    ObservationShareOutcome::RecipientUnavailable
                } else if state
                    .entries
                    .get(&scope)
                    .is_none_or(|entry| entry.authority.is_none())
                {
                    ObservationShareOutcome::ScopeUnavailable
                } else if !can_observe(&state.entries, owner, scope) {
                    ObservationShareOutcome::Unauthorized
                } else {
                    state
                        .entries
                        .get_mut(&recipient)
                        .expect("validated recipient")
                        .authority
                        .as_mut()
                        .expect("validated authority")
                        .observation_roots
                        .insert(scope);
                    ObservationShareOutcome::Shared
                };
                let _ = reply.send(outcome);
            }
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
                                recovery
                                    .or_else(|| state.recovery_boot.clone())
                                    .ok_or(DirectoryError::Unauthorized { actor })?,
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
                            if let Some(authority) = entry.authority.as_mut() {
                                if authority.supervisor_parent == Some(predecessor) {
                                    authority.supervisor_parent = Some(successor);
                                }
                            }
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
                    } else if state.entries.get(&identity).is_some_and(|entry| {
                        let parent_cell = state.entries[&parent].actor.address().get_cell();
                        let supervisor = child.address().get_cell().try_get_supervisor();
                        match entry.spawn_ownership {
                            SpawnOwnership::Supervised(owner) => {
                                owner != parent
                                    || supervisor
                                        .is_none_or(|cell| cell.get_id() != parent_cell.get_id())
                            }
                            SpawnOwnership::Independent => supervisor.is_some(),
                        }
                    }) {
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
                                source: state.entries[&identity].spawn_ownership,
                                destination_run,
                            },
                        );
                        Ok(WorkerTransferReservation {
                            id,
                            owner: myself.clone(),
                            armed: true,
                            parent_lease: None,
                        })
                    };
                let _ = reply.send(result);
            }
            RunControlMessage::CommitWorkerTransfer {
                id,
                publication,
                parent_lease,
                reply,
            } => {
                let result = if let Some(transfer) = state.worker_transfers.remove(&id) {
                    let parent_cell = state
                        .entries
                        .get(&transfer.parent)
                        .map(|entry| entry.actor.address().get_cell());
                    match (state.entries.get_mut(&transfer.child), parent_cell) {
                        (Some(entry), Some(parent)) if entry.spawn_ownership == transfer.source => {
                            let cell = entry.actor.address().get_cell();
                            let supervisor = cell.try_get_supervisor();
                            let current_matches = match transfer.source {
                                SpawnOwnership::Supervised(owner) => {
                                    owner == transfer.parent
                                        && supervisor
                                            .is_some_and(|cell| cell.get_id() == parent.get_id())
                                }
                                SpawnOwnership::Independent => supervisor.is_none(),
                            };
                            if current_matches
                                && publication.matches(
                                    transfer.parent,
                                    transfer.child,
                                    transfer.destination_run,
                                )
                            {
                                let supervision = || {
                                    if transfer.destination_run {
                                        cell.unlink(parent);
                                        entry.spawn_ownership = SpawnOwnership::Independent;
                                    } else {
                                        cell.link(parent);
                                        entry.spawn_ownership =
                                            SpawnOwnership::Supervised(transfer.parent);
                                    }
                                };
                                publication
                                    .publish(supervision)
                                    .map_err(WorkerTransferError::Publication)
                            } else {
                                Err(WorkerTransferError::Directory(
                                    DirectoryError::Unauthorized {
                                        actor: transfer.child,
                                    },
                                ))
                            }
                        }
                        _ => Err(WorkerTransferError::Directory(
                            DirectoryError::Unauthorized {
                                actor: transfer.child,
                            },
                        )),
                    }
                } else {
                    Err(WorkerTransferError::Directory(DirectoryError::Unavailable))
                };
                drop(parent_lease);
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
                    Ok(ResourceTransferReservation { id, owner: myself.clone(), armed: true, parent_lease: None })
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
            RunControlMessage::BeginPreparation {
                intent,
                operation,
                reply,
            } => {
                let Some(supervisor) = state.preparation_supervisor.as_ref() else {
                    let _ = reply.send(Err(DirectoryError::Unavailable));
                    return Ok(());
                };
                if state.phase != RunPhase::Running
                    && !(matches!(intent, PreparationIntent::ExistingInput)
                        && state.phase == RunPhase::RetiringPreparations)
                {
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
            RunControlMessage::PlacementPrepared(issued) => {
                if let Some(record) = state.preparations.get_mut(&issued.id()) {
                    record.placement.get_or_insert(issued.placement());
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
                            if (state.phase != RunPhase::Running
                                && !matches!(record.purpose, ReservationPurpose::ExistingInput))
                                || matches!(
                                    record.phase,
                                    ReservationPhase::Abandoned | ReservationPhase::Abandoning
                                )
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
                                        cleanup_only: state.phase != RunPhase::Running
                                            && matches!(
                                                record.purpose,
                                                ReservationPurpose::ExistingInput
                                            ),
                                        parent_admission: None,
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
                            // No affine reservation was issued and no executor can
                            // have used this failed starter's identity.
                            if state.identities.claimed.get(&actor.id) == Some(&actor) {
                                if let Some(previous) = record.previous_claim {
                                    state.identities.claimed.insert(actor.id, previous);
                                } else {
                                    state.identities.claimed.remove(&actor.id);
                                }
                            }
                            record.phase = ReservationPhase::Abandoned;
                            if let Some(reply) = record.reply.take() {
                                let _ = reply.send(Err(DirectoryError::Unavailable));
                            }
                        }
                    }
                } else if let Ok(lifecycle) = lifecycle {
                    lifecycle.close_abandoned();
                }
                state.settle_preparation_waiters();
                state.start_resource_retirement();
            }
            RunControlMessage::AbandonReservation { actor } => {
                if let Some(record) = state.reservations.get_mut(&actor) {
                    record.phase = if matches!(
                        record.phase,
                        ReservationPhase::Creating | ReservationPhase::Abandoning
                    ) {
                        ReservationPhase::Abandoning
                    } else {
                        ReservationPhase::Abandoned
                    };
                    if let Some(lifecycle) = &record.lifecycle {
                        lifecycle.close_abandoned();
                    }
                }
            }
            RunControlMessage::ReserveExistingInput { incarnation, reply } => {
                if !matches!(
                    state.phase,
                    RunPhase::Running | RunPhase::RetiringPreparations
                ) {
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
                    let purpose = ReservationPurpose::ExistingInput;
                    begin_reservation(state, actor, purpose, reply);
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
                                .reservations
                                .get(previous)
                                .is_none_or(|record| record.terminal.get().is_none())
                    })
                {
                    let _ = reply.send(Err(DirectoryError::IdentityConflict { actor: actor.id }));
                } else {
                    let predecessor = state
                        .identities
                        .claimed
                        .get(&actor.id)
                        .and_then(|previous| state.reservations.get(previous))
                        .and_then(|record| record.lifecycle.clone());
                    advance_actor_ids_past(actor.id);
                    state.identities.fenced.remove(&actor.id);
                    state.identities.claimed.insert(actor.id, actor);
                    begin_reservation_after(
                        state,
                        actor,
                        ReservationPurpose::Root,
                        predecessor,
                        reply,
                    );
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
                    let record = state
                        .reservations
                        .get_mut(&identity)
                        .expect("registration reservation");
                    record.executor = Some(actor.address().get_cell());
                    record.phase = ReservationPhase::Registered;
                    state.entries.insert(
                        identity,
                        DirectoryEntry {
                            actor,
                            context,
                            spawn_ownership: ownership,
                            authority: None,
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
            RunControlMessage::SharedCompilerClient { reply } => {
                let _ = reply.send((
                    state.shared_compiler.client(),
                    state.shared_compiler.cleanup(),
                ));
            }
            RunControlMessage::Seal { reply } => {
                if state.phase == RunPhase::Running {
                    state.shared_compiler.close();
                    state.phase = RunPhase::RetiringPreparations;
                    for record in state.reservations.values_mut() {
                        // Prepared input already owns effects. Its producer must
                        // receive the reservation and run the real refusal cleanup.
                        // Register still checks the sealed run phase.
                        if matches!(record.purpose, ReservationPurpose::ExistingInput) {
                            continue;
                        }
                        if !matches!(record.phase, ReservationPhase::Registered) {
                            record.terminal.request_shutdown(ActorTerminal::new(
                                ActorExitKind::Cancelled,
                                "forest reservation shutdown",
                            ));
                            if let Some(lifecycle) = &record.lifecycle {
                                lifecycle.close_abandoned();
                            }
                            record.phase = if matches!(
                                record.phase,
                                ReservationPhase::Creating | ReservationPhase::Abandoning
                            ) {
                                ReservationPhase::Abandoning
                            } else {
                                ReservationPhase::Abandoned
                            };
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
    shared_compiler: crate::CompilerPreparationClient,
    shared_cleanup: crate::CompilerPreparationCleanup,
}
impl RunControlClient {
    pub(crate) fn preparation_client(&self) -> crate::CompilerPreparationClient {
        self.shared_compiler.clone()
    }
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        self.owner.get_id() == other.owner.get_id()
    }
    #[cfg(test)]
    pub(crate) async fn stop_owner_for_test(&self) {
        self.owner.stop(None);
        let _ = self.owner.get_cell().wait(Some(SHUTDOWN_BUDGET)).await;
    }
    pub async fn start() -> Result<(Self, ractor::concurrency::JoinHandle<()>), ractor::SpawnErr> {
        let (owner, task) = Actor::spawn(None, RunControl, ()).await?;
        let (supervisor, supervisor_task) =
            Actor::spawn_linked(None, PreparationSupervisor, owner.clone(), owner.get_cell())
                .await?;
        drop(supervisor_task);
        let (reply, receiver) = oneshot::channel();
        owner
            .send_message(RunControlMessage::SharedCompilerClient { reply })
            .map_err(|error| ractor::SpawnErr::StartupFailed(Box::new(error)))?;
        let (shared_compiler, shared_cleanup) = receiver
            .await
            .map_err(|error| ractor::SpawnErr::StartupFailed(Box::new(error)))?;
        let client = Self {
            owner,
            shared_compiler,
            shared_cleanup,
        };
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
    pub(crate) async fn finish_run(&self) -> Result<(), DirectoryError> {
        self.seal().await?;
        let targets = self
            .ask(|reply| RunControlMessage::FinalizationTargets { reply })
            .await??;
        let grants = match targets {
            RunFinalizationTargets::Finalize(grants) => grants,
            RunFinalizationTargets::Retire(lifecycles) => {
                let mut members = Vec::with_capacity(lifecycles.len());
                for (lifecycle, cell) in lifecycles {
                    let receipt =
                        tokio::time::timeout(SHUTDOWN_BUDGET, lifecycle.retire_for_successor())
                            .await
                            .map_err(|_| DirectoryError::RetirementDeadline {
                                actor: lifecycle.identity(),
                            })?
                            .map_err(|cause| DirectoryError::RetirementRefused {
                                actor: lifecycle.identity(),
                                cause,
                            })?;
                    if let Some(cell) = cell {
                        cell.wait(Some(SHUTDOWN_BUDGET))
                            .await
                            .map_err(|_| DirectoryError::Unavailable)?;
                    }
                    members.push(receipt);
                }
                self.ask(|reply| RunControlMessage::FinishRun { members, reply })
                    .await??
            }
        };
        for grant in grants {
            let lifecycle = grant.lifecycle().clone();
            if !lifecycle.run_finalized() {
                tokio::time::timeout(SHUTDOWN_BUDGET, lifecycle.finish_run(grant))
                    .await
                    .map_err(|_| DirectoryError::RetirementDeadline {
                        actor: lifecycle.identity(),
                    })?
                    .map_err(|cause| DirectoryError::RetirementRefused {
                        actor: lifecycle.identity(),
                        cause,
                    })?;
            }
        }
        if let Some(supervisor) = self
            .ask(|reply| RunControlMessage::CompleteRun { reply })
            .await??
        {
            supervisor
                .wait(Some(SHUTDOWN_BUDGET))
                .await
                .map_err(|_| DirectoryError::Unavailable)?;
        }
        self.owner
            .get_cell()
            .wait(Some(SHUTDOWN_BUDGET))
            .await
            .map_err(|_| DirectoryError::Unavailable)
    }
    pub(crate) async fn register_recovery_boot(
        &self,
        journal: Arc<crate::ActorRecoveryJournal>,
        registration: crate::JournalBootRegistration,
    ) -> Result<(), DirectoryError> {
        self.ask(|reply| RunControlMessage::RegisterRecoveryBoot {
            journal,
            registration,
            reply,
        })
        .await?
    }
    pub(crate) async fn creation_descendant(
        &self,
        owner: ActorRef,
        target: ActorRef,
    ) -> Result<bool, DirectoryError> {
        self.ask(|reply| RunControlMessage::CreationDescendant {
            owner,
            target,
            reply,
        })
        .await
    }
    /// Install both resident directory facts in one serialized owner turn.
    /// Losing the reply observer does not withdraw accepted registration.
    pub(crate) async fn register_resident(
        &self,
        actor: ActorRef,
        context: crate::ActorSessionContext,
        descriptor: crate::ActorDescriptor,
        forest_control: bool,
    ) -> Result<(), DirectoryError> {
        self.ask(|reply| RunControlMessage::RegisterResident {
            actor,
            context,
            descriptor,
            forest_control,
            reply,
        })
        .await?
    }
    pub(crate) async fn authorize_control(
        &self,
        owner: ActorRef,
        target: ActorRef,
    ) -> Result<bool, DirectoryError> {
        self.ask(|reply| RunControlMessage::AuthorizeRelation {
            owner,
            target,
            observe: false,
            reply,
        })
        .await
    }
    pub(crate) async fn authorize_observe(
        &self,
        owner: ActorRef,
        target: ActorRef,
    ) -> Result<bool, DirectoryError> {
        self.ask(|reply| RunControlMessage::AuthorizeRelation {
            owner,
            target,
            observe: true,
            reply,
        })
        .await
    }
    pub(crate) async fn share_observation(
        &self,
        owner: ActorRef,
        recipient: ActorRef,
        scope: ActorRef,
    ) -> Result<ObservationShareOutcome, DirectoryError> {
        self.ask(|reply| RunControlMessage::ShareObservation {
            owner,
            recipient,
            scope,
            reply,
        })
        .await
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
        let endpoint = self
            .resolve(parent)
            .await?
            .ok_or(DirectoryError::Unauthorized { actor: parent })?;
        let lease = endpoint
            .lifecycle()
            .resource_admission()
            .await
            .map_err(|_| DirectoryError::Unauthorized { actor: parent })?;
        let mut reservation = self
            .ask(|reply| RunControlMessage::ReserveWorkerTransfer {
                parent,
                child,
                destination_run,
                reply,
            })
            .await??;
        reservation.parent_lease = Some(lease);
        Ok(reservation)
    }
    pub(super) async fn reserve_resource_transfer(
        &self,
        parent: ActorRef,
        cells: Vec<ractor::ActorCell>,
        source_run: bool,
        destination_run: bool,
    ) -> Result<ResourceTransferReservation, DirectoryError> {
        let endpoint = self
            .resolve(parent)
            .await?
            .ok_or(DirectoryError::Unauthorized { actor: parent })?;
        let lease = endpoint
            .lifecycle()
            .resource_admission()
            .await
            .map_err(|_| DirectoryError::Unauthorized { actor: parent })?;
        let mut reservation = self
            .ask(|reply| RunControlMessage::ReserveResourceTransfer {
                parent,
                cells,
                source_run,
                destination_run,
                reply,
            })
            .await??;
        reservation.parent_lease = Some(lease);
        Ok(reservation)
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
            .ask(|reply| RunControlMessage::BeginPreparation {
                intent: PreparationIntent::Creation,
                operation,
                reply,
            })
            .await??;
        let delivery = receiver.await.map_err(|_| DirectoryError::Unavailable)?;
        if delivery.is_error() {
            return Ok(delivery.accept());
        }
        self.ask(|reply| RunControlMessage::AcceptPreparation {
            id: delivery.id,
            reply,
        })
        .await??;
        Ok(delivery.accept())
    }
    pub(crate) async fn prepare_existing_input(
        &self,
        operation: RootOperation,
    ) -> Result<RootOperationResult, DirectoryError> {
        let receiver = self
            .ask(|reply| RunControlMessage::BeginPreparation {
                intent: PreparationIntent::ExistingInput,
                operation,
                reply,
            })
            .await??;
        let delivery = receiver.await.map_err(|_| DirectoryError::Unavailable)?;
        if delivery.is_error() {
            return Ok(delivery.accept());
        }
        self.ask(|reply| RunControlMessage::AcceptPreparation {
            id: delivery.id,
            reply,
        })
        .await??;
        Ok(delivery.accept())
    }
    pub(crate) async fn reserve_existing_input(
        &self,
        incarnation: crate::Incarnation,
    ) -> Result<ActorReservation, DirectoryError> {
        self.ask(|reply| RunControlMessage::ReserveExistingInput { incarnation, reply })
            .await?
    }
    pub(crate) async fn settle_preparations(
        &self,
        timeout: Duration,
    ) -> crate::CleanupComponentOutcome {
        let deadline = tokio::time::Instant::now() + timeout;
        let shared = self.shared_cleanup.wait_for_settlement(timeout).await;
        let preparations = match tokio::time::timeout_at(
            deadline,
            self.ask(|reply| RunControlMessage::ObservePreparations { reply }),
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(error)) => crate::CleanupComponentOutcome::Unconfirmed(error.to_string()),
            Err(_) => crate::CleanupComponentOutcome::Unconfirmed(
                "root preparation cleanup remains in flight".into(),
            ),
        };
        if shared.is_confirmed() {
            preparations
        } else {
            crate::CleanupComponentOutcome::Unconfirmed(format!(
                "shared compiler {shared:?}; preparations {preparations:?}"
            ))
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
    pub(crate) async fn context(
        &self,
        actor: ActorRef,
    ) -> Result<Option<Arc<KernelContext>>, DirectoryError> {
        self.ask(|reply| RunControlMessage::Context { actor, reply })
            .await
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
