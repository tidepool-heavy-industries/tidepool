//! Admission belongs to the generation issuer, independently of retained code.

use std::sync::Arc;

use tidepool_runtime::session::NotebookGenerationId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceDependency {
    World(BootstrapWorld),
    Generation(GenerationDependency),
    Unqualified,
}

#[derive(Clone, Debug)]
pub(crate) struct BootstrapWorld(Arc<BootstrapWorldOwner>);

#[derive(Debug)]
struct BootstrapWorldOwner {
    prerequisite: Option<GenerationDependency>,
    scope: std::sync::OnceLock<Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
    state: parking_lot::Mutex<WorldState>,
}

struct WorldState {
    phase: WorldPhase,
    claims: usize,
    unconfirmed: usize,
    uncertainty_fence: Option<tidepool_runtime::session::RuntimeSourceFence>,
    publication: Option<Arc<()>>,
}
impl std::fmt::Debug for WorldState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldState")
            .field("phase", &self.phase)
            .field("claims", &self.claims)
            .field("unconfirmed", &self.unconfirmed)
            .finish_non_exhaustive()
    }
}
#[derive(Debug)]
enum WorldPhase {
    Bootstrap,
    Publishing(GenerationDependency),
    Bound(GenerationDependency),
    Unavailable(GenerationDependency),
}

impl BootstrapWorld {
    /// Issued once by the actual actor capture owner, before publication.
    pub(crate) fn new(prerequisite: Option<GenerationDependency>) -> Self {
        Self(Arc::new(BootstrapWorldOwner {
            prerequisite,
            scope: std::sync::OnceLock::new(),
            state: parking_lot::Mutex::new(WorldState {
                phase: WorldPhase::Bootstrap,
                claims: 0,
                unconfirmed: 0,
                uncertainty_fence: None,
                publication: None,
            }),
        }))
    }
    pub(super) fn prerequisite(&self) -> &Option<GenerationDependency> {
        &self.0.prerequisite
    }

    pub(crate) fn adopt_scope(
        &self,
        scope: Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
    ) -> Result<(), GenerationAdmissionError> {
        scope.validate_source_issuer()?;
        if let Some(existing) = self.0.scope.get() {
            if !Arc::ptr_eq(existing, &scope) {
                return Err(GenerationAdmissionError::StaleSourceLease);
            }
            return Ok(());
        }
        self.0
            .scope
            .set(scope)
            .map_err(|_| GenerationAdmissionError::StaleSourceLease)
    }

    fn claim(&self) -> Result<SourceClaim, GenerationAdmissionError> {
        let mut state = self.0.state.lock();
        match &state.phase {
            WorldPhase::Bootstrap if state.unconfirmed == 0 => {
                let prerequisite = self
                    .0
                    .scope
                    .get()
                    .cloned()
                    .or_else(|| {
                        self.0
                            .prerequisite
                            .as_ref()
                            .map(|parent| parent.runtime_lease().clone())
                    })
                    .map(|lease| lease.admit_source())
                    .transpose()?;
                state.claims += 1;
                Ok(SourceClaim::Bootstrap(BootstrapClaim {
                    world: self.clone(),
                    settled: false,
                    prerequisite,
                }))
            }
            WorldPhase::Bound(dependency) => {
                let scope = dependency.runtime_lease().clone();
                drop(state);
                Ok(SourceClaim::Runtime(scope.admit_source()?))
            }
            WorldPhase::Bootstrap => Err(GenerationAdmissionError::BootstrapBusy {
                claims: state.claims,
                unconfirmed: state.unconfirmed,
            }),
            WorldPhase::Publishing(_) | WorldPhase::Unavailable(_) => {
                Err(GenerationAdmissionError::BootstrapUnavailable)
            }
        }
    }

    pub(super) fn begin_publication(
        &self,
        target: &GenerationDependency,
    ) -> Result<WorldPublication, GenerationAdmissionError> {
        let mut state = self.0.state.lock();
        if !matches!(state.phase, WorldPhase::Bootstrap) {
            return Err(GenerationAdmissionError::BootstrapUnavailable);
        }
        if state.claims != 0 || state.unconfirmed != 0 {
            return Err(GenerationAdmissionError::BootstrapBusy {
                claims: state.claims,
                unconfirmed: state.unconfirmed,
            });
        }
        let prerequisite = self
            .0
            .prerequisite
            .as_ref()
            .map(|parent| parent.runtime_lease().admit_source())
            .transpose()?;
        let scope = self
            .0
            .scope
            .get()
            .ok_or(GenerationAdmissionError::Unqualified)?;
        scope.validate_source_issuer()?;
        if !Arc::ptr_eq(scope, target.runtime_lease()) {
            return Err(GenerationAdmissionError::StaleSourceLease);
        }
        let fence = scope.source_admission().fence_if_idle()?;
        state.phase = WorldPhase::Publishing(target.clone());
        let attempt = Arc::new(());
        state.publication = Some(attempt.clone());
        Ok(WorldPublication {
            world: self.clone(),
            settlement: WorldPublicationSettlement::Armed {
                prerequisite,
                fence: super::PublicationFence::Held(fence),
            },
            visibility: WorldPublicationVisibility {
                world: self.clone(),
                target: target.clone(),
                attempt,
            },
        })
    }
}

impl PartialEq for BootstrapWorld {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for BootstrapWorld {}

pub(crate) enum SourceClaim {
    Runtime(tidepool_runtime::session::RuntimeSourceLeaseClaim),
    Bootstrap(BootstrapClaim),
}
impl SourceClaim {
    pub(crate) fn retain_unconfirmed(self) {
        match self {
            Self::Runtime(claim) => claim.retain_unconfirmed(),
            Self::Bootstrap(mut claim) => {
                if let Some(scope) = claim.world.0.scope.get() {
                    scope.retain_unconfirmed_source_custody();
                } else if let Some(parent) = &claim.world.0.prerequisite {
                    parent.0.scope.retain_unconfirmed_source_custody();
                }
                let mut state = claim.world.0.state.lock();
                state.claims -= 1;
                state.unconfirmed += 1;
                claim.settled = true;
                drop(state);
                if let Some(prerequisite) = claim.prerequisite.take() {
                    prerequisite.retain_unconfirmed();
                }
            }
        }
    }
}
pub(crate) struct BootstrapClaim {
    world: BootstrapWorld,
    settled: bool,
    prerequisite: Option<tidepool_runtime::session::RuntimeSourceLeaseClaim>,
}
impl Drop for BootstrapClaim {
    fn drop(&mut self) {
        if !self.settled {
            self.world.0.state.lock().claims -= 1;
        }
    }
}

/// The actual publisher owns this cutoff. Dropping it preserves uncertainty.
pub(super) struct WorldPublication {
    world: BootstrapWorld,
    settlement: WorldPublicationSettlement,
    visibility: WorldPublicationVisibility,
}

enum WorldPublicationSettlement {
    Armed {
        prerequisite: Option<tidepool_runtime::session::RuntimeSourceLeaseClaim>,
        fence: super::PublicationFence,
    },
    Settled,
    UnavailableRecorded,
}

/// Visibility of one prepared attempt, without its fence or settlement rights.
#[derive(Clone)]
pub(super) struct WorldPublicationVisibility {
    world: BootstrapWorld,
    target: GenerationDependency,
    attempt: Arc<()>,
}

impl WorldPublicationVisibility {
    pub(super) fn install(
        &self,
        installed: &super::InstalledToolsState,
        attempt: &Arc<super::ActorPublicationAttempt>,
        generation: Arc<super::ActorGeneration>,
    ) -> bool {
        let mut world = self.world.0.state.lock();
        let mut current = installed.0.lock();
        // `InstalledToolsRecord` keeps installation and reply custody together;
        // publication only owns the availability half.
        let current = &mut current.availability;
        let exact = world
            .publication
            .as_ref()
            .is_some_and(|issued| Arc::ptr_eq(issued, &self.attempt))
            && Arc::ptr_eq(&self.target.0, &generation.dependency().0)
            && Arc::ptr_eq(self.target.runtime_lease(), &generation.scope);
        let live = exact
            && matches!(&world.phase, WorldPhase::Publishing(_))
            && matches!(&*current, super::InstalledToolsAvailability::PublicationInProgress(issued) if Arc::ptr_eq(issued, attempt));
        if exact {
            world.phase = if live {
                WorldPhase::Bound(self.target.clone())
            } else {
                WorldPhase::Unavailable(self.target.clone())
            };
        }
        // A late callback records actual visibility, never operational authority.
        if matches!(&*current,
            super::InstalledToolsAvailability::PublicationInProgress(issued)
            | super::InstalledToolsAvailability::PublicationUnavailable(issued)
            if Arc::ptr_eq(issued, attempt))
        {
            *current = if live {
                super::InstalledToolsAvailability::PublicationVisible {
                    attempt: attempt.clone(),
                    generation,
                }
            } else {
                super::InstalledToolsAvailability::GenerationUnavailable(generation)
            };
        }
        live
    }
}

impl WorldPublication {
    pub(super) fn visibility(&self) -> WorldPublicationVisibility {
        self.visibility.clone()
    }

    pub(super) fn unavailable(&mut self, mark_installed: impl FnOnce()) {
        let WorldPublicationSettlement::Armed {
            prerequisite,
            fence,
        } = &mut self.settlement
        else {
            mark_installed();
            return;
        };
        {
            let mut state = self.world.0.state.lock();
            if state
                .publication
                .as_ref()
                .is_some_and(|issued| Arc::ptr_eq(issued, &self.visibility.attempt))
            {
                state.phase = WorldPhase::Unavailable(self.visibility.target.clone());
                if let super::PublicationFence::Held(held) =
                    std::mem::replace(fence, super::PublicationFence::ReopenAttempted)
                {
                    // A repeated fallback cannot erase a fence already retained.
                    state.uncertainty_fence.get_or_insert(held);
                }
            }
            // Installation has its own exact attempt compare, even if this
            // world's selection changed. Keep the world -> installation order.
            mark_installed();
        }
        if let Some(claim) = prerequisite.take() {
            claim.retain_unconfirmed();
        }
        self.settlement = WorldPublicationSettlement::UnavailableRecorded;
    }

