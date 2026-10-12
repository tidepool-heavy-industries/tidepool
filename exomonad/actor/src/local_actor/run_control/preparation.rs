//! Supervised preparation and acknowledged result delivery.
use super::*;

pub(crate) enum RootAdmissionOutcome {
    StartupPermit(super::RootStartupAdmission),
    Actor(LocalActorRef, ractor::concurrency::JoinHandle<()>),
    Workbench(LocalActorRef),
    Pending(
        LocalActorRef,
        ractor::concurrency::JoinHandle<()>,
        crate::RootStartupRelease,
    ),
    NativeResource(ResourceChild),
}
impl RootAdmissionOutcome {
    fn custody(&self) -> ProducedCustody {
        match self {
            Self::StartupPermit(_) => ProducedCustody::NoResource,
            Self::Actor(actor, _) | Self::Workbench(actor) | Self::Pending(actor, _, _) => {
                ProducedCustody::Resident(actor.clone())
            }
            Self::NativeResource(resource) => ProducedCustody::NativeResource(resource.clone()),
        }
    }
}
#[derive(Clone)]
enum ProducedCustody {
    Placement {
        placement: crate::ActorPlacement,
        cleanup: Arc<
            dyn Fn(crate::ActorPlacement) -> BoxFuture<'static, crate::CleanupComponentOutcome>
                + Send
                + Sync,
        >,
    },
    NoResource,
    Resident(LocalActorRef),
    NativeResource(ResourceChild),
}
impl ProducedCustody {
    async fn retire(self) -> crate::CleanupComponentOutcome {
        match self {
            Self::Placement { placement, cleanup } => cleanup(placement).await,
            Self::NoResource => crate::CleanupComponentOutcome::Confirmed,
            Self::Resident(actor) => {
                let cleanup =
                    crate::resident_actor::shutdown_forest_root(&actor, SHUTDOWN_BUDGET).await;
                if cleanup.is_confirmed() {
                    crate::CleanupComponentOutcome::Confirmed
                } else {
                    crate::CleanupComponentOutcome::Unconfirmed(format!(
                        "abandoned root cleanup: {cleanup:?}"
                    ))
                }
            }
            Self::NativeResource(resource) => {
                resource.cell.stop(None);
                match tokio::time::timeout(SHUTDOWN_BUDGET, async {
                    resource.cell.wait(Some(SHUTDOWN_BUDGET)).await?;
                    Ok::<_, ractor::concurrency::Timeout>(
                        (resource.cleanup)(ResourceCleanup::Retire).await,
                    )
                })
                .await
                {
                    Ok(Ok(outcome)) => outcome,
                    _ => crate::CleanupComponentOutcome::Unconfirmed(
                        "abandoned native resource cleanup is unconfirmed".into(),
                    ),
                }
            }
        }
    }
}
pub(crate) type RootOperationResult =
    Result<RootAdmissionOutcome, Box<dyn std::error::Error + Send + Sync>>;
pub(crate) type RootOperation = Box<
    dyn FnOnce(
            RootPreparationTicket,
        ) -> BoxFuture<'static, (RootOperationResult, crate::CleanupComponentOutcome)>
        + Send,
>;

