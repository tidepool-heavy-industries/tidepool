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
                    .map(|scope| scope.source_admission().clone())
                    .or_else(|| {
                        self.0
                            .prerequisite
                            .as_ref()
                            .map(|parent| parent.runtime_admission().clone())
                    })
                    .map(|admission| admission.admit())
                    .transpose()?;
                state.claims += 1;
                Ok(SourceClaim::Bootstrap(BootstrapClaim {
                    world: self.clone(),
                    settled: false,
                    prerequisite,
                }))
            }
            WorldPhase::Bound(dependency) => {
                let admission = dependency.runtime_admission().clone();
                let scope = dependency.0.scope.clone();
                drop(state);
                Ok(SourceClaim::Runtime {
                    claim: admission.admit()?,
                    scope,
                })
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
            .map(|parent| parent.runtime_admission().admit())
            .transpose()?;
        let scope = self
            .0
            .scope
            .get()
            .ok_or(GenerationAdmissionError::Unqualified)?;
        if !scope
            .source_admission()
            .same_owner(target.runtime_admission())
        {
            return Err(GenerationAdmissionError::StaleSourceLease);
        }
        let fence = scope.source_admission().fence()?;
        if fence.pending_claims() != 0 || fence.unconfirmed_claims() != 0 {
            let claims = fence.pending_claims();
            let unconfirmed = fence.unconfirmed_claims();
            if unconfirmed == 0 {
                fence.reopen()?;
            }
            return Err(GenerationAdmissionError::BootstrapBusy {
                claims,
                unconfirmed,
            });
        }
        state.phase = WorldPhase::Publishing(target.clone());
        Ok(WorldPublication {
            world: self.clone(),
            settled: false,
            prerequisite,
            fence: Some(fence),
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
    Runtime {
        claim: tidepool_runtime::session::RuntimeSourceClaim,
        scope: Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
    },
    Bootstrap(BootstrapClaim),
}
impl SourceClaim {
    pub(crate) fn retain_unconfirmed(self) {
        match self {
            Self::Runtime { claim, scope } => {
                scope.retain_unconfirmed_source_custody();
                claim.retain_unconfirmed();
            }
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
    prerequisite: Option<tidepool_runtime::session::RuntimeSourceClaim>,
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
    settled: bool,
    prerequisite: Option<tidepool_runtime::session::RuntimeSourceClaim>,
    fence: Option<tidepool_runtime::session::RuntimeSourceFence>,
}
impl WorldPublication {
    pub(super) fn visible(&mut self, target: &GenerationDependency) {
        let mut state = self.world.0.state.lock();
        match &state.phase {
            WorldPhase::Publishing(expected) if expected.same_owner(target) => {}
            _ => panic!("initial world publication lost its exact owner"),
        }
        state.phase = WorldPhase::Bound(target.clone());
    }
    pub(super) fn confirmed(mut self) {
        self.fence
            .take()
            .expect("initial source publication fence")
            .reopen()
            .expect("confirmed publisher owns source fence");
        self.settled = true;
    }
    pub(super) fn refused(mut self) {
        let mut state = self.world.0.state.lock();
        if matches!(state.phase, WorldPhase::Publishing(_)) {
            state.phase = WorldPhase::Bootstrap;
        }
        self.fence
            .take()
            .expect("initial source publication fence")
            .reopen()
            .expect("refused publisher owns source fence");
        self.settled = true;
    }
}
impl Drop for WorldPublication {
    fn drop(&mut self) {
        if !self.settled {
            if let Some(prerequisite) = self.prerequisite.take() {
                prerequisite.retain_unconfirmed();
            }
            let mut state = self.world.0.state.lock();
            if let WorldPhase::Publishing(target) | WorldPhase::Bound(target) = &state.phase {
                let target = target.clone();
                state.uncertainty_fence = self.fence.take();
                state.phase = WorldPhase::Unavailable(target);
            }
        }
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
                let lease = session.adopt_initial_notebook_scope_with_source(
                    scope,
                    world
                        .prerequisite()
                        .as_ref()
                        .map(|p| p.runtime_admission().clone()),
                )?;
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
            Self::Generation(dependency) => Ok(SourceClaim::Runtime {
                claim: dependency.runtime_admission().admit()?,
                scope: dependency.0.scope.clone(),
            }),
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

    /// A new independent world holds its bootstrap claim until the existing
    /// directory has recorded its reservation. A bound alias uses its exact
    /// selected generation and the directory atomically orders its registration.
    pub(crate) fn registration_dependency(
        &self,
    ) -> Result<(Option<GenerationDependency>, Option<SourceClaim>), GenerationAdmissionError> {
        match self {
            Self::Generation(dependency) => Ok((Some(dependency.clone()), None)),
            Self::World(world) => {
                let mut state = world.0.state.lock();
                match &state.phase {
                    WorldPhase::Bootstrap if state.unconfirmed == 0 => {
                        let prerequisite = world
                            .0
                            .prerequisite
                            .as_ref()
                            .map(|parent| parent.runtime_admission().admit())
                            .transpose()?;
                        state.claims += 1;
                        Ok((
                            world.0.prerequisite.clone(),
                            Some(SourceClaim::Bootstrap(BootstrapClaim {
                                world: world.clone(),
                                settled: false,
                                prerequisite,
                            })),
                        ))
                    }
                    WorldPhase::Bound(dependency) => Ok((Some(dependency.clone()), None)),
                    _ => Err(GenerationAdmissionError::BootstrapUnavailable),
                }
            }
            Self::Unqualified => Err(GenerationAdmissionError::Unqualified),
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
                        match &world.0.prerequisite {
                            Some(parent) => parent.with_open_admission(admit),
                            None => Ok(admit()),
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
    pub(crate) fn depends_on(&self, other: &Self) -> bool {
        self.0.admission.depends_on(&other.0.admission)
    }
    pub(crate) fn prerequisite(&self) -> &Option<GenerationDependency> {
        &self.0.prerequisite
    }
    pub(crate) fn runtime_admission(&self) -> &tidepool_runtime::session::RuntimeSourceAdmission {
        &self.0.admission
    }
    pub(crate) fn with_open_admission<T>(
        &self,
        admit: impl FnOnce() -> T,
    ) -> Result<T, GenerationAdmissionError> {
        self.0.admission.with_open_admission(admit)
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
    pub(crate) fn reopen(self) -> Result<(), GenerationAdmissionError> {
        self.fence.reopen()
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
                    .map(|parent| parent.runtime_admission().clone()),
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
        assert!(alias
            .registration_dependency()
            .unwrap()
            .0
            .unwrap()
            .same_owner(&child));
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
        let independent = generation(&mut session, NotebookGenerationId::INITIAL, None);
        parent.fence().unwrap();
        assert!(SourceDependency::Generation(next).claim().is_err());
        drop(SourceDependency::Generation(independent).claim().unwrap());
        assert!(SourceDependency::World(BootstrapWorld::new(None))
            .selected_dependency()
            .is_err());
    }
}