    #[cfg(test)]
    pub(super) fn visible(&mut self, target: &GenerationDependency) {
        let mut state = self.world.0.state.lock();
        match &state.phase {
            WorldPhase::Publishing(expected) if Arc::ptr_eq(&expected.0, &target.0) => {}
            _ => panic!("initial world publication lost its exact owner"),
        }
        state.phase = WorldPhase::Bound(target.clone());
    }
    pub(super) fn confirmed_when(
        &mut self,
        current: impl for<'a> FnOnce(
            tidepool_runtime::session::RuntimeSourceReopen<'a>,
        ) -> Result<(), super::PublicationSettlementError>,
    ) -> Result<(), super::PublicationSettlementError> {
        let mut state = self.world.0.state.lock();
        if !state
            .publication
            .as_ref()
            .is_some_and(|issued| Arc::ptr_eq(issued, &self.visibility.attempt))
            || !matches!(&state.phase, WorldPhase::Bound(target) if target.same_prepared(&self.visibility.target))
        {
            return Err(super::PublicationSettlementError::SelectionChanged);
        }
        let WorldPublicationSettlement::Armed { fence, .. } = &mut self.settlement else {
            return Err(super::PublicationSettlementError::ReopenAttempted);
        };
        fence.reopen(self.visibility.target.runtime_lease(), current)?;
        state.publication = None;
        drop(state);
        // Last prerequisite custody must never drop under world/source locks.
        self.settlement = WorldPublicationSettlement::Settled;
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn confirmed(mut self) {
        self.confirmed_when(|reopen| {
            reopen.commit();
            Ok(())
        })
        .unwrap();
    }
    pub(super) fn refused_when(
        &mut self,
        current: impl for<'a> FnOnce(
            tidepool_runtime::session::RuntimeSourceReopen<'a>,
        ) -> Result<(), super::PublicationSettlementError>,
    ) -> Result<(), super::PublicationSettlementError> {
        let mut state = self.world.0.state.lock();
        if !state
            .publication
            .as_ref()
            .is_some_and(|issued| Arc::ptr_eq(issued, &self.visibility.attempt))
            || !matches!(state.phase, WorldPhase::Publishing(_))
        {
            return Err(super::PublicationSettlementError::SelectionChanged);
        }
        let WorldPublicationSettlement::Armed { fence, .. } = &mut self.settlement else {
            return Err(super::PublicationSettlementError::ReopenAttempted);
        };
        fence.reopen(self.visibility.target.runtime_lease(), current)?;
        state.phase = WorldPhase::Bootstrap;
        state.publication = None;
        drop(state);
        self.settlement = WorldPublicationSettlement::Settled;
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn refused(mut self) {
        self.refused_when(|reopen| {
            reopen.commit();
            Ok(())
        })
        .unwrap();
    }
}
impl Drop for WorldPublication {
    fn drop(&mut self) {
        self.unavailable(|| {});
    }
}

impl SourceDependency {
    pub(crate) fn source_lease(
        &self,
    ) -> Result<Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>, GenerationAdmissionError>
    {
        match self {
            Self::Generation(dependency) => Ok(dependency.0.scope.clone()),
            Self::World(world) => match &world.0.state.lock().phase {
                WorldPhase::Bootstrap => world
                    .0
                    .scope
                    .get()
                    .cloned()
                    .ok_or(GenerationAdmissionError::Unqualified),
                WorldPhase::Bound(dependency) => Ok(dependency.0.scope.clone()),
                _ => Err(GenerationAdmissionError::BootstrapUnavailable),
            },
            Self::Unqualified => Err(GenerationAdmissionError::Unqualified),
        }
    }

    pub(crate) fn adopt_final_scope<H, O>(
        &self,
        session: &mut tidepool_runtime::session::ResidentSession<H, O>,
        scope: tidepool_codegen::scope::ScopeId,
    ) -> Result<(), crate::ResidentActorWorkbenchError>
    where
        H: tidepool_effect::dispatch::DispatchEffect<O> + Send,
        O: tidepool_runtime::session::OutputSink + Sync,
    {
        if let Self::World(world) = self {
            if world.0.scope.get().is_none() {
                let lease = session
                    .adopt_initial_notebook_scope_with_source(
                        scope,
                        world
                            .prerequisite()
                            .as_ref()
                            .map(|p| p.runtime_lease().clone()),
                    )
                    .map_err(tidepool_runtime::session::ResidentError::Session)?;
                world.adopt_scope(lease)?;
            }
        }
        Ok(())
    }

    /// Actor bootstrap accounting supplements, but never replaces, the lower lease.
    pub(crate) fn claim_actor_world(
        &self,
    ) -> Result<Option<SourceClaim>, GenerationAdmissionError> {
        let Self::World(world) = self else {
            return Ok(None);
        };
        let mut state = world.0.state.lock();
        match state.phase {
            WorldPhase::Bootstrap if state.unconfirmed == 0 => {
                state.claims += 1;
                Ok(Some(SourceClaim::Bootstrap(BootstrapClaim {
                    world: world.clone(),
                    settled: false,
                    prerequisite: None,
                })))
            }
            WorldPhase::Bound(_) => Ok(None),
            _ => Err(GenerationAdmissionError::BootstrapUnavailable),
        }
    }

    pub(crate) fn claim(&self) -> Result<SourceClaim, GenerationAdmissionError> {
        let lease = match self {
            Self::World(world) => world.0.scope.get().cloned(),
            Self::Generation(generation) => Some(generation.0.scope.clone()),
            Self::Unqualified => None,
        };
        if let Some(lease) = &lease {
            lease.validate_source_issuer()?;
        }
        let claim = match self {
            Self::World(world) => world.claim(),
            Self::Generation(dependency) => Ok(SourceClaim::Runtime(
                dependency.runtime_lease().admit_source()?,
            )),
            Self::Unqualified => Err(GenerationAdmissionError::Unqualified),
        }?;
        if let Some(lease) = &lease {
            lease.validate_source_issuer()?;
        }
        Ok(claim)
    }

    /// Typed descendants may inherit only a selected world, never erase an
    /// unbound parent's prerequisite by creating their own first generation.
    pub(crate) fn selected_dependency(
        &self,
    ) -> Result<GenerationDependency, GenerationAdmissionError> {
        match self {
            Self::Generation(dependency) => Ok(dependency.clone()),
            Self::World(world) => match &world.0.state.lock().phase {
                WorldPhase::Bound(dependency) => Ok(dependency.clone()),
                _ => Err(GenerationAdmissionError::BootstrapUnavailable),
            },
            Self::Unqualified => Err(GenerationAdmissionError::Unqualified),
        }
    }

    /// Directory membership follows the issued source alias and exact lower
    /// ancestry, including bootstrap reservations recorded before publication.
    pub(crate) fn depends_on(&self, generation: &GenerationDependency) -> bool {
        match self {
            Self::Generation(dependency) => dependency.depends_on(generation),
            Self::World(world) => world.0.scope.get().map_or_else(
                || {
                    world
                        .0
                        .prerequisite
                        .as_ref()
                        .is_some_and(|parent| parent.depends_on(generation))
                },
                |scope| {
                    scope
                        .source_admission()
                        .depends_on(generation.runtime_admission())
                },
            ),
            Self::Unqualified => false,
        }
    }

    pub(crate) fn generation(&self) -> Option<&GenerationDependency> {
        match self {
            Self::Generation(dependency) => Some(dependency),
            Self::World(_) | Self::Unqualified => None,
        }
    }

    pub(crate) fn with_open_admission<T>(
        &self,
        admit: impl FnOnce() -> T,
    ) -> Result<T, GenerationAdmissionError> {
        match self {
            Self::World(world) => {
                let state = world.0.state.lock();
                match &state.phase {
                    WorldPhase::Bootstrap if state.unconfirmed == 0 => {
                        if let Some(scope) = world.0.scope.get() {
                            scope.with_open_source_admission(admit)
                        } else {
                            match &world.0.prerequisite {
                                Some(parent) => parent.with_open_admission(admit),
                                None => Ok(admit()),
                            }
                        }
                    }
                    WorldPhase::Bound(dependency) => {
                        let dependency = dependency.clone();
                        drop(state);
                        dependency.with_open_admission(admit)
                    }
                    _ => Err(GenerationAdmissionError::BootstrapUnavailable),
                }
            }
            Self::Generation(dependency) => dependency.with_open_admission(admit),
            Self::Unqualified => Err(GenerationAdmissionError::Unqualified),
        }
    }
}

#[derive(Clone)]
pub(crate) struct GenerationDependency(Arc<GenerationAdmissionOwner>);

struct GenerationAdmissionOwner {
    actor: crate::ActorRef,
    generation: NotebookGenerationId,
    _source: crate::CheckpointSourceLayer,
    admission: tidepool_runtime::session::RuntimeSourceAdmission,
    scope: Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
    prerequisite: Option<GenerationDependency>,
}

pub use tidepool_runtime::session::SourceAdmissionError as GenerationAdmissionError;

impl std::fmt::Debug for GenerationDependency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenerationDependency")
            .field("actor", &self.actor())
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}
impl PartialEq for GenerationDependency {
    fn eq(&self, other: &Self) -> bool {
        self.same_owner(other)
    }
}
impl Eq for GenerationDependency {}

impl GenerationDependency {
    pub(super) fn new(
        actor: crate::ActorRef,
        generation: NotebookGenerationId,
        source: crate::CheckpointSourceLayer,
        scope: &Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
        prerequisite: Option<GenerationDependency>,
    ) -> Self {
        Self(Arc::new(GenerationAdmissionOwner {
            actor,
            generation,
            _source: source,
            admission: scope.source_admission().clone(),
            scope: scope.clone(),
            prerequisite,
        }))
    }
    pub(crate) fn actor(&self) -> crate::ActorRef {
        self.0.actor
    }
    pub(crate) fn generation(&self) -> NotebookGenerationId {
        self.0.generation
    }
    pub(crate) fn same_owner(&self, other: &Self) -> bool {
        self.0.admission.same_owner(&other.0.admission)
    }
    pub(super) fn same_prepared(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) && Arc::ptr_eq(self.runtime_lease(), other.runtime_lease())
    }
    pub(crate) fn depends_on(&self, other: &Self) -> bool {
        self.0.admission.depends_on(&other.0.admission)
    }
    pub(crate) fn prerequisite(&self) -> &Option<GenerationDependency> {
        &self.0.prerequisite
    }
    pub(crate) fn runtime_admission(&self) -> &tidepool_runtime::session::RuntimeSourceAdmission {
        &self.0.admission
    }
    pub(crate) fn runtime_lease(
        &self,
    ) -> &Arc<tidepool_runtime::session::RuntimeLexicalScopeLease> {
        &self.0.scope
    }
    pub(crate) fn with_open_admission<T>(
        &self,
        admit: impl FnOnce() -> T,
    ) -> Result<T, GenerationAdmissionError> {
        self.0.scope.with_open_source_admission(admit)
    }
    pub(crate) fn fence(&self) -> Result<GenerationFence, GenerationAdmissionError> {
        Ok(GenerationFence {
            dependency: self.clone(),
            fence: self.0.admission.fence()?,
        })
    }
}