/// The native placement issuer publishes facts against this admitted operation.
pub(crate) struct RootPreparationTicket {
    pub(crate) id: u64,
    owner: RactorRef<RunControlMessage>,
    supervisor: RactorRef<PreparationMessage>,
}
/// Immutable placement facts are published by the native mint operation.
#[derive(Clone)]
pub(crate) struct RootPlacementPublisher {
    id: u64,
    owner: RactorRef<RunControlMessage>,
    supervisor: RactorRef<PreparationMessage>,
    cleanup: Arc<
        dyn Fn(crate::ActorPlacement) -> BoxFuture<'static, crate::CleanupComponentOutcome>
            + Send
            + Sync,
    >,
}
impl RootPlacementPublisher {
    #[cfg(test)]
    pub(crate) async fn kill_producer_for_test(&self) -> Result<(), DirectoryError> {
        let (reply, receiver) = oneshot::channel();
        self.supervisor
            .send_message(PreparationMessage::KillRoot { id: self.id, reply })
            .map_err(|_| DirectoryError::Unavailable)?;
        receiver.await.map_err(|_| DirectoryError::Unavailable)
    }
    pub(crate) fn publish(&self, placement: crate::ActorPlacement) -> Result<(), DirectoryError> {
        self.supervisor
            .send_message(PreparationMessage::NativePlacement {
                id: self.id,
                custody: ProducedCustody::Placement {
                    placement,
                    cleanup: self.cleanup.clone(),
                },
            })
            .map_err(|_| DirectoryError::Unavailable)?;
        self.owner
            .send_message(RunControlMessage::PlacementPrepared {
                id: self.id,
                placement,
            })
            .map_err(|_| DirectoryError::Unavailable)
    }
    pub(crate) fn release(&self, placement: crate::ActorPlacement) {
        let _ = self
            .supervisor
            .send_message(PreparationMessage::NativePlacementReleased {
                id: self.id,
                placement,
            });
        let _ = self
            .owner
            .send_message(RunControlMessage::PlacementReleased {
                id: self.id,
                placement,
            });
    }
}
impl RootPreparationTicket {
    pub(crate) fn placement_publisher(
        &self,
        cleanup: Arc<
            dyn Fn(crate::ActorPlacement) -> BoxFuture<'static, crate::CleanupComponentOutcome>
                + Send
                + Sync,
        >,
    ) -> RootPlacementPublisher {
        RootPlacementPublisher {
            id: self.id,
            owner: self.owner.clone(),
            supervisor: self.supervisor.clone(),
            cleanup,
        }
    }
    pub(crate) fn placement_prepared(
        &self,
        placement: crate::ActorPlacement,
    ) -> Result<(), DirectoryError> {
        self.owner
            .send_message(RunControlMessage::PlacementPrepared {
                id: self.id,
                placement,
            })
            .map_err(|_| DirectoryError::Unavailable)
    }
    pub(crate) fn placement_released(
        &self,
        placement: crate::ActorPlacement,
    ) -> Result<(), DirectoryError> {
        let _ = self
            .supervisor
            .send_message(PreparationMessage::NativePlacementReleased {
                id: self.id,
                placement,
            });
        self.owner
            .send_message(RunControlMessage::PlacementReleased {
                id: self.id,
                placement,
            })
            .map_err(|_| DirectoryError::Unavailable)
    }
}

pub(super) struct RootDelivery {
    pub(super) id: u64,
    outcome: RootOperationResult,
    accepted: oneshot::Sender<()>,
}
impl RootDelivery {
    pub(super) fn accept(self) -> RootOperationResult {
        let _ = self.accepted.send(());
        self.outcome
    }
}

pub(super) enum PreparationMessage {
    #[cfg(test)]
    KillRoot {
        id: u64,
        reply: oneshot::Sender<()>,
    },
    NativePlacement {
        id: u64,
        custody: ProducedCustody,
    },
    NativePlacementReleased {
        id: u64,
        placement: crate::ActorPlacement,
    },
    RetireResources {
        owner: ResourceLifetime,
        resources: Vec<(ractor::ActorId, ResourceChild)>,
    },
    ReserveActor {
        identity: ActorRef,
        terminal: RetainedActorExit,
        predecessor: Option<crate::kernel::LifecycleHandle>,
    },
    Start {
        id: u64,
        operation: RootOperation,
        compiler: RetainedActorExit,
        reply: oneshot::Sender<RootDelivery>,
    },
    Produced {
        id: u64,
        custody: ProducedCustody,
        reply: oneshot::Sender<()>,
    },
    WorkerSettled {
        id: u64,
        outcome: crate::CleanupComponentOutcome,
    },
    ReservationReady {
        actor: ActorRef,
        lifecycle: Result<crate::kernel::LifecycleHandle, String>,
    },
    ResourcesSettled {
        owner: ResourceLifetime,
        outcomes: Vec<(ractor::ActorId, crate::CleanupComponentOutcome)>,
    },
    VerifyTerminal {
        worker: ractor::ActorId,
    },
    AbandonedCleanupSettled {
        id: u64,
        outcome: crate::CleanupComponentOutcome,
    },
    Cancel {
        id: u64,
    },
}
pub(super) struct PreparationSupervisor;
pub(super) struct PreparationState {
    control: RactorRef<RunControlMessage>,
    workers: HashMap<ractor::ActorId, TrackedWorker>,
}
enum WorkerKind {
    Root {
        id: u64,
        compiler: RetainedActorExit,
        produced: Option<ProducedCustody>,
        native: Option<ProducedCustody>,
    },
    Reserve {
        identity: ActorRef,
        terminal: RetainedActorExit,
    },
    Retire {
        owner: ResourceLifetime,
        resources: Vec<(ractor::ActorId, ResourceChild)>,
    },
    Abandoned {
        id: u64,
        custody: ProducedCustody,
    },
}
impl WorkerKind {
    // Completion reporting does not release an uncertain cleanup capability.
    // These records remain owned until confirmed run finalization stops us.
    fn retains_cleanup_custody(&self) -> bool {
        matches!(
            self,
            Self::Abandoned { .. }
                | Self::Root {
                    native: Some(_),
                    ..
                }
        )
    }
}

