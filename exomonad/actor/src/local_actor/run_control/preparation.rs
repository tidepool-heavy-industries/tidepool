//! Supervised preparation and acknowledged result delivery.
use super::*;

pub(crate) enum RootAdmissionOutcome {
    Actor(LocalActorRef, ractor::concurrency::JoinHandle<()>),
    Workbench(LocalActorRef),
    Pending(
        LocalActorRef,
        ractor::concurrency::JoinHandle<()>,
        crate::RootStartupRelease,
    ),
}
impl RootAdmissionOutcome {
    fn actor(&self) -> LocalActorRef {
        match self {
            Self::Actor(actor, _) | Self::Workbench(actor) | Self::Pending(actor, _, _) => {
                actor.clone()
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
}
impl RootPreparationTicket {
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
    ReserveActor {
        identity: ActorRef,
        terminal: RetainedActorExit,
    },
    Start {
        id: u64,
        operation: RootOperation,
        compiler: RetainedActorExit,
        reply: oneshot::Sender<RootDelivery>,
    },
    WorkerSettled {
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
    workers: HashMap<u64, PreparationWorker>,
}
struct PreparationWorker {
    cell: ractor::ActorCell,
    compiler: RetainedActorExit,
    settled: Option<crate::CleanupComponentOutcome>,
    terminal: bool,
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
            PreparationMessage::ReserveActor { identity, terminal } => {
                let arguments = ReservationArguments {
                    identity,
                    terminal,
                    control: state.control.clone(),
                };
                match Actor::spawn_linked(None, ReservationStarter, arguments, myself.get_cell())
                    .await
                {
                    Ok((worker, task)) => {
                        drop(task);
                        let _ = worker.send_message(());
                    }
                    Err(error) => {
                        let _ = state
                            .control
                            .send_message(RunControlMessage::ReservationReady {
                                actor: identity,
                                lifecycle: Err(error.to_string()),
                            });
                    }
                }
            }
            PreparationMessage::Start {
                id,
                operation,
                compiler,
                reply,
            } => {
                // This worker's pre_start only constructs state. It never asks
                // directory registration or starts native preparation there.
                let arguments = RootWorkerArguments {
                    id,
                    operation: Some(operation),
                    compiler: compiler.clone(),
                    reply: Some(reply),
                    control: state.control.clone(),
                    supervisor: myself.clone(),
                };
                match Actor::spawn_linked(None, RootWorker, arguments, myself.get_cell()).await {
                    Ok((worker, task)) => {
                        state.workers.insert(
                            id,
                            PreparationWorker {
                                cell: worker.get_cell(),
                                compiler,
                                settled: None,
                                terminal: false,
                            },
                        );
                        drop(task);
                        let _ = worker.send_message(RootWorkerMessage::Execute);
                    }
                    Err(error) => {
                        let _ = state
                            .control
                            .send_message(RunControlMessage::PreparationSettled {
                                id,
                                outcome: crate::CleanupComponentOutcome::Unconfirmed(format!(
                                    "root preparation worker failed to start: {error}"
                                )),
                            });
                    }
                }
            }
            PreparationMessage::Cancel { id } => {
                if let Some(worker) = state.workers.get(&id) {
                    worker.compiler.request_shutdown(ActorTerminal::new(
                        ActorExitKind::Cancelled,
                        "forest preparation shutdown",
                    ));
                }
            }
            PreparationMessage::WorkerSettled { id, outcome } => {
                if let Some(worker) = state.workers.get_mut(&id) {
                    worker.settled = Some(outcome.clone());
                }
                let _ = state
                    .control
                    .send_message(RunControlMessage::PreparationSettled { id, outcome });
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
        let (cell, failed) = match event {
            SupervisionEvent::ActorTerminated(cell, _, _) => (cell, false),
            SupervisionEvent::ActorFailed(cell, _) => (cell, true),
            _ => return Ok(()),
        };
        if let Some((id, worker)) = state
            .workers
            .iter_mut()
            .find(|(_, worker)| worker.cell.get_id() == cell.get_id())
        {
            worker.terminal = true;
            if failed && worker.settled.is_none() {
                worker.compiler.request_shutdown(ActorTerminal::new(
                    ActorExitKind::Cancelled,
                    "preparation worker failed",
                ));
                let _ = state
                    .control
                    .send_message(RunControlMessage::PreparationSettled {
                        id: *id,
                        outcome: crate::CleanupComponentOutcome::Unconfirmed(
                            "preparation worker failed before cleanup settlement".into(),
                        ),
                    });
            }
            // A normal stop may overtake the queued settlement message.
        }
        state.workers.retain(|_, worker| {
            !(worker.terminal
                && worker.settled.as_ref() == Some(&crate::CleanupComponentOutcome::Confirmed))
        });
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
                let actor = result.as_ref().ok().map(RootAdmissionOutcome::actor);
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
                } else if let Some(actor) = actor {
                    let cleanup =
                        crate::resident_actor::shutdown_forest_root(&actor, SHUTDOWN_BUDGET).await;
                    if cleanup.is_confirmed() {
                        crate::CleanupComponentOutcome::Confirmed
                    } else {
                        crate::CleanupComponentOutcome::Unconfirmed(format!(
                            "abandoned root cleanup: {cleanup:?}"
                        ))
                    }
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
    pub(super) identity: ActorRef,
    pub(super) terminal: RetainedActorExit,
    pub(super) control: RactorRef<RunControlMessage>,
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
        let lifecycle =
            crate::kernel::LifecycleHandle::start(state.identity, state.terminal.clone())
                .await
                .map_err(|error| error.to_string());
        let _ = state
            .control
            .send_message(RunControlMessage::ReservationReady {
                actor: state.identity,
                lifecycle,
            });
        myself.stop(None);
        Ok(())
    }
}