#[must_use]
pub(crate) struct GenerationFence {
    dependency: GenerationDependency,
    fence: tidepool_runtime::session::RuntimeSourceFence,
}

/// The retirement attempt retains the exact original even after reopening has
/// consumed its lower fence. A failed attempt cannot manufacture a fresh fence.
pub(crate) struct GenerationRetirementFence {
    state: GenerationRetirementFenceState,
}

enum GenerationRetirementFenceState {
    Held(GenerationFence),
    ReopenAttempted(GenerationDependency),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum GenerationRetirementFenceError {
    #[error("the original retirement fence was consumed by a reopen attempt")]
    ReopenAttempted,
    #[error(transparent)]
    Admission(#[from] GenerationAdmissionError),
}

impl GenerationRetirementFence {
    pub(crate) fn new(fence: GenerationFence) -> Self {
        Self {
            state: GenerationRetirementFenceState::Held(fence),
        }
    }

    pub(crate) fn dependency(&self) -> &GenerationDependency {
        match &self.state {
            GenerationRetirementFenceState::Held(fence) => fence.dependency(),
            GenerationRetirementFenceState::ReopenAttempted(dependency) => dependency,
        }
    }

    fn held(&self) -> Result<&GenerationFence, GenerationRetirementFenceError> {
        match &self.state {
            GenerationRetirementFenceState::Held(fence) => Ok(fence),
            GenerationRetirementFenceState::ReopenAttempted(_) => {
                Err(GenerationRetirementFenceError::ReopenAttempted)
            }
        }
    }

    pub(crate) fn pending_claims(&self) -> Result<usize, GenerationRetirementFenceError> {
        Ok(self.held()?.pending_claims())
    }

    pub(crate) async fn wait_for_claims(&self) -> Result<(), GenerationRetirementFenceError> {
        Ok(self.held()?.wait_for_claims().await?)
    }

    pub(crate) fn retire(
        self,
        successor: NotebookGenerationId,
    ) -> Result<(), GenerationRetirementFenceError> {
        match self.state {
            GenerationRetirementFenceState::Held(fence) => Ok(fence.retire(successor)?),
            GenerationRetirementFenceState::ReopenAttempted(_) => {
                Err(GenerationRetirementFenceError::ReopenAttempted)
            }
        }
    }

    pub(super) fn validate_held(&self) -> Result<(), super::PublicationSettlementError> {
        match self.state {
            GenerationRetirementFenceState::Held(_) => Ok(()),
            GenerationRetirementFenceState::ReopenAttempted(_) => {
                Err(super::PublicationSettlementError::ReopenAttempted)
            }
        }
    }

    pub(super) fn reopen_with(
        &mut self,
        settle: impl for<'a> FnOnce(
            tidepool_runtime::session::RuntimeSourceReopen<'a>,
        ) -> Result<(), super::PublicationSettlementError>,
    ) -> Result<(), super::PublicationSettlementError> {
        let replacement =
            GenerationRetirementFenceState::ReopenAttempted(self.dependency().clone());
        let GenerationRetirementFenceState::Held(fence) =
            std::mem::replace(&mut self.state, replacement)
        else {
            return Err(super::PublicationSettlementError::ReopenAttempted);
        };
        fence
            .fence
            .reopen_with(fence.dependency.runtime_lease(), settle)?
            .map(|_reopened| ())
    }
}

impl GenerationFence {
    pub(crate) fn dependency(&self) -> &GenerationDependency {
        &self.dependency
    }
    pub(crate) fn pending_claims(&self) -> usize {
        self.fence.pending_claims()
    }
    pub(crate) async fn wait_for_claims(&self) -> Result<(), GenerationAdmissionError> {
        self.fence.wait_for_claims().await
    }
    #[cfg(test)]
    pub(crate) fn reopen(self) -> Result<(), GenerationAdmissionError> {
        let lease = self.dependency.runtime_lease();
        self.fence
            .reopen_with(lease, |reopen| {
                reopen.commit();
                Ok::<_, ()>(())
            })
            .map(|settled| {
                settled.expect("an unconditional commit settles the fence");
            })
    }
    pub(crate) fn retire(
        self,
        successor: NotebookGenerationId,
    ) -> Result<(), GenerationAdmissionError> {
        self.fence.retire(successor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_runtime::session::PersistentSession;

    struct History {
        session: PersistentSession,
        scopes: Vec<Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
    }
    impl History {
        fn new() -> Self {
            Self {
                session: PersistentSession::new(None, 1024),
                scopes: Vec::new(),
            }
        }
    }

    fn generation(
        history: &mut History,
        id: NotebookGenerationId,
        prerequisite: Option<GenerationDependency>,
    ) -> GenerationDependency {
        let scope = history
            .session
            .mint_notebook_scope_with_lease(
                prerequisite
                    .as_ref()
                    .map(|parent| parent.runtime_lease().clone()),
            )
            .unwrap();
        let dependency = GenerationDependency::new(
            crate::ActorRef::first(crate::ActorId(71)),
            id,
            crate::CheckpointSourceLayer::default(),
            &scope,
            prerequisite,
        );
        history.scopes.push(scope);
        dependency
    }

    /// Owner-cluster controls use real canonical runtime leases and the actual
    /// publication/visibility entry. They allocate no compiler code or durable head.
    struct PublicationFixture {
        _history: History,
        preparation: Option<crate::CompilerPreparationOwner>,
        source: SourceDependency,
        state: Arc<super::super::InstalledToolsState>,
        generation: Arc<super::super::ActorGeneration>,
        expected: super::super::ActorRequestSnapshot,
    }

    impl PublicationFixture {
        fn new() -> Self {
            Self::with_prerequisite(false)
        }

        fn with_prerequisite(external: bool) -> Self {
            let mut history = History::new();
            let prerequisite = external.then(|| {
                let scope = history
                    .session
                    .mint_notebook_scope_with_lease(None)
                    .unwrap();
                let parent = GenerationDependency::new(
                    crate::ActorRef::first(crate::ActorId(99)),
                    NotebookGenerationId::INITIAL,
                    crate::CheckpointSourceLayer::default(),
                    &scope,
                    None,
                );
                history.scopes.push(scope);
                parent
            });
            let dependency = generation(
                &mut history,
                NotebookGenerationId::INITIAL,
                prerequisite.clone(),
            );
            let world = BootstrapWorld::new(prerequisite);
            world
                .adopt_scope(dependency.runtime_lease().clone())
                .unwrap();
            let source = SourceDependency::World(world);
            let placement = crate::ActorPlacement {
                session: tidepool_repr::SessionId(71),
                resource_scope: tidepool_codegen::suspension::RealmId::ROOT,
                lexical_scope: dependency.runtime_lease().scope(),
            };
            let descriptor = crate::ActorDescriptor::new("publication owner component", placement)
                .with_persistence_policy(crate::ActorPersistencePolicy::Ephemeral)
                .with_source_dependency(SourceDependency::Generation(dependency.clone()));
            let context = descriptor.session_context(dependency.actor());
            let owner = crate::resident_actor::WorkbenchPublicOwner::prepare_generation(
                &context,
                &descriptor,
                dependency.runtime_lease(),
                None,
            )
            .unwrap()
            .bind(None);
            let installation = super::super::InstalledToolLease::new(
                dependency.actor(),
                crate::CheckpointSourceLayer::default(),
                None,
            )
            .with_source_dependency(&source);
            let state = Arc::new(super::super::InstalledToolsState::default());
            state
                .install_bootstrap(
                    super::super::BootstrapExpectation::Empty,
                    installation.clone(),
                )
                .unwrap();
            let expected = super::super::ActorRequestSnapshot::Bootstrap(installation.clone());
            let generation = Arc::new(super::super::ActorGeneration {
                id: dependency.generation(),
                installation: installation
                    .with_source_dependency(&SourceDependency::Generation(dependency.clone())),
                context,
                scope: dependency.runtime_lease().clone(),
                public_owner: owner,
                prompts: tidepool_runtime::session::ComposedPromptSnapshot::capture(String::new()),
                configuration_digest: [0; 32],
                durable: None,
            });
            Self {
                _history: history,
                preparation: Some(crate::CompilerPreparationOwner::new()),
                source,
                state,
                generation,
                expected,
            }
        }

        fn after_confirmed_c() -> Self {
            Self::replacement(false)
        }

        fn replacement(external: bool) -> Self {
            let mut fixture = Self::with_prerequisite(external);
            let initial = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
            assert!(initial.visibility().install(fixture.generation.clone()));
            initial.confirmed().unwrap();
            let predecessor = fixture.generation.clone();
            let dependency = generation(
                &mut fixture._history,
                predecessor.id.next().unwrap(),
                predecessor.dependency().prerequisite().clone(),
            );
            let mut context = predecessor.context.clone();
            context.placement.lexical_scope = dependency.runtime_lease().scope();
            context.source_dependency = SourceDependency::Generation(dependency.clone());
            let descriptor =
                crate::ActorDescriptor::new("replacement publication component", context.placement)
                    .with_persistence_policy(crate::ActorPersistencePolicy::Ephemeral)
                    .with_source_dependency(context.source_dependency().clone());
            let public_owner = crate::resident_actor::WorkbenchPublicOwner::prepare_generation(
                &context,
                &descriptor,
                dependency.runtime_lease(),
                None,
            )
            .unwrap()
            .bind(None);
            fixture.generation = Arc::new(super::super::ActorGeneration {
                id: dependency.generation(),
                installation: predecessor
                    .installation
                    .clone()
                    .with_source_dependency(&SourceDependency::Generation(dependency.clone())),
                context,
                scope: dependency.runtime_lease().clone(),
                public_owner,
                prompts: predecessor.prompts.clone(),
                configuration_digest: [0; 32],
                durable: None,
            });
            fixture.source = SourceDependency::Generation(predecessor.dependency().clone());
            fixture.expected = super::super::ActorRequestSnapshot::Generation(predecessor);
            fixture
        }

        fn assert_candidate_unavailable(&self) {
            match self.generation.installation.source_dependency().claim() {
                Err(GenerationAdmissionError::Closed { scope, .. }) => {
                    assert_eq!(scope, self.generation.scope.scope());
                }
                Err(GenerationAdmissionError::Unconfirmed { scope, count }) => {
                    let parent = self
                        .generation
                        .dependency()
                        .prerequisite()
                        .as_ref()
                        .unwrap();
                    assert_eq!(scope, parent.runtime_lease().scope());
                    assert_eq!(count, 1);
                }
                _ => panic!("candidate or its exact prerequisite must refuse source admission"),
            }
            assert!(self.state.snapshot_for_call().is_err());
            assert!(self.state.current_generation().is_none());
        }

        fn reserve(
            &self,
            decision: Arc<tidepool_runtime::session::PublicationDecision>,
        ) -> super::super::ActorPublicationGuard {
            super::super::ActorPublicationGuard::reserve(
                self.state.clone(),
                self.expected.clone(),
                self.generation.dependency().clone(),
                &self.source,
                decision,
                super::super::ActorPublicationOwner::Preparation(
                    self.preparation.as_ref().unwrap().client(),
                ),
            )
            .unwrap()
            .unwrap()
        }

        fn assert_unavailable(&self) {
            assert!(self.source.claim().is_err());
            assert!(self.state.snapshot_for_call().is_err());
            assert!(matches!(
                self.state.observe(),
                super::super::InstalledToolsObservation::SourceUnavailable(_)
            ));
        }
    }

    #[test]
    fn retirement_refusal_settles_candidate_before_exact_original_reopen() {
        let fixture = PublicationFixture::replacement(true);
        let mut owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        let expected = fixture.expected.generation().unwrap().clone();
        let mut original = GenerationRetirementFence::new(expected.dependency().fence().unwrap());
        owner.refuse_retirement(&mut original).unwrap();
        assert!(Arc::ptr_eq(
            &fixture.state.current_generation().unwrap(),
            &expected
        ));
        assert!(expected.dependency().runtime_lease().admit_source().is_ok());
        assert!(fixture.generation.scope.admit_source().is_ok());
        assert!(matches!(
            original.pending_claims(),
            Err(GenerationRetirementFenceError::ReopenAttempted)
        ));
        assert!(owner.refuse_retirement(&mut original).is_err());
        drop(owner);
        assert!(Arc::ptr_eq(
            &fixture.state.current_generation().unwrap(),
            &expected
        ));
    }

    #[test]
    fn retirement_failed_candidate_settlement_keeps_original_fence_held() {
        let mut fixture = PublicationFixture::replacement(true);
        let mut owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        let expected = fixture.expected.generation().unwrap().clone();
        let mut original = GenerationRetirementFence::new(expected.dependency().fence().unwrap());
        drop(fixture.preparation.take());
        let error = owner.refuse_retirement(&mut original).unwrap_err();
        assert!(
            matches!(error, super::super::PublicationSettlementError::Retirement {
            stage: super::super::RetirementSettlementStage::Candidate,
            source,
        } if matches!(*source, super::super::PublicationSettlementError::OwnerUnavailable))
        );
        assert_eq!(original.pending_claims().unwrap(), 0);
        assert!(expected
            .dependency()
            .runtime_lease()
            .admit_source()
            .is_err());
        drop(owner);
        fixture.assert_candidate_unavailable();
    }

    #[test]
    fn retirement_unstarted_refusal_preserves_an_existing_original_fence() {
        let fixture = PublicationFixture::replacement(true);
        let mut owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        let expected = fixture.expected.generation().unwrap().clone();
        let held_elsewhere = expected.dependency().fence().unwrap();
        owner.refuse_unstarted().unwrap();
        assert!(Arc::ptr_eq(
            &fixture.state.current_generation().unwrap(),
            &expected
        ));
        assert!(expected.scope.admit_source().is_err());
        assert!(fixture.generation.scope.admit_source().is_ok());
        held_elsewhere.reopen().unwrap();
        assert!(expected.scope.admit_source().is_ok());
    }

    #[test]
    fn retirement_owner_loss_between_settlements_preserves_consumed_original_phase() {
        let mut fixture = PublicationFixture::replacement(true);
        let mut owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        let expected = fixture.expected.generation().unwrap().clone();
        let mut original = GenerationRetirementFence::new(expected.dependency().fence().unwrap());
        let (entered, started) = std::sync::mpsc::sync_channel(0);
        let (release, finish) = std::sync::mpsc::sync_channel(0);
        owner.settlement_probe = Some(Arc::new(super::super::PublicationSettlementProbe {
            entered,
            release: parking_lot::Mutex::new(finish),
            unwind: false,
        }));
        let preparation = fixture.preparation.take().unwrap();
        let target = fixture.generation.scope.clone();
        let closer = std::thread::spawn(move || {
            started
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            assert!(
                target.admit_source().is_ok(),
                "the candidate really settled first"
            );
            drop(preparation);
            release.send(()).unwrap();
        });
        let error = owner.refuse_retirement(&mut original).unwrap_err();
        closer.join().unwrap();
        assert!(
            matches!(error, super::super::PublicationSettlementError::Retirement {
            stage: super::super::RetirementSettlementStage::Original,
            source,
        } if matches!(*source, super::super::PublicationSettlementError::OwnerUnavailable))
        );
        assert!(matches!(
            original.pending_claims(),
            Err(GenerationRetirementFenceError::ReopenAttempted)
        ));
        assert!(expected
            .dependency()
            .runtime_lease()
            .admit_source()
            .is_err());
        assert!(owner.refuse_retirement(&mut original).is_err());
        drop(owner);
        assert!(fixture.state.current_generation().is_none());
        assert!(
            fixture.generation.scope.admit_source().is_ok(),
            "unselected settled candidate needs no invented rollback"
        );
    }

    #[test]
    fn retirement_terminal_clear_between_settlements_never_restores_original() {
        let fixture = PublicationFixture::replacement(true);
        let mut owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        let expected = fixture.expected.generation().unwrap().clone();
        let mut original = GenerationRetirementFence::new(expected.dependency().fence().unwrap());
        let (entered, started) = std::sync::mpsc::sync_channel(0);
        let (release, finish) = std::sync::mpsc::sync_channel(0);
        owner.settlement_probe = Some(Arc::new(super::super::PublicationSettlementProbe {
            entered,
            release: parking_lot::Mutex::new(finish),
            unwind: false,
        }));
        let state = fixture.state.clone();
        let clearer = std::thread::spawn(move || {
            started
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            drop(state.drain_retired());
            release.send(()).unwrap();
        });
        let error = owner.refuse_retirement(&mut original).unwrap_err();
        clearer.join().unwrap();
        assert!(
            matches!(error, super::super::PublicationSettlementError::Retirement {
            stage: super::super::RetirementSettlementStage::Original,
            source,
        } if matches!(*source, super::super::PublicationSettlementError::SelectionChanged))
        );
        assert!(matches!(
            original.pending_claims(),
            Err(GenerationRetirementFenceError::ReopenAttempted)
        ));
        drop(owner);
        assert!(matches!(
            &fixture.state.0.lock().availability,
            super::super::InstalledToolsAvailability::Retired
        ));
        assert!(expected.scope.admit_source().is_err());
    }

    #[test]
    fn retirement_unwind_after_candidate_settlement_keeps_original_held() {
        let fixture = PublicationFixture::replacement(true);
        let mut owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        let expected = fixture.expected.generation().unwrap().clone();
        let mut original = GenerationRetirementFence::new(expected.dependency().fence().unwrap());
        let (entered, started) = std::sync::mpsc::sync_channel(0);
        let (release, finish) = std::sync::mpsc::sync_channel(0);
        owner.settlement_probe = Some(Arc::new(super::super::PublicationSettlementProbe {
            entered,
            release: parking_lot::Mutex::new(finish),
            unwind: true,
        }));
        let observer = std::thread::spawn(move || {
            started
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            release.send(()).unwrap();
        });
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || owner.refuse_retirement(&mut original)
        ))
        .is_err());
        observer.join().unwrap();
        assert_eq!(original.pending_claims().unwrap(), 0);
        assert!(expected.scope.admit_source().is_err());
        assert!(fixture.generation.scope.admit_source().is_ok());
        drop(owner);
        assert!(fixture.state.current_generation().is_none());
        assert!(expected.scope.admit_source().is_err());
    }