struct TrackedWorker {
    cell: ractor::ActorCell,
    kind: WorkerKind,
    terminal: bool,
    reported: bool,
}
impl Actor for PreparationSupervisor {
    type Msg = PreparationMessage;
    type State = PreparationState;
    type Arguments = RactorRef<RunControlMessage>;
    async fn pre_start(
        &self,
        _myself: RactorRef<Self::Msg>,
        control: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(PreparationState {
            control,
            workers: HashMap::new(),
        })
    }
    async fn handle(
        &self,
        myself: RactorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            #[cfg(test)]
            PreparationMessage::KillRoot { id, reply } => {
                for worker in state.workers.values() { if matches!(&worker.kind, WorkerKind::Root { id: issued, .. } if *issued == id) { worker.cell.kill(); } }
                let _ = reply.send(());
            }
            PreparationMessage::NativePlacement { id, custody } => {
                let mut reverify = Vec::new();
                for (identity, worker) in state.workers.iter_mut() {
                    if let WorkerKind::Root { id: issued, native, .. } = &mut worker.kind {
                        if *issued == id {
                            *native = Some(custody.clone());
                            if worker.terminal && !worker.reported { reverify.push(*identity); }
                        }
                    }
                }
                // An admitted blocking mint can publish after producer death.
                // Its retained native capability must still reach cleanup.
                for worker in reverify { let _ = myself.send_message(PreparationMessage::VerifyTerminal { worker }); }
            }
            PreparationMessage::NativePlacementReleased { id, placement } => {
                for worker in state.workers.values_mut() { if let WorkerKind::Root { id: issued, native, .. } = &mut worker.kind { if *issued == id && matches!(native, Some(ProducedCustody::Placement { placement: exact, .. }) if *exact == placement) { *native = None; } } }
            }
            PreparationMessage::RetireResources { owner, resources } => {
                let arguments = ResourceRetirementArguments { owner, resources: resources.clone(), supervisor: myself.clone() };
                match Actor::spawn_linked(None, ResourceRetirementWorker, arguments, myself.get_cell()).await {
                    Ok((worker, task)) => {
                        state.workers.insert(worker.get_id(), TrackedWorker { cell: worker.get_cell(), kind: WorkerKind::Retire { owner, resources }, terminal: false, reported: false });
                        drop(task); let _ = worker.send_message(());
                    }
                    Err(error) => { let _ = state.control.send_message(RunControlMessage::RunResourceWorkerFailed { owner, detail: format!("resource retirement worker failed to start: {error}") }); }
                }
            }
            PreparationMessage::ReserveActor { identity, terminal, predecessor } => {
                let arguments = ReservationArguments { identity, terminal: terminal.clone(), supervisor: myself.clone(), predecessor };
                match Actor::spawn_linked(None, ReservationStarter, arguments, myself.get_cell()).await {
                    Ok((worker, task)) => {
                        state.workers.insert(worker.get_id(), TrackedWorker { cell: worker.get_cell(), kind: WorkerKind::Reserve { identity, terminal }, terminal: false, reported: false });
                        drop(task); let _ = worker.send_message(());
                    }
                    Err(error) => { let _ = state.control.send_message(RunControlMessage::ReservationReady { actor: identity, lifecycle: Err(error.to_string()) }); }
                }
            }
            PreparationMessage::Start { id, operation, compiler, reply } => {
                let arguments = RootWorkerArguments { id, operation: Some(operation), compiler: compiler.clone(), reply: Some(reply), control: state.control.clone(), supervisor: myself.clone() };
                match Actor::spawn_linked(None, RootWorker, arguments, myself.get_cell()).await {
                    Ok((worker, task)) => {
                        state.workers.insert(worker.get_id(), TrackedWorker { cell: worker.get_cell(), kind: WorkerKind::Root { id, compiler, produced: None, native: None }, terminal: false, reported: false });
                        drop(task); let _ = worker.send_message(RootWorkerMessage::Execute);
                    }
                    Err(error) => { let _ = state.control.send_message(RunControlMessage::PreparationSettled { id, outcome: crate::CleanupComponentOutcome::Unconfirmed(format!("root preparation worker failed to start: {error}")) }); }
                }
            }
            PreparationMessage::Produced { id, custody, reply } => {
                if let Some(worker) = state.workers.values_mut().find(|worker| matches!(&worker.kind, WorkerKind::Root { id: issued, .. } if *issued == id)) {
                    if let WorkerKind::Root { produced, .. } = &mut worker.kind { *produced = Some(custody); }
                    let _ = reply.send(());
                }
            }
            PreparationMessage::Cancel { id } => {
                for worker in state.workers.values() { if let WorkerKind::Root { id: issued, compiler, .. } = &worker.kind { if *issued == id { compiler.request_shutdown(ActorTerminal::new(ActorExitKind::Cancelled, "forest preparation shutdown")); } } }
            }
            PreparationMessage::WorkerSettled { id, outcome } => {
                for worker in state.workers.values_mut() { if matches!(&worker.kind, WorkerKind::Root { id: issued, .. } if *issued == id) { worker.reported = true; } }
                let custody = state.workers.values_mut().find_map(|worker| match &mut worker.kind { WorkerKind::Root { id: issued, produced, native, .. } if *issued == id && produced.is_none() => native.take(), _ => None });
                if let Some(custody) = custody {
                    let compiler = state.workers.values().find_map(|worker| match &worker.kind { WorkerKind::Root { id: issued, compiler, .. } if *issued == id => Some(compiler.clone()), _ => None });
                    let arguments = AbandonedCleanupArguments { id, custody: custody.clone(), supervisor: myself.clone(), prior: outcome.clone(), compiler };
                    match Actor::spawn_linked(None, AbandonedCleanupWorker, arguments, myself.get_cell()).await {
                        Ok((cleanup, task)) => { state.workers.insert(cleanup.get_id(), TrackedWorker { cell: cleanup.get_cell(), kind: WorkerKind::Abandoned { id, custody }, terminal: false, reported: false }); drop(task); let _ = cleanup.send_message(()); }
                        Err(error) => {
                            for worker in state.workers.values_mut() { if let WorkerKind::Root { id: issued, native, .. } = &mut worker.kind { if *issued == id { *native = Some(custody.clone()); } } }
                            let _ = state.control.send_message(RunControlMessage::PreparationSettled { id, outcome: crate::CleanupComponentOutcome::Unconfirmed(format!("native placement cleanup worker failed to start: {error}; original {outcome:?}")) });
                        }
                    }
                } else { let _ = state.control.send_message(RunControlMessage::PreparationSettled { id, outcome }); }
            }
            PreparationMessage::ReservationReady { actor, lifecycle } => {
                for worker in state.workers.values_mut() { if matches!(&worker.kind, WorkerKind::Reserve { identity, .. } if *identity == actor) { worker.reported = true; } }
                if let Err(error) = state.control.send_message(RunControlMessage::ReservationReady { actor, lifecycle }) {
                    if let ractor::MessagingErr::SendErr(RunControlMessage::ReservationReady { lifecycle: Ok(lifecycle), .. }) = error { lifecycle.close_abandoned(); }
                }
            }
            PreparationMessage::ResourcesSettled { owner, outcomes } => {
                for worker in state.workers.values_mut() { if matches!(&worker.kind, WorkerKind::Retire { owner: issued, .. } if *issued == owner) { worker.reported = true; } }
                let _ = state.control.send_message(RunControlMessage::RunResourcesSettled { owner, outcomes });
            }
            PreparationMessage::AbandonedCleanupSettled { id, outcome } => {
                for worker in state.workers.values_mut() { if matches!(&worker.kind, WorkerKind::Abandoned { id: issued, .. } if *issued == id) { worker.reported = true; } }
                let _ = state.control.send_message(RunControlMessage::PreparationSettled { id, outcome });
            }
            PreparationMessage::VerifyTerminal { worker } => {
                let Some(record) = state.workers.get_mut(&worker) else { return Ok(()); };
                if record.reported {
                    if !record.kind.retains_cleanup_custody() { state.workers.remove(&worker); }
                    return Ok(());
                }
                match &mut record.kind {
                    WorkerKind::Root { id, compiler, produced, native } => {
                        compiler.request_shutdown(ActorTerminal::new(ActorExitKind::Cancelled, "root preparation worker lost"));
                        let id = *id;
                        if let Some(custody) = produced.take().or_else(|| native.take()) {
                            let arguments = AbandonedCleanupArguments { id, custody: custody.clone(), supervisor: myself.clone(), prior: crate::CleanupComponentOutcome::Unconfirmed("root producer exited before its completion receipt".into()), compiler: Some(compiler.clone()) };
                            match Actor::spawn_linked(None, AbandonedCleanupWorker, arguments, myself.get_cell()).await {
                                Ok((cleanup, task)) => { state.workers.insert(cleanup.get_id(), TrackedWorker { cell: cleanup.get_cell(), kind: WorkerKind::Abandoned { id, custody }, terminal: false, reported: false }); drop(task); let _ = cleanup.send_message(()); }
                                Err(error) => {
                                    if let Some(record) = state.workers.get_mut(&worker) { if let WorkerKind::Root { native, .. } = &mut record.kind { *native = Some(custody); } }
                                    let _ = state.control.send_message(RunControlMessage::PreparationSettled { id, outcome: crate::CleanupComponentOutcome::Unconfirmed(format!("abandoned root cleanup worker failed to start: {error}")) });
                                }
                            }
                        } else { let _ = state.control.send_message(RunControlMessage::PreparationLost { id }); }
                    }
                    WorkerKind::Reserve { identity, terminal } => {
                        terminal.request_shutdown(ActorTerminal::new(ActorExitKind::Cancelled, "actor reservation worker lost"));
                        let _ = state.control.send_message(RunControlMessage::ReservationReady { actor: *identity, lifecycle: Err("actor reservation worker exited without settlement".into()) });
                    }
                    WorkerKind::Retire { owner, .. } => { let _ = state.control.send_message(RunControlMessage::RunResourceWorkerFailed { owner: *owner, detail: "resource retirement worker exited without settlement".into() }); }
                    WorkerKind::Abandoned { id, .. } => { let _ = state.control.send_message(RunControlMessage::PreparationSettled { id: *id, outcome: crate::CleanupComponentOutcome::Unconfirmed("abandoned root cleanup worker exited without settlement".into()) }); }
                }
            }
        }
        state.workers.retain(|_, worker| {
            !(worker.terminal && worker.reported) || worker.kind.retains_cleanup_custody()
        });
        Ok(())
    }
    async fn handle_supervisor_evt(
        &self,
        myself: RactorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let cell = match event {
            SupervisionEvent::ActorTerminated(cell, _, _)
            | SupervisionEvent::ActorFailed(cell, _) => cell,
            _ => return Ok(()),
        };
        if let Some(worker) = state.workers.get_mut(&cell.get_id()) {
            worker.terminal = true;
            // Ordinary payload publication precedes worker termination, but
            // Ractor prioritizes this event. Verify behind queued payloads.
            let _ = myself.send_message(PreparationMessage::VerifyTerminal {
                worker: cell.get_id(),
            });
        }
        Ok(())
    }
}

