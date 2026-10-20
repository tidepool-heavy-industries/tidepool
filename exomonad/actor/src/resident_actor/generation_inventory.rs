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