    #[test]
    fn publisher_failed_settlement_retains_external_prerequisite_and_primary_outcome() {
        for bootstrap in [true, false] {
            for visible in [true, false] {
                let mut fixture = if bootstrap {
                    PublicationFixture::with_prerequisite(true)
                } else {
                    PublicationFixture::replacement(true)
                };
                let owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
                let parent = fixture
                    .generation
                    .dependency()
                    .prerequisite()
                    .as_ref()
                    .unwrap()
                    .clone();
                let parent_fence = parent.runtime_admission().fence().unwrap();
                assert_eq!(parent_fence.pending_claims(), 1);
                if visible {
                    assert!(owner.visibility().install(fixture.generation.clone()));
                }
                drop(fixture.preparation.take());
                let primary = if visible {
                    Ok(super::super::ActorGenerationCommit::Published {
                        generation: fixture.generation.clone(),
                        cancellation_pending: false,
                    })
                } else {
                    Err(crate::ResidentActorWorkbenchError::InvocationCancelled)
                };
                let Err(error @ crate::ResidentActorWorkbenchError::PublicationSettlement { .. }) =
                    owner.complete(primary)
                else {
                    panic!("failed restoration must retain both outcomes")
                };
                assert!(error.requires_recovery());
                assert_eq!(
                    error.execution_failure_kind(),
                    if visible {
                        None
                    } else {
                        Some(tidepool_runtime::session::PreparedFailureKind::Cancelled)
                    }
                );
                let crate::ResidentActorWorkbenchError::PublicationSettlement { failure } = error
                else {
                    unreachable!()
                };
                assert!(matches!(
                    failure.settlement,
                    super::super::PublicationSettlementError::OwnerUnavailable
                ));
                assert_eq!(parent_fence.pending_claims(), 1);
                assert_eq!(parent_fence.unconfirmed_claims(), 1);
                {
                    let state = fixture.state.0.lock();
                    assert!(
                        matches!(&state.availability,
                        super::super::InstalledToolsAvailability::GenerationUnavailable(_) if visible)
                            || matches!(&state.availability,
                        super::super::InstalledToolsAvailability::PublicationUnavailable(_) if !visible)
                    );
                }
                if let SourceDependency::World(world) = &fixture.source {
                    assert!(matches!(
                        world.0.state.lock().phase,
                        WorldPhase::Unavailable(_)
                    ));
                }
                fixture.assert_candidate_unavailable();
            }
        }
    }