struct RootWorker;
enum RootWorkerMessage {
    Execute,
}
struct RootWorkerArguments {
    id: u64,
    operation: Option<RootOperation>,
    compiler: RetainedActorExit,
    reply: Option<oneshot::Sender<RootDelivery>>,
    control: RactorRef<RunControlMessage>,
    supervisor: RactorRef<PreparationMessage>,
}
impl Actor for RootWorker {
    type Msg = RootWorkerMessage;
    type State = RootWorkerArguments;
    type Arguments = RootWorkerArguments;
    async fn pre_start(
        &self,
        _myself: RactorRef<Self::Msg>,
        arguments: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(arguments)
    }
    async fn handle(
        &self,
        myself: RactorRef<Self::Msg>,
        _: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let Some(operation) = state.operation.take() else {
            return Ok(());
        };
        let ticket = RootPreparationTicket {
            id: state.id,
            owner: state.control.clone(),
            supervisor: state.supervisor.clone(),
        };
        let owner =
            crate::resident_workbench::CompilerCloseOwner::ActorLifecycle(state.compiler.clone());
        let mut reply = state.reply.take().expect("root result has one recipient");
        let operation = std::panic::AssertUnwindSafe(owner.scope(operation(ticket))).catch_unwind();
        tokio::pin!(operation);
        let completed = tokio::select! {
            completed = &mut operation => completed,
            _ = reply.closed() => {
                state.compiler.request_shutdown(ActorTerminal::new(ActorExitKind::Cancelled, "root preparation recipient disappeared"));
                operation.await
            }
        };
        let outcome = match completed {
            Err(_) => crate::CleanupComponentOutcome::Unconfirmed(
                "root preparation panicked before cleanup settlement".into(),
            ),
            Ok((result, settled)) => {
                let settled = if let Some(cleanup) = result
                    .as_ref()
                    .err()
                    .and_then(|error| error.downcast_ref::<ractor::SpawnErr>())
                    .and_then(crate::local_actor::startup_cleanup)
                {
                    if cleanup.is_confirmed() {
                        settled
                    } else {
                        combine_cleanup(
                            settled,
                            crate::CleanupComponentOutcome::Unconfirmed(format!(
                                "root startup cleanup: {cleanup:?}"
                            )),
                        )
                    }
                } else {
                    settled
                };
                let custody = result.as_ref().ok().map(RootAdmissionOutcome::custody);
                if let Some(custody) = &custody {
                    let (retained, receipt) = oneshot::channel();
                    let published = state.supervisor.send_message(PreparationMessage::Produced {
                        id: state.id,
                        custody: custody.clone(),
                        reply: retained,
                    });
                    if published.is_err() || receipt.await.is_err() {
                        return Err("preparation supervisor lost produced custody".into());
                    }
                }
                let (accepted, acceptance) = oneshot::channel();
                let delivered = reply.send(RootDelivery {
                    id: state.id,
                    outcome: result,
                    accepted,
                });
                let accepted = if delivered.is_ok() {
                    tokio::select! { accepted = acceptance => accepted.is_ok(), _ = state.compiler.wait_requested_shutdown() => false }
                } else {
                    false
                };
                if accepted {
                    settled
                } else if let Some(custody) = custody {
                    custody.retire().await
                } else {
                    settled
                }
            }
        };
        let compilers = state.compiler.compiler_close_observations();
        let outcome = if compilers.iter().all(|close| close.is_confirmed()) {
            outcome
        } else {
            crate::CleanupComponentOutcome::Unconfirmed(format!(
                "root cleanup {outcome:?}; compiler close {compilers:?}"
            ))
        };
        let _ = state
            .supervisor
            .send_message(PreparationMessage::WorkerSettled {
                id: state.id,
                outcome,
            });
        myself.stop(None);
        Ok(())
    }
}

