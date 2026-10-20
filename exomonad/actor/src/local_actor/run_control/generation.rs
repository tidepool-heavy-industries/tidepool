//! Generation provenance travels through the existing run-directory owners.
use super::*;
use crate::resident_workbench::generation_admission::{GenerationAdmissionError, GenerationFence};

#[derive(Debug, thiserror::Error)]
pub(crate) enum GenerationDirectoryError {
    #[error(transparent)]
    Admission(#[from] GenerationAdmissionError),
    #[error(transparent)]
    Directory(#[from] DirectoryError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GenerationResourceState {
    Starting,
    Running,
    Transferring,
    CleanupUnsupported,
    CleanupUnconfirmed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum GenerationResourceIdentity {
    Actor(ActorRef),
    Native {
        issued_to: ActorRef,
        id: ractor::ActorId,
    },
    Preparation(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GenerationResourceObservation {
    pub(crate) identity: GenerationResourceIdentity,
    pub(crate) state: GenerationResourceState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GenerationCollateralIdentity {
    Classified(GenerationResourceIdentity),
    UnclassifiedPhysical(ractor::ActorId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GenerationLifetimeEdge {
    PhysicalSupervision,
    PendingSupervision,
    ResourceOwnership,
    OwnershipTransfer,
    OwnershipTransferUnconfirmed,
}

/// An actual lifetime edge leaving the proposed dependent stop population.
/// Source independence does not authorize stopping the affected owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GenerationCollateral {
    pub(crate) stopping: GenerationResourceIdentity,
    pub(crate) affected: GenerationCollateralIdentity,
    pub(crate) edge: GenerationLifetimeEdge,
}

/// Issued by the directory after all pre-fence reservation messages. This is
/// observation of the actual population, never a caller-authored stop list.
pub(crate) struct GenerationPopulation {
    pub(crate) dependency: GenerationDependency,
    pub(crate) resources: Vec<GenerationResourceObservation>,
    pub(crate) collateral: Vec<GenerationCollateral>,
}

impl RunState {
    pub(super) fn generation_population(
        &self,
        dependency: GenerationDependency,
    ) -> GenerationPopulation {
        let mut resources = Vec::new();
        for (actor, record) in &self.reservations {
            if !record
                .dependency
                .as_ref()
                .is_some_and(|issued| issued.depends_on(&dependency))
            {
                continue;
            }
            if matches!(record.phase, ReservationPhase::NotStarted) {
                continue;
            }
            if let ReservationPhase::StartUnconfirmed(detail) = &record.phase {
                resources.push(GenerationResourceObservation {
                    identity: GenerationResourceIdentity::Actor(*actor),
                    state: GenerationResourceState::CleanupUnconfirmed(detail.clone()),
                });
                continue;
            }
            if let Some(entry) = self.entries.get(actor) {
                if entry.actor.terminal().get().is_none() {
                    let link_state = match record.executor_link {
                        ExecutorLink::Confirmed => None,
                        ExecutorLink::Pending => Some(GenerationResourceState::Starting),
                        ExecutorLink::Unconfirmed => {
                            Some(GenerationResourceState::CleanupUnconfirmed(
                                "executor physical supervision differs from its issued reservation"
                                    .into(),
                            ))
                        }
                    };
                    if let Some(state) = link_state {
                        resources.push(GenerationResourceObservation {
                            identity: GenerationResourceIdentity::Actor(*actor),
                            state,
                        });
                        continue;
                    }
                    resources.push(GenerationResourceObservation {
                        identity: GenerationResourceIdentity::Actor(*actor),
                        state: GenerationResourceState::Running,
                    });
                } else if !entry
                    .actor
                    .terminal()
                    .cleanup()
                    .is_some_and(|cleanup| cleanup.is_confirmed())
                {
                    resources.push(GenerationResourceObservation {
                        identity: GenerationResourceIdentity::Actor(*actor),
                        state: GenerationResourceState::CleanupUnconfirmed(
                            "actor terminal cleanup is not confirmed".into(),
                        ),
                    });
                }
            } else if record
                .terminal
                .cleanup()
                .is_some_and(|cleanup| cleanup.is_confirmed())
            {
                continue;
            } else if record.preparation_pending()
                || matches!(record.phase, ReservationPhase::Ready)
            {
                resources.push(GenerationResourceObservation {
                    identity: GenerationResourceIdentity::Actor(*actor),
                    state: GenerationResourceState::Starting,
                });
            } else {
                resources.push(GenerationResourceObservation {
                    identity: GenerationResourceIdentity::Actor(*actor),
                    state: GenerationResourceState::CleanupUnconfirmed(
                        "reservation has no confirmed terminal cleanup".into(),
                    ),
                });
            }
        }
        for (id, record) in &self.resources {
            if !record
                .dependency
                .as_ref()
                .is_some_and(|issued| issued.depends_on(&dependency))
            {
                continue;
            }
            let state = match record.owner {
                ResourceOwner::Retired { .. } => continue,
                ResourceOwner::Transfer { .. } => GenerationResourceState::Transferring,
                ResourceOwner::Owned(_) => GenerationResourceState::Running,
            };
            resources.push(GenerationResourceObservation {
                identity: GenerationResourceIdentity::Native {
                    issued_to: record.issued_to,
                    id: *id,
                },
                state,
            });
        }
        for (id, record) in &self.preparations {
            if !record
                .dependency
                .as_ref()
                .is_some_and(|issued| issued.depends_on(&dependency))
            {
                continue;
            }
            let state = match &record.phase {
                PreparationPhase::Running => GenerationResourceState::Starting,
                PreparationPhase::Accepted
                | PreparationPhase::Settled(crate::CleanupComponentOutcome::Confirmed) => continue,
                PreparationPhase::Settled(crate::CleanupComponentOutcome::Unsupported) => {
                    GenerationResourceState::CleanupUnsupported
                }
                PreparationPhase::Settled(crate::CleanupComponentOutcome::Unconfirmed(detail)) => {
                    GenerationResourceState::CleanupUnconfirmed(detail.clone())
                }
            };
            resources.push(GenerationResourceObservation {
                identity: GenerationResourceIdentity::Preparation(*id),
                state,
            });
        }
        let collateral = self.generation_collateral(&resources);
        GenerationPopulation {
            dependency,
            resources,
            collateral,
        }
    }

    fn generation_collateral(
        &self,
        population: &[GenerationResourceObservation],
    ) -> Vec<GenerationCollateral> {
        let captured: HashSet<_> = population
            .iter()
            .map(|resource| resource.identity.clone())
            .collect();
        let mut collateral = Vec::new();
        for resource in population {
            let cell = match resource.identity {
                GenerationResourceIdentity::Actor(actor) => self
                    .entries
                    .get(&actor)
                    .map(|entry| entry.actor.address().get_cell())
                    .or_else(|| self.reservations.get(&actor)?.executor.clone()),
                GenerationResourceIdentity::Native { id, .. } => self
                    .resources
                    .get(&id)
                    .map(|record| record.resource.cell.clone()),
                GenerationResourceIdentity::Preparation(_) => None,
            };
            if let Some(cell) = cell {
                let mut pending = cell.get_children();
                let mut visited = HashSet::new();
                while let Some(child) = pending.pop() {
                    let id = child.get_id();
                    if !visited.insert(id) {
                        continue;
                    }
                    pending.extend(child.get_children());
                    let identity = self
                        .identities
                        .runtime
                        .get(&id)
                        .copied()
                        .map(GenerationResourceIdentity::Actor)
                        .or_else(|| {
                            self.resources.get(&id).map(|record| {
                                GenerationResourceIdentity::Native {
                                    issued_to: record.issued_to,
                                    id,
                                }
                            })
                        });
                    let affected = match identity {
                        Some(identity) if captured.contains(&identity) => continue,
                        Some(identity) => GenerationCollateralIdentity::Classified(identity),
                        None => GenerationCollateralIdentity::UnclassifiedPhysical(id),
                    };
                    collateral.push(GenerationCollateral {
                        stopping: resource.identity.clone(),
                        affected,
                        edge: GenerationLifetimeEdge::PhysicalSupervision,
                    });
                }
            }
            if let GenerationResourceIdentity::Native { id, .. } = resource.identity {
                if self.resources.get(&id).is_some_and(|record| {
                    matches!(record.owner, ResourceOwner::Transfer { id, .. } if !self.resource_transfers.contains_key(&id))
                }) {
                    collateral.push(GenerationCollateral {
                        stopping: resource.identity.clone(),
                        affected: GenerationCollateralIdentity::Classified(resource.identity.clone()),
                        edge: GenerationLifetimeEdge::OwnershipTransferUnconfirmed,
                    });
                }
            }
            let GenerationResourceIdentity::Actor(parent) = resource.identity else {
                continue;
            };
            for transfer in self.worker_transfers.values() {
                if transfer.parent == parent {
                    collateral.push(GenerationCollateral {
                        stopping: resource.identity.clone(),
                        affected: GenerationCollateralIdentity::Classified(
                            GenerationResourceIdentity::Actor(transfer.child),
                        ),
                        edge: GenerationLifetimeEdge::OwnershipTransfer,
                    });
                }
            }
            for (child, record) in &self.reservations {
                let ownership = self
                    .entries
                    .get(child)
                    .map_or(record.planned_ownership, |entry| entry.spawn_ownership);
                if ownership != SpawnOwnership::Supervised(parent)
                    || matches!(record.phase, ReservationPhase::NotStarted)
                    || record
                        .terminal
                        .cleanup()
                        .is_some_and(|cleanup| cleanup.is_confirmed())
                {
                    continue;
                }
                let identity = GenerationResourceIdentity::Actor(*child);
                if !captured.contains(&identity) {
                    collateral.push(GenerationCollateral {
                        stopping: resource.identity.clone(),
                        affected: GenerationCollateralIdentity::Classified(identity),
                        edge: GenerationLifetimeEdge::PendingSupervision,
                    });
                }
            }
            for (id, record) in &self.resources {
                let edge = match record.owner {
                    ResourceOwner::Owned(ResourceLifetime::Actor(owner)) if owner == parent => {
                        GenerationLifetimeEdge::ResourceOwnership
                    }
                    ResourceOwner::Transfer { id, source }
                        if source == ResourceLifetime::Actor(parent)
                            || self.resource_transfers.get(&id).is_some_and(|transfer| {
                                transfer.destination == ResourceLifetime::Actor(parent)
                            }) =>
                    {
                        GenerationLifetimeEdge::OwnershipTransfer
                    }
                    _ => continue,
                };
                let identity = GenerationResourceIdentity::Native {
                    issued_to: record.issued_to,
                    id: *id,
                };
                if edge == GenerationLifetimeEdge::OwnershipTransfer
                    || !captured.contains(&identity)
                {
                    collateral.push(GenerationCollateral {
                        stopping: resource.identity.clone(),
                        affected: GenerationCollateralIdentity::Classified(identity),
                        edge,
                    });
                }
            }
        }
        collateral
    }
}

impl RunControlClient {
    /// Send under the gate; waiting for the directory and startup happens after
    /// releasing it. A subsequent fence observation cannot overtake this send.
    fn source_enqueue<T: Send + 'static>(
        &self,
        dependency: &SourceDependency,
        message: impl FnOnce(oneshot::Sender<T>) -> RunControlMessage,
    ) -> Result<oneshot::Receiver<T>, GenerationDirectoryError> {
        let (reply, receiver) = oneshot::channel();
        let message = message(reply);
        dependency
            .with_open_admission(|| self.owner.send_message(message))?
            .map_err(|_| DirectoryError::Unavailable)?;
        Ok(receiver)
    }

    pub(crate) async fn reserve_source_worker(
        &self,
        dependency: &SourceDependency,
        parent: ActorRef,
        incarnation: crate::Incarnation,
    ) -> Result<ActorReservation, GenerationDirectoryError> {
        let receiver =
            self.source_enqueue(dependency, |reply| RunControlMessage::ReserveWorker {
                parent,
                incarnation,
                dependency: Some(dependency.clone()),
                reply,
            })?;
        Ok(receiver.await.map_err(|_| DirectoryError::Unavailable)??)
    }

    pub(crate) async fn reserve_source_successor(
        &self,
        dependency: &SourceDependency,
        predecessor: ActorRef,
        incarnation: crate::Incarnation,
    ) -> Result<ActorReservation, GenerationDirectoryError> {
        let receiver =
            self.source_enqueue(dependency, |reply| RunControlMessage::ReserveSuccessor {
                predecessor,
                incarnation,
                dependency: Some(dependency.clone()),
                reply,
            })?;
        Ok(receiver.await.map_err(|_| DirectoryError::Unavailable)??)
    }

    pub(crate) async fn reserve_source_actor(
        &self,
        dependency: &SourceDependency,
        incarnation: crate::Incarnation,
    ) -> Result<ActorReservation, GenerationDirectoryError> {
        let receiver =
            self.source_enqueue(dependency, |reply| RunControlMessage::ReserveSourceActor {
                dependency: dependency.clone(),
                incarnation,
                reply,
            })?;
        Ok(receiver.await.map_err(|_| DirectoryError::Unavailable)??)
    }

    /// The reservation owner supplies provenance; callers cannot substitute a
    /// generation or omit it when preparing an already reserved worker.
    pub(crate) async fn prepare_worker_root(
        &self,
        reservation: ActorRef,
        operation: RootOperation,
    ) -> Result<RootOperationResult, DirectoryError> {
        self.prepare_owned_root(
            RootPreparationOwner::Worker(reservation),
            PreparationIntent::Creation,
            operation,
        )
        .await
    }

    pub(crate) async fn generation_population(
        &self,
        fence: &GenerationFence,
    ) -> Result<GenerationPopulation, DirectoryError> {
        self.ask(|reply| RunControlMessage::GenerationPopulation {
            dependency: fence.dependency().clone(),
            reply,
        })
        .await
    }
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Probe;
    impl Actor for Probe {
        type Msg = ();
        type State = Arc<AtomicUsize>;
        type Arguments = Arc<AtomicUsize>;

        async fn pre_start(
            &self,
            _myself: RactorRef<()>,
            stopped: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(stopped)
        }

        async fn post_stop(
            &self,
            _myself: RactorRef<()>,
            stopped: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            stopped.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn retain_native(state: &mut RunState, cell: ractor::ActorCell, issued_to: ActorRef) {
        state.resources.insert(
            cell.get_id(),
            OwnedResource {
                resource: ResourceChild {
                    cell,
                    cleanup: Arc::new(|_| {
                        Box::pin(async { crate::CleanupComponentOutcome::Confirmed })
                    }),
                },
                issued_to,
                owner: ResourceOwner::Owned(ResourceLifetime::Run),
                dependency: None,
            },
        );
    }

    fn observation(cell: &ractor::ActorCell, issued_to: ActorRef) -> GenerationResourceObservation {
        GenerationResourceObservation {
            identity: GenerationResourceIdentity::Native {
                issued_to,
                id: cell.get_id(),
            },
            state: GenerationResourceState::Running,
        }
    }

    #[tokio::test]
    async fn physical_capture_reports_independent_child_and_leaves_sibling_live() {
        let stopped = Arc::new(AtomicUsize::new(0));
        let (parent, parent_task) = Actor::spawn(None, Probe, stopped.clone()).await.unwrap();
        let (child, child_task) =
            Actor::spawn_linked(None, Probe, stopped.clone(), parent.get_cell())
                .await
                .unwrap();
        let (sibling, sibling_task) = Actor::spawn(None, Probe, stopped.clone()).await.unwrap();
        let actor = ActorRef::first(crate::ActorId(1));
        let independent = ActorRef {
            id: actor.id,
            incarnation: crate::Incarnation(2),
        };
        let mut state = RunState::default();
        retain_native(&mut state, parent.get_cell(), actor);
        retain_native(&mut state, child.get_cell(), independent);
        retain_native(&mut state, sibling.get_cell(), independent);
        let captured = [observation(&parent.get_cell(), actor)];
        let report = state.generation_collateral(&captured);
        assert_eq!(
            report,
            [GenerationCollateral {
                stopping: captured[0].identity.clone(),
                affected: GenerationCollateralIdentity::Classified(
                    observation(&child.get_cell(), independent).identity
                ),
                edge: GenerationLifetimeEdge::PhysicalSupervision,
            }]
        );
        assert_eq!(
            stopped.load(Ordering::SeqCst),
            0,
            "capture dispatches no stops"
        );
        assert_ne!(sibling.get_status(), ActorStatus::Stopped);
        child
            .stop_and_wait(None, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        parent
            .stop_and_wait(None, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        sibling
            .stop_and_wait(None, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        child_task.await.unwrap();
        parent_task.await.unwrap();
        sibling_task.await.unwrap();
        assert_eq!(stopped.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn physical_capture_traverses_captured_child_and_preserves_unclassified_grandchild() {
        let stopped = Arc::new(AtomicUsize::new(0));
        let (parent, parent_task) = Actor::spawn(None, Probe, stopped.clone()).await.unwrap();
        let (child, child_task) =
            Actor::spawn_linked(None, Probe, stopped.clone(), parent.get_cell())
                .await
                .unwrap();
        let (grandchild, grandchild_task) =
            Actor::spawn_linked(None, Probe, stopped.clone(), child.get_cell())
                .await
                .unwrap();
        let actor = ActorRef::first(crate::ActorId(1));
        let mut state = RunState::default();
        retain_native(&mut state, parent.get_cell(), actor);
        retain_native(&mut state, child.get_cell(), actor);
        let captured = [
            observation(&parent.get_cell(), actor),
            observation(&child.get_cell(), actor),
        ];
        let report = state.generation_collateral(&captured);
        assert_eq!(report.len(), 2);
        for (edge, stopping) in report.iter().zip(&captured) {
            assert_eq!(edge.stopping, stopping.identity);
            assert_eq!(
                edge.affected,
                GenerationCollateralIdentity::UnclassifiedPhysical(grandchild.get_id())
            );
        }
        assert_eq!(stopped.load(Ordering::SeqCst), 0);
        grandchild
            .stop_and_wait(None, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        child
            .stop_and_wait(None, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        parent
            .stop_and_wait(None, Some(Duration::from_secs(2)))
            .await
            .unwrap();
        grandchild_task.await.unwrap();
        child_task.await.unwrap();
        parent_task.await.unwrap();
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(16))]
        #[test]
        fn physical_capture_matches_independent_parent_index_oracle(
            nodes in proptest::collection::vec((proptest::bool::ANY, proptest::bool::ANY, 0usize..16), 1..7)
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            runtime.block_on(async {
                let stopped = Arc::new(AtomicUsize::new(0));
                let issued_to = ActorRef::first(crate::ActorId(1));
                let mut state = RunState::default();
                let mut actors: Vec<RactorRef<()>> = Vec::new();
                let mut tasks = Vec::new();
                let mut parents = Vec::new();
                let mut selected = Vec::new();
                let mut captured = Vec::new();
                for (index, (capture, known, parent_seed)) in nodes.iter().enumerate() {
                    let parent = if index == 0 || parent_seed % 2 == 0 {
                        None
                    } else {
                        Some(parent_seed % index)
                    };
                    let (actor, task) = if let Some(parent) = parent {
                        Actor::spawn_linked(None, Probe, stopped.clone(), actors[parent].get_cell()).await.unwrap()
                    } else {
                        Actor::spawn(None, Probe, stopped.clone()).await.unwrap()
                    };
                    let capture = index == 0 || *capture;
                    if capture || *known {
                        retain_native(&mut state, actor.get_cell(), issued_to);
                    }
                    if capture {
                        captured.push(observation(&actor.get_cell(), issued_to));
                    }
                    selected.push(capture);
                    parents.push(parent);
                    actors.push(actor);
                    tasks.push(task);
                }
                let mut expected = HashSet::new();
                for (stopping, capture) in selected.iter().enumerate() {
                    if !capture { continue; }
                    for affected in 0..nodes.len() {
                        if selected[affected] { continue; }
                        let mut cursor = parents[affected];
                        while let Some(parent) = cursor {
                            if parent == stopping {
                                expected.insert((stopping, affected));
                                break;
                            }
                            cursor = parents[parent];
                        }
                    }
                }
                let report = state.generation_collateral(&captured);
                let observed: HashSet<_> = report.iter().map(|edge| {
                    assert_eq!(edge.edge, GenerationLifetimeEdge::PhysicalSupervision);
                    let GenerationResourceIdentity::Native { id, .. } = edge.stopping else { unreachable!() };
                    let stopping = actors.iter().position(|actor| actor.get_id() == id).unwrap();
                    let id = match &edge.affected {
                        GenerationCollateralIdentity::Classified(GenerationResourceIdentity::Native { id, .. }) => *id,
                        GenerationCollateralIdentity::UnclassifiedPhysical(id) => *id,
                        _ => unreachable!(),
                    };
                    let affected = actors.iter().position(|actor| actor.get_id() == id).unwrap();
                    assert_eq!(matches!(&edge.affected, GenerationCollateralIdentity::UnclassifiedPhysical(_)), !nodes[affected].1);
                    (stopping, affected)
                }).collect();
                assert_eq!(observed, expected);
                assert_eq!(stopped.load(Ordering::SeqCst), 0);
                for actor in actors.iter().rev() {
                    actor.stop_and_wait(None, Some(Duration::from_secs(2))).await.unwrap();
                }
                for task in tasks { task.await.unwrap(); }
                assert_eq!(stopped.load(Ordering::SeqCst), nodes.len());
            });
        }
    }
}