    #[test]
    fn publisher_success_and_refusal_release_external_prerequisite_once() {
        for bootstrap in [true, false] {
            for visible in [true, false] {
                let fixture = if bootstrap {
                    PublicationFixture::with_prerequisite(true)
                } else {
                    PublicationFixture::replacement(true)
                };
                let owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
                let parent = fixture
                    .generation
                    .dependency()
                    .prerequisite()
                    .as_ref()
                    .unwrap()
                    .clone();
                let parent_fence = parent.runtime_admission().fence().unwrap();
                assert_eq!(parent_fence.pending_claims(), 1);
                if visible {
                    assert!(owner.visibility().install(fixture.generation.clone()));
                    owner.confirmed().unwrap();
                } else {
                    owner.refused().unwrap();
                }
                assert_eq!(parent_fence.pending_claims(), 0);
                assert_eq!(parent_fence.unconfirmed_claims(), 0);
                parent_fence
                    .reopen_with(parent.runtime_lease(), |reopen| {
                        reopen.commit();
                        Ok::<_, ()>(())
                    })
                    .unwrap()
                    .unwrap();
                assert!(fixture.state.snapshot_for_call().is_ok());
                fixture.source.claim().unwrap();
            }
        }
    }

    #[test]
    fn publisher_unwind_after_promotion_preserves_external_uncertainty() {
        for bootstrap in [true, false] {
            let fixture = if bootstrap {
                PublicationFixture::with_prerequisite(true)
            } else {
                PublicationFixture::replacement(true)
            };
            let mut owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
            assert!(owner.visibility().install(fixture.generation.clone()));
            let parent = fixture
                .generation
                .dependency()
                .prerequisite()
                .as_ref()
                .unwrap()
                .clone();
            let parent_fence = parent.runtime_admission().fence().unwrap();
            let (entered, observed) = std::sync::mpsc::sync_channel(0);
            let (release, released) = std::sync::mpsc::channel();
            owner.settlement_probe = Some(Arc::new(super::super::PublicationSettlementProbe {
                entered,
                release: parking_lot::Mutex::new(released),
                unwind: true,
            }));
            let settling = std::thread::spawn(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner.confirmed()))
            });
            observed
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            assert!(fixture.state.0.try_lock().is_none());
            release.send(()).unwrap();
            assert!(settling.join().unwrap().is_err());
            assert_eq!(parent_fence.pending_claims(), 1);
            assert_eq!(parent_fence.unconfirmed_claims(), 1);
            assert!(
                matches!(&fixture.state.0.lock().availability, super::super::InstalledToolsAvailability::GenerationUnavailable(selected) if Arc::ptr_eq(selected, &fixture.generation))
            );
            fixture.assert_candidate_unavailable();
        }
    }

    #[test]
    fn publisher_unobserved_body_and_uncertain_phase_preserve_primary_recovery_obligations() {
        for bootstrap in [true, false] {
            for body_observed in [true, false] {
                let fixture = if bootstrap {
                    PublicationFixture::with_prerequisite(true)
                } else {
                    PublicationFixture::replacement(true)
                };
                let decision = tidepool_runtime::session::PublicationDecision::new();
                let owner = fixture.reserve(decision.clone());
                let parent = fixture
                    .generation
                    .dependency()
                    .prerequisite()
                    .as_ref()
                    .unwrap()
                    .clone();
                let parent_fence = parent.runtime_admission().fence().unwrap();
                let primary = crate::ResidentActorWorkbenchError::InvocationCancelled;
                let error = if body_observed {
                    let _claim = decision.claim_commit().unwrap();
                    match owner.complete(Err(primary)) {
                        Err(error) => error,
                        Ok(_) => panic!("unconfirmed commit must not become ordinary cancellation"),
                    }
                } else {
                    owner.unobserved(primary)
                };
                assert!(error.requires_recovery());
                assert_eq!(
                    error.execution_failure_kind(),
                    Some(tidepool_runtime::session::PreparedFailureKind::Cancelled)
                );
                let crate::ResidentActorWorkbenchError::PublicationSettlement { failure } = error
                else {
                    panic!("both facts remain owned")
                };
                assert!(matches!(
                    failure.outcome,
                    Err(crate::ResidentActorWorkbenchError::InvocationCancelled)
                ));
                assert!(
                    matches!(failure.settlement,
                    super::super::PublicationSettlementError::PublicationUnconfirmed if body_observed)
                        || matches!(failure.settlement, super::super::PublicationSettlementError::BodyUnobserved if !body_observed)
                );
                assert_eq!(parent_fence.pending_claims(), 1);
                assert_eq!(parent_fence.unconfirmed_claims(), 1);
                assert!(matches!(
                    fixture.state.0.lock().availability,
                    super::super::InstalledToolsAvailability::PublicationUnavailable(_)
                ));
            }
        }
    }

    #[test]
    fn publisher_repeated_world_fallback_retains_fence_and_uncertainty_once() {
        let fixture = PublicationFixture::with_prerequisite(true);
        let mut owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        let SourceDependency::World(world) = &fixture.source else {
            unreachable!()
        };
        let parent = fixture
            .generation
            .dependency()
            .prerequisite()
            .as_ref()
            .unwrap()
            .clone();
        let parent_fence = parent.runtime_admission().fence().unwrap();
        let super::super::ActorPublicationState::Armed(authority) = &mut owner.settlement else {
            unreachable!()
        };
        authority.unavailable(|| {});
        authority.unavailable(|| {});
        assert!(world.0.state.lock().uncertainty_fence.is_some());
        drop(owner);
        assert!(world.0.state.lock().uncertainty_fence.is_some());
        assert_eq!(parent_fence.pending_claims(), 1);
        assert_eq!(parent_fence.unconfirmed_claims(), 1);
        assert!(matches!(
            fixture.state.0.lock().availability,
            super::super::InstalledToolsAvailability::PublicationUnavailable(_)
        ));
    }

    #[test]
    fn publisher_terminal_clear_before_confirmation_never_reopens_retained_target() {
        for bootstrap in [true, false] {
            let fixture = if bootstrap {
                PublicationFixture::new()
            } else {
                PublicationFixture::after_confirmed_c()
            };
            let owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
            assert!(owner.visibility().install(fixture.generation.clone()));
            drop(fixture.state.drain_retired());
            assert!(owner.confirmed().is_err());
            fixture.assert_candidate_unavailable();
        }
    }

    #[test]
    fn publisher_settlement_holds_installed_compare_through_gate_commit() {
        for bootstrap in [true, false] {
            let fixture = if bootstrap {
                PublicationFixture::new()
            } else {
                PublicationFixture::after_confirmed_c()
            };
            let mut owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
            assert!(owner.visibility().install(fixture.generation.clone()));
            let (entered, observed) = std::sync::mpsc::sync_channel(0);
            let (release, released) = std::sync::mpsc::channel();
            owner.settlement_probe = Some(Arc::new(super::super::PublicationSettlementProbe {
                entered,
                release: parking_lot::Mutex::new(released),
                unwind: false,
            }));
            let settling = std::thread::spawn(move || owner.confirmed());
            observed
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            // This observation occurs in the actual publisher callback, after
            // selection but before the same borrowed gate token commits.
            assert!(fixture.state.0.try_lock().is_none());
            let state = fixture.state.clone();
            let (clearing, started) = std::sync::mpsc::sync_channel(0);
            let clear = std::thread::spawn(move || {
                clearing.send(()).unwrap();
                drop(state.drain_retired());
            });
            started
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            release.send(()).unwrap();
            settling.join().unwrap().unwrap();
            clear.join().unwrap();
            assert!(fixture.state.snapshot_for_call().is_err());
        }
    }

    #[test]
    fn terminal_drain_racing_republication_never_repopulates() {
        // A bootstrap world can begin only one publication and is Bound after
        // its first install, so the contested republication is a generation
        // replacement. Bootstrap refusal after retirement is covered below.
        {
            let fixture = PublicationFixture::after_confirmed_c();
            let mut owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
            assert!(owner.visibility().install(fixture.generation.clone()));
            let (entered, observed) = std::sync::mpsc::sync_channel(0);
            let (release, released) = std::sync::mpsc::channel();
            owner.settlement_probe = Some(Arc::new(super::super::PublicationSettlementProbe {
                entered,
                release: parking_lot::Mutex::new(released),
                unwind: false,
            }));
            let settling = std::thread::spawn(move || owner.confirmed());
            observed
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();

            let drain_state = fixture.state.clone();
            let (draining, drain_started) = std::sync::mpsc::sync_channel(0);
            let drain = std::thread::spawn(move || {
                draining.send(()).unwrap();
                drop(drain_state.drain_retired());
            });
            drain_started
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();

            let republish_state = fixture.state.clone();
            let expected = fixture.expected.clone();
            let generation = fixture.generation.clone();
            let dependency = fixture.generation.dependency().clone();
            let source = fixture.source.clone();
            let preparation = fixture.preparation.as_ref().unwrap().client();
            let (republishing, republish_started) = std::sync::mpsc::sync_channel(0);
            let republish = std::thread::spawn(move || {
                republishing.send(()).unwrap();
                let owner = super::super::ActorPublicationGuard::reserve(
                    republish_state,
                    expected,
                    dependency,
                    &source,
                    tidepool_runtime::session::PublicationDecision::new(),
                    super::super::ActorPublicationOwner::Preparation(preparation),
                )
                .unwrap();
                if let Some(owner) = &owner {
                    owner.visibility().install(generation);
                }
                owner
            });
            republish_started
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();

            // Both terminal drain and republication wait on the settlement
            // owner's installation lock. Exactly one ordering wins, but the
            // terminal transition must dominate either outcome.
            release.send(()).unwrap();
            settling.join().unwrap().unwrap();
            drain.join().unwrap();
            if let Some(owner) = republish.join().unwrap() {
                assert!(
                    owner.confirmed().is_err(),
                    "a late publisher must not confirm after terminal drain"
                );
            }
            assert!(fixture.state.is_retired());
            assert!(fixture.state.snapshot_for_call().is_err());
            fixture.assert_candidate_unavailable();
        }
    }

    #[test]
    fn terminal_drain_refuses_bootstrap_reinstall_into_retired_state() {
        let fixture = PublicationFixture::new();
        drop(fixture.state.drain_retired());
        assert!(fixture.state.is_retired());
        assert_eq!(
            fixture.state.install_bootstrap(
                super::super::BootstrapExpectation::Empty,
                fixture.generation.installation.clone(),
            ),
            Err(super::super::BootstrapInstallationError::Retired)
        );
        assert!(fixture.state.snapshot_for_call().is_err());
        assert!(fixture.state.current_generation().is_none());
    }

    #[test]
    fn publisher_closed_actual_preparation_owner_prevents_confirmation_and_refusal_reopen() {
        for confirmed in [true, false] {
            let mut fixture = PublicationFixture::new();
            let owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
            if confirmed {
                assert!(owner.visibility().install(fixture.generation.clone()));
            }
            drop(fixture.preparation.take());
            if confirmed {
                assert!(owner.confirmed().is_err());
            } else {
                assert!(owner.refused().is_err());
            }
            fixture.assert_candidate_unavailable();
        }
    }

    #[test]
    fn publisher_terminal_clear_before_refusal_does_not_restore_bootstrap_aliases() {
        let fixture = PublicationFixture::new();
        let owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        drop(fixture.state.drain_retired());
        assert!(owner.refused().is_err());
        assert!(fixture.source.claim().is_err());
        fixture.assert_candidate_unavailable();
    }

    #[test]
    fn publisher_busy_target_refuses_without_fencing_original_admission() {
        let fixture = PublicationFixture::new();
        let claim = fixture.generation.scope.admit_source().unwrap();
        let result = super::super::ActorPublicationGuard::reserve(
            fixture.state.clone(),
            fixture.expected.clone(),
            fixture.generation.dependency().clone(),
            &fixture.source,
            tidepool_runtime::session::PublicationDecision::new(),
            super::super::ActorPublicationOwner::Preparation(
                fixture.preparation.as_ref().unwrap().client(),
            ),
        );
        assert!(matches!(
            result,
            Err(crate::ResidentActorWorkbenchError::SourceAdmission(
                GenerationAdmissionError::BootstrapBusy {
                    claims: 1,
                    unconfirmed: 0
                }
            ))
        ));
        assert!(fixture.state.snapshot_for_call().is_ok());
        fixture.source.claim().unwrap();
        drop(claim);
        let owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        owner.refused().unwrap();
        fixture.source.claim().unwrap();
    }

    #[test]
    fn publisher_replacement_visibility_waits_for_ack_before_selection_and_source() {
        let fixture = PublicationFixture::after_confirmed_c();
        // D is a real new canonical scope, not bootstrap C's already-owned fence.
        assert!(!fixture
            .generation
            .dependency()
            .depends_on(fixture.source.generation().unwrap()));
        let retained_d = fixture.generation.installation.source_dependency().clone();
        drop(retained_d.claim().unwrap());
        let owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        fixture.assert_candidate_unavailable();
        assert!(owner.visibility().install(fixture.generation.clone()));
        // Visibility records actual D while the actual ACK owner has not completed.
        fixture.assert_candidate_unavailable();
        assert!(matches!(
            owner.complete(Ok(super::super::ActorGenerationCommit::Published {
                generation: fixture.generation.clone(),
                cancellation_pending: false,
            })),
            Ok(super::super::ActorGenerationCommit::Published { .. })
        ));
        assert!(Arc::ptr_eq(
            &fixture.state.current_generation().unwrap(),
            &fixture.generation
        ));
        drop(retained_d.claim().unwrap());
    }

    #[test]
    fn publisher_replacement_unknown_after_visibility_never_opens_retained_d() {
        let fixture = PublicationFixture::after_confirmed_c();
        let retained_d = fixture.generation.installation.source_dependency().clone();
        let owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        assert!(owner.visibility().install(fixture.generation.clone()));
        fixture.assert_candidate_unavailable();
        assert!(matches!(
            owner.complete(Ok(super::super::ActorGenerationCommit::Unconfirmed(
                "post-visible acknowledgement unavailable".into()
            ))),
            Ok(super::super::ActorGenerationCommit::Unconfirmed(_))
        ));
        fixture.assert_candidate_unavailable();
        assert!(retained_d.claim().is_err());
        assert!(
            matches!(&fixture.state.0.lock().availability, super::super::InstalledToolsAvailability::GenerationUnavailable(selected)
            if Arc::ptr_eq(selected, &fixture.generation))
        );
    }

    #[test]
    fn publisher_cancel_after_current_check_keeps_late_visibility_unavailable() {
        let fixture = PublicationFixture::new();
        let decision = tidepool_runtime::session::PublicationDecision::new();
        let owner = fixture.reserve(decision.clone());
        let visibility = owner.visibility();
        let (checked, checked_rx) = std::sync::mpsc::sync_channel(0);
        let release = Arc::new(std::sync::Barrier::new(2));
        let callback_release = release.clone();
        let generation = fixture.generation.clone();
        let callback = std::thread::spawn(move || {
            assert!(visibility.is_current());
            checked.send(()).unwrap();
            callback_release.wait();
            visibility.install(generation)
        });
        checked_rx.recv().unwrap();
        drop(owner); // The async owner can disappear while native visibility is pending.
        fixture.assert_unavailable();
        assert!(decision.claim_commit().is_none());
        release.wait();
        assert!(!callback.join().unwrap());
        fixture.assert_unavailable();
        assert!(
            matches!(&fixture.state.0.lock().availability, super::super::InstalledToolsAvailability::GenerationUnavailable(selected)
            if Arc::ptr_eq(selected, &fixture.generation))
        );
    }

    #[test]
    fn publisher_cancel_after_visibility_retires_both_operational_views() {
        let fixture = PublicationFixture::new();
        let owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        let visibility = owner.visibility();
        let generation = fixture.generation.clone();
        let (visible, visible_rx) = std::sync::mpsc::sync_channel(0);
        let callback = std::thread::spawn(move || {
            assert!(visibility.install(generation));
            visible.send(()).unwrap();
        });
        visible_rx.recv().unwrap();
        drop(owner);
        callback.join().unwrap();
        fixture.assert_unavailable();
        assert!(
            matches!(&fixture.state.0.lock().availability, super::super::InstalledToolsAvailability::GenerationUnavailable(selected)
            if Arc::ptr_eq(selected, &fixture.generation))
        );
    }

    #[test]
    fn publisher_refusal_retry_rejects_stale_visibility_attempt() {
        let fixture = PublicationFixture::new();
        let owner = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        let stale = owner.visibility();
        assert!(matches!(
            owner.complete(Ok(super::super::ActorGenerationCommit::Refused(
                "known precommit refusal".into()
            ))),
            Ok(super::super::ActorGenerationCommit::Refused(_))
        ));
        drop(fixture.source.claim().unwrap());
        assert!(fixture.state.snapshot_for_call().is_ok());
        let retry = fixture.reserve(tidepool_runtime::session::PublicationDecision::new());
        assert!(!stale.install(fixture.generation.clone()));
        assert!(retry.visibility().is_current());
        assert!(retry.visibility().install(fixture.generation.clone()));
        retry.confirmed().unwrap();
        drop(fixture.source.claim().unwrap());
        assert!(Arc::ptr_eq(
            &fixture.state.current_generation().unwrap(),
            &fixture.generation
        ));
    }

    #[test]
    fn publisher_completed_error_needs_no_commit_proof_and_setup_panic_stays_closed() {
        for mode in 0..3 {
            let fixture = PublicationFixture::new();
            let decision = tidepool_runtime::session::PublicationDecision::new();
            let owner = fixture.reserve(decision.clone());
            if mode == 0 {
                assert!(owner
                    .complete(Err(crate::ResidentActorWorkbenchError::ActorProtocol(
                        "body precommit error".into()
                    )))
                    .is_err());
                drop(fixture.source.claim().unwrap());
                assert!(fixture.state.snapshot_for_call().is_ok());
            } else if mode == 1 {
                let claim = decision.claim_commit().unwrap();
                assert!(owner
                    .complete(Err(crate::ResidentActorWorkbenchError::ActorProtocol(
                        "unknown after claim".into()
                    )))
                    .is_err());
                fixture.assert_unavailable();
                drop(claim);
            } else {
                let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _owner = owner;
                    panic!("setup or body did not return a completed result");
                }));
                assert!(panic.is_err());
                fixture.assert_unavailable();
            }
        }
    }

    #[test]
    fn first_visible_world_closes_old_alias_after_replacement() {
        let mut session = History::new();
        let world = BootstrapWorld::new(None);
        let alias = SourceDependency::World(world.clone());
        let first = generation(&mut session, NotebookGenerationId::INITIAL, None);
        world.adopt_scope(first.0.scope.clone()).unwrap();
        // Preparing and discarding a candidate does not bind or fence aliases.
        drop(alias.claim().unwrap());
        let mut publication = world.begin_publication(&first).unwrap();
        assert!(alias.claim().is_err());
        publication.visible(&first);
        publication.confirmed();
        assert!(alias.selected_dependency().unwrap().same_owner(&first));
        let next = generation(
            &mut session,
            first.generation().next().unwrap(),
            first.prerequisite().clone(),
        );
        assert!(!next.depends_on(&first));
        first.fence().unwrap().retire(next.generation()).unwrap();
        assert!(alias.claim().is_err());
        drop(SourceDependency::Generation(next).claim().unwrap());
    }

    #[test]
    fn bootstrap_claim_and_unknown_end_prevent_publication_cutoff() {
        let mut session = History::new();
        let world = BootstrapWorld::new(None);
        let alias = SourceDependency::World(world.clone());
        let first = generation(&mut session, NotebookGenerationId::INITIAL, None);
        world.adopt_scope(first.0.scope.clone()).unwrap();
        let claim = alias.claim().unwrap();
        assert!(matches!(
            world.begin_publication(&first),
            Err(GenerationAdmissionError::BootstrapBusy { claims: 1, .. })
        ));
        drop(claim);
        let checkout = alias.claim().unwrap();
        assert!(world.begin_publication(&first).is_err());
        drop(checkout);
        alias.claim().unwrap().retain_unconfirmed();
        assert!(matches!(
            world.begin_publication(&first),
            Err(GenerationAdmissionError::BootstrapBusy { unconfirmed: 1, .. })
        ));
        assert!(alias.claim().is_err());
    }

    #[test]
    fn known_unpublished_retry_and_unknown_publication_are_distinct() {
        let mut session = History::new();
        let world = BootstrapWorld::new(None);
        let alias = SourceDependency::World(world.clone());
        let first = generation(&mut session, NotebookGenerationId::INITIAL, None);
        world.adopt_scope(first.0.scope.clone()).unwrap();
        world.begin_publication(&first).unwrap().refused();
        drop(alias.claim().unwrap());
        let different = GenerationDependency::new(
            first.actor(),
            first.generation().next().unwrap(),
            crate::CheckpointSourceLayer::default(),
            &first.0.scope,
            None,
        );
        drop(world.begin_publication(&different).unwrap());
        assert!(alias.claim().is_err());
        assert!(world.begin_publication(&first).is_err());
        let state = world.0.state.lock();
        assert!(
            matches!(&state.phase, WorldPhase::Unavailable(retained) if retained.same_owner(&different))
        );
    }

    #[test]
    fn typed_bootstrap_alias_tracks_child_while_parent_stays_open() {
        let mut session = History::new();
        let parent = generation(&mut session, NotebookGenerationId::INITIAL, None);
        let world = BootstrapWorld::new(Some(parent.clone()));
        let alias = SourceDependency::World(world.clone());
        let child = generation(
            &mut session,
            NotebookGenerationId::INITIAL,
            world.prerequisite().clone(),
        );
        world.adopt_scope(child.0.scope.clone()).unwrap();
        let checkout = alias.claim().unwrap();
        assert!(world.begin_publication(&child).is_err());
        assert_eq!(parent.fence().unwrap().pending_claims(), 1);
        drop(checkout);
        // A separate control owner is used below because dropping an affine
        // fence intentionally does not reopen the owner above.
        let parent = generation(&mut session, NotebookGenerationId::INITIAL, None);
        let world = BootstrapWorld::new(Some(parent.clone()));
        let alias = SourceDependency::World(world.clone());
        let child = generation(
            &mut session,
            NotebookGenerationId::INITIAL,
            world.prerequisite().clone(),
        );
        world.adopt_scope(child.0.scope.clone()).unwrap();
        let mut publication = world.begin_publication(&child).unwrap();
        publication.visible(&child);
        publication.confirmed();
        let next = generation(
            &mut session,
            child.generation().next().unwrap(),
            child.prerequisite().clone(),
        );
        child.fence().unwrap().retire(next.generation()).unwrap();
        assert!(alias.claim().is_err());
        assert!(alias.depends_on(&child));
        assert!(alias.with_open_admission(|| ()).is_err());
        drop(SourceDependency::Generation(parent).claim().unwrap());
        drop(SourceDependency::Generation(next).claim().unwrap());
    }

    #[test]
    fn typed_initial_publication_retains_external_claim_through_visibility() {
        let mut session = History::new();
        let parent = generation(&mut session, NotebookGenerationId::INITIAL, None);
        let world = BootstrapWorld::new(Some(parent.clone()));
        let child = generation(
            &mut session,
            NotebookGenerationId::INITIAL,
            world.prerequisite().clone(),
        );
        world.adopt_scope(child.0.scope.clone()).unwrap();
        let mut publication = world.begin_publication(&child).unwrap();
        let fence = parent.fence().unwrap();
        assert_eq!(fence.pending_claims(), 1);
        publication.visible(&child);
        publication.confirmed();
        assert_eq!(fence.pending_claims(), 0);
        assert!(SourceDependency::World(world).claim().is_err());
        let late_world = BootstrapWorld::new(Some(parent));
        assert!(late_world.begin_publication(&child).is_err());
    }

    #[tokio::test]
    async fn actual_compiler_ticket_retains_uncertainty_at_generation_fence() {
        let mut session = History::new();
        let dependency = generation(&mut session, NotebookGenerationId::INITIAL, None);
        let services = crate::capabilities::WorkbenchServices::new();
        let scope = services
            .with_compiler_owner(super::super::CompilerCloseOwner::ActorLifecycle(
                crate::RetainedActorExit::new(),
            ))
            .with_source_dependency(&SourceDependency::Generation(dependency.clone()));
        let ticket = scope.admit_work().unwrap();
        let fence = dependency.fence().unwrap();
        assert_eq!(fence.pending_claims(), 1);
        ticket.run(|close| {
            close(tidepool_runtime::CompilerTransactionClose::Unconfirmed(
                tidepool_extract_cmd::CompilerTransactionCloseEvidence::new(
                    tidepool_extract_cmd::CompilerTransactionCloseReason::Cancelled,
                    tidepool_extract_cmd::CompilerTransactionRetirement::DaemonUnobserved {
                        disconnect: None,
                    },
                ),
            ))
        });
        assert!(matches!(
            fence.wait_for_claims().await,
            Err(GenerationAdmissionError::Unconfirmed { .. })
        ));
        assert!(fence.reopen().is_err());
        assert!(scope.admit_work().is_err());
    }

    #[test]
    fn source_callback_transfer_keeps_dependency_without_generation_metadata() {
        let mut session = History::new();
        let dependency = generation(&mut session, NotebookGenerationId::INITIAL, None);
        let actor = crate::ActorRef::first(crate::ActorId(71));
        let placement = crate::ActorPlacement {
            session: tidepool_repr::SessionId(71),
            resource_scope: tidepool_codegen::suspension::RealmId::ROOT,
            lexical_scope: tidepool_codegen::scope::ScopeId::ROOT,
        };
        let context = crate::ActorDescriptor::new("source", placement)
            .with_source_dependency(SourceDependency::Generation(dependency.clone()))
            .session_context(actor);
        let from_context =
            crate::request::sources::CallbackExecution::from_context(context.clone());
        let execution = crate::request::sources::CallbackExecution {
            context,
            snapshot: Some(super::super::ActorRequestSnapshot::SourceCallback(
                super::super::InstalledToolLease::source_callback_custody(
                    actor,
                    crate::CheckpointSourceLayer::default(),
                    SourceDependency::Generation(dependency.clone()),
                ),
            )),
            scope: None,
        };
        let recipient = crate::ActorDescriptor::new("recipient", placement)
            .admit_independent_world()
            .session_context(crate::ActorRef::first(crate::ActorId(72)));
        let transferred = execution.transferred(recipient).unwrap();
        assert!(transferred
            .snapshot
            .as_ref()
            .unwrap()
            .generation()
            .is_none());
        assert!(transferred
            .source_dependency()
            .generation()
            .unwrap()
            .same_owner(&dependency));
        dependency.fence().unwrap();
        assert!(transferred.admit_activation().is_err());
        assert!(from_context.admit_activation().is_err());
        assert!(transferred
            .activation_context()
            .source_dependency()
            .claim()
            .is_err());
    }

    #[test]
    fn typed_child_replacement_preserves_external_parent_only() {
        let mut session = History::new();
        let parent = generation(&mut session, NotebookGenerationId::INITIAL, None);
        let child = generation(
            &mut session,
            NotebookGenerationId::INITIAL,
            Some(parent.clone()),
        );
        let next = generation(
            &mut session,
            child.generation().next().unwrap(),
            child.prerequisite().clone(),
        );
        assert!(next.depends_on(&parent));
        assert!(!next.depends_on(&child));
        assert!(Arc::ptr_eq(
            &child.runtime_lease().prerequisites()[0],
            parent.runtime_lease()
        ));
        assert!(Arc::ptr_eq(
            &next.runtime_lease().prerequisites()[0],
            parent.runtime_lease()
        ));
        let imported = generation(
            &mut session,
            NotebookGenerationId::INITIAL,
            Some(child.clone()),
        );
        assert!(Arc::ptr_eq(
            &imported.runtime_lease().prerequisites()[0],
            child.runtime_lease()
        ));
        assert!(imported.depends_on(&parent) && imported.depends_on(&child));
        let independent = generation(&mut session, NotebookGenerationId::INITIAL, None);
        parent.fence().unwrap();
        assert!(SourceDependency::Generation(next).claim().is_err());
        drop(SourceDependency::Generation(independent).claim().unwrap());
        assert!(SourceDependency::World(BootstrapWorld::new(None))
            .selected_dependency()
            .is_err());
    }

    struct PopulationBehavior {
        start_entered: Option<tokio::sync::oneshot::Sender<()>>,
        start_release: Option<tokio::sync::oneshot::Receiver<()>>,
        stopped_entered: Option<tokio::sync::oneshot::Sender<()>>,
        stopped_release: Option<tokio::sync::oneshot::Receiver<()>>,
        realm: crate::CleanupComponentOutcome,
    }

    impl crate::KernelBehavior for PopulationBehavior {
        fn start<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
        ) -> futures_util::future::BoxFuture<
            'a,
            Result<crate::KernelStep<()>, crate::KernelBehaviorError>,
        > {
            let entered = self.start_entered.take().unwrap();
            let release = self.start_release.take().unwrap();
            Box::pin(async move {
                entered.send(()).unwrap();
                release.await.unwrap();
                Ok(crate::KernelStep::Stop {
                    output: (),
                    terminal: crate::ActorTerminal::new(
                        crate::ActorExitKind::Completed,
                        "population actor completed during startup",
                    ),
                })
            })
        }

        fn cast<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
            _sender: crate::ActorRef,
            _request: crate::MailboxValue,
        ) -> futures_util::future::BoxFuture<
            'a,
            Result<crate::KernelStep<()>, crate::KernelBehaviorError>,
        > {
            unreachable!("population fixture runs only startup and cleanup")
        }

        fn call<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
            _caller: crate::ActorRef,
            _ancestry: crate::CallAncestry,
            _request: crate::MailboxValue,
        ) -> futures_util::future::BoxFuture<
            'a,
            Result<crate::KernelStep<crate::MailboxValue>, crate::KernelBehaviorError>,
        > {
            unreachable!("population fixture runs only startup and cleanup")
        }

        fn tool<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
            _invocation: exomonad_tool::ToolInvocation,
            _capture: Option<Arc<dyn crate::HostedCheckpointCapture>>,
        ) -> futures_util::future::BoxFuture<
            'a,
            Result<crate::KernelStep<serde_json::Value>, crate::KernelInvocationFailure>,
        > {
            unreachable!("population fixture runs only startup and cleanup")
        }

        fn dispatch_workbench(
            &mut self,
            _context: &crate::KernelContext,
            _invocation: crate::ActorWorkbenchInvocation,
            _control: Option<Arc<crate::WorkbenchExecutionControl>>,
        ) -> crate::WorkbenchDispatch<Self> {
            unreachable!("population fixture runs only startup and cleanup")
        }

        fn external_application_failed<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
            _failure: crate::ExternalApplicationFailure,
        ) -> futures_util::future::BoxFuture<'a, crate::ExternalFailureDisposition> {
            unreachable!("population fixture has no external application")
        }

        fn shutdown<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
            _terminal: &'a crate::ActorTerminal,
        ) -> futures_util::future::BoxFuture<'a, Result<(), crate::KernelBehaviorError>> {
            Box::pin(async { Ok(()) })
        }

        fn shutdown_components<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
            _terminal: &'a crate::ActorTerminal,
            _deadline: tokio::time::Instant,
        ) -> futures_util::future::BoxFuture<
            'a,
            (
                crate::CleanupComponentOutcome,
                crate::CleanupComponentOutcome,
            ),
        > {
            let realm = self.realm.clone();
            // This native-only behavior allocates no runtime realm. Its
            // configured component result is retained by the real wrapper.
            Box::pin(async move { (crate::CleanupComponentOutcome::Confirmed, realm) })
        }

        fn stopped<'a>(
            &'a mut self,
            _context: &'a crate::KernelContext,
            _terminal: &'a crate::ActorTerminal,
        ) -> futures_util::future::BoxFuture<'a, ()> {
            let entered = self.stopped_entered.take().unwrap();
            let release = self.stopped_release.take().unwrap();
            Box::pin(async move {
                entered.send(()).unwrap();
                release.await.unwrap();
            })
        }

        fn child_exited(&mut self, _notice: crate::ChildExitNotice) {
            unreachable!("population fixture owns no children")
        }
    }

    #[tokio::test]
    async fn generation_population_reports_actual_unclassified_child_without_stopping_it() {
        use crate::local_actor::run_control::{
            GenerationCollateralIdentity, GenerationLifetimeEdge,
        };
        struct PhysicalChild;
        impl ractor::Actor for PhysicalChild {
            type Msg = ();
            type State = ();
            type Arguments = ();
            async fn pre_start(
                &self,
                _myself: ractor::ActorRef<()>,
                (): (),
            ) -> Result<(), ractor::ActorProcessingErr> {
                Ok(())
            }
        }

        let mut history = History::new();
        let dependency = generation(&mut history, NotebookGenerationId::INITIAL, None);
        let (directory, directory_task) = crate::LocalActorDirectory::start().await.unwrap();
        let reservation = directory
            .reserve_source_actor(
                &SourceDependency::Generation(dependency.clone()),
                crate::Incarnation::FIRST,
            )
            .await
            .unwrap();
        let identity = reservation.identity();
        let terminal = reservation.terminal().clone();
        let (start_entered, starting) = tokio::sync::oneshot::channel();
        let (start_release, continue_start) = tokio::sync::oneshot::channel();
        let (stopped_entered, stopped) = tokio::sync::oneshot::channel();
        let (stopped_release, continue_stop) = tokio::sync::oneshot::channel();
        let spawning = tokio::spawn(crate::local_actor::spawn_reserved_local_actor(
            None,
            PopulationBehavior {
                start_entered: Some(start_entered),
                start_release: Some(continue_start),
                stopped_entered: Some(stopped_entered),
                stopped_release: Some(continue_stop),
                realm: crate::CleanupComponentOutcome::Confirmed,
            },
            reservation,
            directory.clone(),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(10), starting)
            .await
            .unwrap()
            .unwrap();
        let parent = directory.resolve(identity).await.unwrap().unwrap();
        let (child, child_task) = <PhysicalChild as ractor::Actor>::spawn_linked(
            None,
            PhysicalChild,
            (),
            parent.address().get_cell(),
        )
        .await
        .unwrap();
        let fence = dependency.fence().unwrap();
        let population = directory.generation_population(&fence).await.unwrap();
        assert_eq!(population.resources.len(), 1);
        assert_eq!(population.collateral.len(), 1);
        assert_eq!(
            population.collateral[0].affected,
            GenerationCollateralIdentity::UnclassifiedPhysical(child.get_id())
        );
        assert_eq!(
            population.collateral[0].edge,
            GenerationLifetimeEdge::PhysicalSupervision
        );
        assert!(terminal.get().is_none());
        assert!(terminal.requested_shutdown().is_none());
        assert_ne!(child.get_status(), ractor::ActorStatus::Stopped);
        fence.reopen().unwrap();
        drop(SourceDependency::Generation(dependency).claim().unwrap());

        child
            .stop_and_wait(None, Some(std::time::Duration::from_secs(2)))
            .await
            .unwrap();
        child_task.await.unwrap();
        start_release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), stopped)
            .await
            .unwrap()
            .unwrap();
        stopped_release.send(()).unwrap();
        let (_, task) = spawning.await.unwrap().unwrap();
        task.await.unwrap();
        directory.stop_owner_for_test().await;
        directory_task.await.unwrap();
    }

    #[tokio::test]
    async fn generation_population_preserves_abandoned_source_reservation_uncertainty() {
        use crate::local_actor::run_control::{
            GenerationResourceIdentity as Identity, GenerationResourceState as State,
        };
        let mut history = History::new();
        let dependency = generation(&mut history, NotebookGenerationId::INITIAL, None);
        let (directory, task) = crate::LocalActorDirectory::start().await.unwrap();
        let reservation = directory
            .reserve_source_actor(
                &SourceDependency::Generation(dependency.clone()),
                crate::Incarnation::FIRST,
            )
            .await
            .unwrap();
        let actor = reservation.identity();
        let terminal = reservation.terminal().clone();
        let fence = dependency.fence().unwrap();
        let ready = directory.generation_population(&fence).await.unwrap();
        assert!(ready.dependency.same_owner(&dependency));
        assert_eq!(ready.resources.len(), 1);
        assert_eq!(ready.resources[0].identity, Identity::Actor(actor));
        assert_eq!(ready.resources[0].state, State::Starting);

        drop(reservation);
        // Drop enqueues AbandonReservation before this observation on the
        // same directory owner; no polling or direct phase writes are needed.
        let abandoned = directory.generation_population(&fence).await.unwrap();
        assert_eq!(abandoned.resources.len(), 1);
        assert_eq!(abandoned.resources[0].identity, Identity::Actor(actor));
        assert!(matches!(
            abandoned.resources[0].state,
            State::CleanupUnconfirmed(_)
        ));
        assert!(terminal.requested_shutdown().is_some());
        assert!(terminal.get().is_none());
        assert!(terminal.cleanup().is_none());
        directory.stop_owner_for_test().await;
        task.await.unwrap();
    }

    #[tokio::test]
    async fn generation_population_observes_terminal_cleanup_before_executor_ack() {
        use crate::local_actor::run_control::{
            GenerationResourceIdentity as Identity, GenerationResourceState as State,
        };
        for realm in [
            crate::CleanupComponentOutcome::Unsupported,
            crate::CleanupComponentOutcome::Unconfirmed("native cleanup not confirmed".into()),
            crate::CleanupComponentOutcome::Confirmed,
        ] {
            let mut history = History::new();
            let dependency = generation(&mut history, NotebookGenerationId::INITIAL, None);
            let (directory, directory_task) = crate::LocalActorDirectory::start().await.unwrap();
            let reservation = directory
                .reserve_source_actor(
                    &SourceDependency::Generation(dependency.clone()),
                    crate::Incarnation::FIRST,
                )
                .await
                .unwrap();
            let identity = reservation.identity();
            let terminal = reservation.terminal().clone();
            let (start_entered, starting) = tokio::sync::oneshot::channel();
            let (start_release, continue_start) = tokio::sync::oneshot::channel();
            let (stopped_entered, stopped) = tokio::sync::oneshot::channel();
            let (stopped_release, continue_stop) = tokio::sync::oneshot::channel();
            let spawning = tokio::spawn(crate::local_actor::spawn_reserved_local_actor(
                None,
                PopulationBehavior {
                    start_entered: Some(start_entered),
                    start_release: Some(continue_start),
                    stopped_entered: Some(stopped_entered),
                    stopped_release: Some(continue_stop),
                    realm: realm.clone(),
                },
                reservation,
                directory.clone(),
            ));
            tokio::time::timeout(std::time::Duration::from_secs(10), starting)
                .await
                .expect("real registered actor reaches start")
                .unwrap();
            assert!(!spawning.is_finished());
            assert!(terminal.get().is_none());
            let fence = dependency.fence().unwrap();
            let live = directory.generation_population(&fence).await.unwrap();
            assert!(live.dependency.same_owner(&dependency));
            assert_eq!(live.resources.len(), 1);
            assert_eq!(live.resources[0].identity, Identity::Actor(identity));
            assert_eq!(live.resources[0].state, State::Starting);

            start_release.send(()).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(10), stopped)
                .await
                .expect("real cleanup and terminal publication reach stopped")
                .unwrap();
            assert!(!spawning.is_finished(), "executor ack has not been sent");
            assert_eq!(
                terminal.get().unwrap().kind,
                crate::ActorExitKind::Completed
            );
            let cleanup = terminal.cleanup().expect("actual retained cleanup");
            assert_eq!(cleanup.realm, realm);
            assert_eq!(cleanup.hook, crate::CleanupComponentOutcome::Confirmed);
            assert_eq!(cleanup.children, crate::CleanupComponentOutcome::Confirmed);
            assert_eq!(
                cleanup.is_confirmed(),
                matches!(realm, crate::CleanupComponentOutcome::Confirmed)
            );
            let population = directory.generation_population(&fence).await.unwrap();
            if matches!(realm, crate::CleanupComponentOutcome::Confirmed) {
                assert!(population.resources.is_empty());
            } else {
                assert_eq!(population.resources.len(), 1);
                assert_eq!(population.resources[0].identity, Identity::Actor(identity));
                assert!(matches!(
                    population.resources[0].state,
                    State::CleanupUnconfirmed(_)
                ));
            }

            stopped_release.send(()).unwrap();
            let (actor, task) = spawning
                .await
                .unwrap()
                .expect("successful completed startup");
            assert_eq!(actor.identity(), identity);
            task.await.unwrap();
            let acknowledged = directory.generation_population(&fence).await.unwrap();
            assert_eq!(acknowledged.resources, population.resources);
            assert_eq!(terminal.cleanup(), Some(cleanup));
            // Unknown remains an observed retained result; stopping this test's
            // empty native owner is not a successful finish_run certificate.
            directory.stop_owner_for_test().await;
            directory_task.await.unwrap();
        }
    }
}