pub(super) struct ReservationStarter;
pub(super) struct ReservationArguments {
    pub(super) predecessor: Option<crate::kernel::LifecycleHandle>,
    pub(super) identity: ActorRef,
    pub(super) terminal: RetainedActorExit,
    pub(super) supervisor: RactorRef<PreparationMessage>,
}
impl Actor for ReservationStarter {
    type Msg = ();
    type State = ReservationArguments;
    type Arguments = ReservationArguments;
    async fn pre_start(
        &self,
        _myself: RactorRef<Self::Msg>,
        arguments: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(arguments)
    }
    async fn handle(
        &self,
        myself: RactorRef<Self::Msg>,
        _: (),
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let lifecycle = async {
            if let Some(predecessor) = &state.predecessor {
                let receipt =
                    tokio::time::timeout(SHUTDOWN_BUDGET, predecessor.retire_for_successor())
                        .await
                        .map_err(|_| "predecessor retirement remains in flight".to_owned())?
                        .map_err(|error| error.to_string())?;
                if receipt.predecessor() != predecessor.identity()
                    || !receipt.confirms_actor_retirement()
                {
                    return Err("predecessor retirement is unconfirmed".to_owned());
                }
            }
            if state.terminal.requested_shutdown().is_some() {
                return Err("actor reservation was closed before lifecycle startup".to_owned());
            }
            crate::kernel::LifecycleHandle::start_linked(
                state.identity,
                state.terminal.clone(),
                state.supervisor.get_cell(),
            )
            .await
            .map_err(|error| error.to_string())
        }
        .await;
        let _ = state
            .supervisor
            .send_message(PreparationMessage::ReservationReady {
                actor: state.identity,
                lifecycle,
            });
        myself.stop(None);
        Ok(())
    }
}

