//! Read-only retirement projection from the existing resident owners.

use super::*;
use crate::request::routes::GenerationRouteObservation;
use crate::resident_workbench::generation_admission::GenerationDependency;

/// Issued by the resident environment. Keeping this view retains no admitted
/// work or cancellation capability beyond its exact coordinator's custody.
pub(crate) struct ResidentGenerationInventory {
    actors: Arc<Mutex<std::collections::HashMap<ActorRef, ResidentActorRecord>>>,
    requests: Arc<RequestRegistry>,
    coordinator: Option<Arc<InvocationWork>>,
}

pub(crate) struct GenerationWorkObservations {
    pub(crate) actors: Vec<ActorRef>,
    pub(crate) invocations: Vec<invocation_work::GenerationInvocationObservation>,
    pub(crate) routes: Vec<GenerationRouteObservation>,
}

impl ResidentGenerationInventory {
    /// Called by the serial retirement owner after closing the issued source.
    /// This projects existing membership without granting stop authority.
    pub(crate) fn observe(&self, dependency: &GenerationDependency) -> GenerationWorkObservations {
        let ledgers: Vec<_> = self
            .actors
            .lock()
            .iter()
            .map(|(actor, record)| (*actor, record.workbench_executions.clone()))
            .collect();
        // Retention publication locks work state before actor records. Release
        // the record guard before entering any ledger or invocation owner.
        let actors = ledgers.iter().map(|(actor, _)| *actor).collect();
        let mut invocations = Vec::new();
        for (_, ledger) in ledgers {
            invocations.extend(
                ledger
                    .lock()
                    .generation_observations(dependency, self.coordinator.as_ref()),
            );
        }
        let routes = self.requests.generation_routes(dependency);
        GenerationWorkObservations {
            actors,
            invocations,
            routes,
        }
    }
}

impl<H, O> ResidentEnvironment<H, O> {
    pub(super) fn generation_inventory(
        &self,
        coordinator: Option<Arc<InvocationWork>>,
    ) -> ResidentGenerationInventory {
        ResidentGenerationInventory {
            actors: self.actors.clone(),
            requests: self.requests.clone(),
            coordinator,
        }
    }
}

#[cfg(test)]
pub(crate) async fn assert_owner_observation_contract(dependency: GenerationDependency) {
    use crate::resident_workbench::generation_admission::{BootstrapWorld, SourceDependency};
    use invocation_work::GenerationInvocationState;
    use tidepool_runtime::session::PersistentSession;

    let fixture = invocation_work::tests::Fixture::start().await;
    let actor = fixture.actor.identity();
    let journal = Arc::new(Mutex::new(WorkbenchExecutions::default()));
    fixture.environment.actors.lock().insert(
        actor,
        ResidentActorRecord {
            installed_tools: Arc::default(),
            lifecycle: fixture.kernel.lifecycle().clone(),
            fresh_context_seed: None,
            workbench_executions: journal.clone(),
            interactive_policy_installed: false,
            descriptor: crate::ActorDescriptor::new(
                "inventory owner",
                crate::ActorPlacement {
                    session: tidepool_repr::SessionId(71),
                    lexical_scope: tidepool_codegen::scope::ScopeId::ROOT,
                    resource_scope: RealmId::ROOT,
                },
            ),
            bound_worktree: None,
            terminal: None,
            runtime_observation: Default::default(),
            scheduler_root: true,
            displays: Default::default(),
        },
    );
    let source = SourceDependency::Generation(dependency.clone());
    let coordinator_control = crate::WorkbenchExecutionControl::untracked();
    let sibling_control = crate::WorkbenchExecutionControl::untracked();
    let admit = |control: &Arc<crate::WorkbenchExecutionControl>| {
        let execution = control.execution_id(actor);
        WorkbenchExecutions::admit_source_invocation(
            &journal,
            actor,
            control.reservation_owner(actor).unwrap(),
            source.clone(),
            &WorkbenchRequest::from_cell_input("inventory control").with_execution_id(execution),
            None,
            Some(control),
        )
        .unwrap()
    };
    let coordinator = admit(&coordinator_control);
    let sibling = admit(&sibling_control);
    let pure_custody = WorkbenchExecutions::admit_callback_root(
        &journal,
        actor,
        RequestReservationOwner::Route(crate::WatchId(700)),
        source.clone(),
    )
    .unwrap();
    let neutral_custody = journal.lock().actor_scope_root(actor);
    fixture
        .environment
        .runner
        .compiler_scope(crate::resident_workbench::CompilerCloseOwner::Invocation {
            work: pure_custody.clone(),
            control: None,
        })
        .with_source_dependency(&source)
        .admit_work()
        .unwrap()
        .run(|_| ());
    let compiler_scope = fixture
        .environment
        .runner
        .compiler_scope(crate::resident_workbench::CompilerCloseOwner::Invocation {
            work: coordinator.clone(),
            control: Some(coordinator_control.clone()),
        })
        .with_source_dependency(&source);
    let pending = compiler_scope.admit_work().unwrap();

    let mut independent_session = PersistentSession::new(None, 1024);
    let independent_scope = independent_session
        .mint_notebook_scope_with_lease(None)
        .unwrap();
    let independent_world = BootstrapWorld::new(None);
    independent_world.adopt_scope(independent_scope).unwrap();
    let independent_source = SourceDependency::World(independent_world);
    let independent = WorkbenchExecutions::admit_callback_root(
        &journal,
        actor,
        RequestReservationOwner::Route(crate::WatchId(701)),
        independent_source.clone(),
    )
    .unwrap();
    let independent_pending = fixture
        .environment
        .runner
        .compiler_scope(crate::resident_workbench::CompilerCloseOwner::Invocation {
            work: independent.clone(),
            control: None,
        })
        .with_source_dependency(&independent_source)
        .admit_work()
        .unwrap();
    let fence = dependency.fence().unwrap();
    let observed = fixture
        .environment
        .generation_inventory(Some(coordinator.clone()))
        .observe(&dependency);
    assert_eq!(observed.actors, [actor]);
    assert!(observed.routes.is_empty());
    assert_eq!(
        observed.invocations.len(),
        2,
        "inert custody and independent work do not block A"
    );
    assert!(
        observed.invocations.iter().any(|observation| {
            observation.actor == actor
                && observation.reservation == sibling.reservation_owner()
                && observation.state == GenerationInvocationState::Active
        }),
        "the coordinator exemption is exact, not actor-wide"
    );
    assert!(
        observed.invocations.iter().any(|observation| {
            observation.actor == actor
                && observation.reservation == coordinator.reservation_owner()
                && matches!(
                    observation.state,
                    GenerationInvocationState::Compiler {
                        registration: 0,
                        close: crate::CompilerWorkClose::Pending
                    }
                )
        }),
        "the coordinator's physical compiler obligation remains visible"
    );
    assert!(fixture.actor.terminal().requested_shutdown().is_none());
    drop(independent_source.claim().unwrap());
    pending.run(|_| ());
    independent_pending.run(|_| ());
    fence.reopen().unwrap();
    for work in [
        coordinator,
        sibling,
        pure_custody,
        neutral_custody,
        independent,
    ] {
        fixture.cleanup(&work).await;
    }
    fixture.finish().await;
}