struct ResourceRetirementWorker;
struct ResourceRetirementArguments {
    owner: ResourceLifetime,
    resources: Vec<(ractor::ActorId, ResourceChild)>,
    supervisor: RactorRef<PreparationMessage>,
}
impl Actor for ResourceRetirementWorker {
    type Msg = ();
    type State = ResourceRetirementArguments;
    type Arguments = ResourceRetirementArguments;
    async fn pre_start(
        &self,
        _myself: RactorRef<Self::Msg>,
        arguments: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(arguments)
    }
    async fn handle(
        &self,
        myself: RactorRef<Self::Msg>,
        _: (),
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        for (_, resource) in &state.resources {
            resource.cell.stop(None);
        }
        let mut pending = FuturesUnordered::new();
        for (id, resource) in state.resources.drain(..) {
            pending.push(async move {
                let cleanup = tokio::time::timeout(SHUTDOWN_BUDGET, async {
                    resource.cell.wait(Some(SHUTDOWN_BUDGET)).await?;
                    Ok::<_, ractor::concurrency::Timeout>(
                        (resource.cleanup)(ResourceCleanup::Retire).await,
                    )
                })
                .await;
                let outcome = match cleanup {
                    Ok(Ok(outcome)) => outcome,
                    _ => crate::CleanupComponentOutcome::Unconfirmed(
                        "run resource retirement is unconfirmed".into(),
                    ),
                };
                (id, outcome)
            });
        }
        let mut outcomes = Vec::new();
        while let Some(outcome) = pending.next().await {
            outcomes.push(outcome);
        }
        let _ = state
            .supervisor
            .send_message(PreparationMessage::ResourcesSettled {
                owner: state.owner,
                outcomes,
            });
        myself.stop(None);
        Ok(())
    }
}

struct AbandonedCleanupWorker;
struct AbandonedCleanupArguments {
    prior: crate::CleanupComponentOutcome,
    compiler: Option<RetainedActorExit>,
    id: u64,
    custody: ProducedCustody,
    supervisor: RactorRef<PreparationMessage>,
}
impl Actor for AbandonedCleanupWorker {
    type Msg = ();
    type State = AbandonedCleanupArguments;
    type Arguments = AbandonedCleanupArguments;
    async fn pre_start(
        &self,
        _myself: RactorRef<Self::Msg>,
        arguments: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(arguments)
    }
    async fn handle(
        &self,
        myself: RactorRef<Self::Msg>,
        _: (),
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let outcome = combine_cleanup(state.prior.clone(), state.custody.clone().retire().await);
        let outcome = if state.compiler.as_ref().is_none_or(|compiler| {
            compiler
                .compiler_close_observations()
                .iter()
                .all(|close| close.is_confirmed())
        }) {
            outcome
        } else {
            crate::CleanupComponentOutcome::Unconfirmed(format!(
                "native cleanup {outcome:?}; compiler settlement remains unconfirmed"
            ))
        };
        let _ = state
            .supervisor
            .send_message(PreparationMessage::AbandonedCleanupSettled {
                id: state.id,
                outcome,
            });
        myself.stop(None);
        Ok(())
    }
}
