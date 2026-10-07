//! Atomic allocation of readable actor paths within one retained actor tree.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::SessionId;
use tidepool_repr::{ActorPath, ActorPathError, ActorPathSegment};
use tidepool_runtime::session::WorkbenchForkBoundary;

use crate::ActorRef;
use crate::HostedCheckpointAttachment;

mod spawn_admission;
pub use spawn_admission::{SpawnAdmission, SpawnAdmissionOutcome};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorPathReservation {
    pub requested: ActorPath,
    pub allocated: ActorPath,
}

#[derive(Default)]
struct LineageState {
    campaigns: BTreeMap<(ActorRef, ActorPathSegment), ActorPath>,
    occupied: BTreeSet<ActorPath>,
}

/// One allocator for actor labels and their exact `exomonad/<path>` Git projection.
#[derive(Clone, Default)]
pub struct ActorLineageRegistry {
    state: Arc<Mutex<LineageState>>,
}

impl ActorLineageRegistry {
    pub fn reserve_path(
        &self,
        requested: ActorPath,
    ) -> Result<ActorPathReservation, ActorPathError> {
        let mut state = self.state.lock();
        let Some((leaf, prefix)) = requested.segments().split_last() else {
            return Err(ActorPathError::EmptyPath);
        };
        let allocated = lowest_available(|path| state.occupied.contains(path), prefix, leaf)?;
        state.occupied.insert(allocated.clone());
        Ok(ActorPathReservation {
            requested,
            allocated,
        })
    }

    pub fn reserve_campaign(
        &self,
        root: ActorRef,
        requested: ActorPathSegment,
    ) -> Result<ActorPathReservation, ActorPathError> {
        let mut state = self.state.lock();
        if let Some(allocated) = state.campaigns.get(&(root, requested.clone())) {
            return Ok(ActorPathReservation {
                requested: ActorPath::new(vec![requested])?,
                allocated: allocated.clone(),
            });
        }
        let requested_path = ActorPath::new(vec![requested.clone()])?;
        let allocated = lowest_available(|path| state.occupied.contains(path), &[], &requested)?;
        state.occupied.insert(allocated.clone());
        state.campaigns.insert((root, requested), allocated.clone());
        Ok(ActorPathReservation {
            requested: requested_path,
            allocated,
        })
    }

    /// Reserve one complete sibling batch while holding the allocator lock.
    pub fn reserve_children(
        &self,
        parent: &ActorPath,
        group: ActorPathSegment,
        children: &[ActorPathSegment],
    ) -> Result<Vec<ActorPathReservation>, ActorPathError> {
        let group_path = parent.child(group)?;
        self.reserve_group_children(&group_path, children)
    }

    /// Reserve children directly beneath an already assembled group path.
    pub fn reserve_group_children(
        &self,
        group: &ActorPath,
        children: &[ActorPathSegment],
    ) -> Result<Vec<ActorPathReservation>, ActorPathError> {
        let mut frequencies = BTreeMap::<&ActorPathSegment, usize>::new();
        for child in children {
            *frequencies.entry(child).or_default() += 1;
        }
        let mut ordinals = BTreeMap::<&ActorPathSegment, usize>::new();
        let mut state = self.state.lock();
        let mut allocated_paths = BTreeSet::new();
        let mut reservations = Vec::with_capacity(children.len());
        for child in children {
            let ordinal = ordinals.entry(child).or_default();
            *ordinal += 1;
            let requested_leaf = if frequencies[child] > 1 {
                child.numbered(*ordinal)?
            } else {
                child.clone()
            };
            let requested = group.child(requested_leaf.clone())?;
            let allocated = lowest_available(
                |path| state.occupied.contains(path) || allocated_paths.contains(path),
                group.segments(),
                &requested_leaf,
            )?;
            allocated_paths.insert(allocated.clone());
            reservations.push(ActorPathReservation {
                requested,
                allocated,
            });
        }
        // Publish only after every path validates; errors leave retained history intact.
        state.occupied.extend(allocated_paths);
        Ok(reservations)
    }

    fn release_unpublished<'a>(&self, paths: impl IntoIterator<Item = &'a ActorPath>) {
        let mut state = self.state.lock();
        for path in paths {
            state.occupied.remove(path);
        }
    }

    pub fn retain_external(&self, path: ActorPath) -> bool {
        self.state.lock().occupied.insert(path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ForkGroupId(pub u64);

#[derive(Debug, thiserror::Error)]
pub enum ForkGroupError {
    #[error(transparent)]
    Path(#[from] ActorPathError),
    #[error("checkpoint refusal: {0:?}")]
    Checkpoint(CheckpointRefusal),
    #[error("unknown fork group {0}")]
    Unknown(u64),
    #[error("actor {actual:?} does not own fork group {group}")]
    WrongOwner { group: u64, actual: ActorRef },
    #[error("fork group {0} received more child starts than it reserved")]
    TooManyChildren(u64),
    #[error("fork group {group} expected child path `{expected}`, got `{actual}`")]
    WrongChildPath {
        group: u64,
        expected: ActorPath,
        actual: ActorPath,
    },
    #[error("fork group {group} is incomplete: {started} of {expected} children started")]
    Incomplete {
        group: u64,
        started: usize,
        expected: usize,
    },
    #[error("fork group {0} was already committed")]
    AlreadyCommitted(u64),
    #[error("fork group {0} is not committed")]
    NotCommitted(u64),
    #[error("fork group {0} is not ready for publication")]
    NotReady(u64),
    #[error("fork group {group} checkpoint admission does not match child {child:?}")]
    CheckpointAdmissionMismatch { group: u64, child: ActorRef },
    #[error("fork group {0} has members outside the inspected cleanup scope")]
    CleanupScopeChanged(u64),
    #[error("actor {0:?} is being cleaned up")]
    Cleaning(ActorRef),
    #[error("fork group {0} has unfinished descendant admission")]
    CleanupAdmissionPending(u64),
    /// Every ancestor's ceiling counts this actor's whole subtree, so the
    /// coordinator whose ceiling is full is often not the one forking: a node
    /// can be refused because a grandchild of its sibling is still running.
    /// Naming it is the difference between freeing the right slot and
    /// widening the wrong budget.
    #[error(
        "fork group would exceed the active descendant ceiling of coordinator {coordinator:?} ({requested} requested, {active} already active or reserved in its subtree, maximum {maximum})"
    )]
    DescendantBudgetExceeded {
        coordinator: ActorRef,
        requested: usize,
        active: usize,
        maximum: usize,
    },
}

#[derive(Clone, Copy)]
enum ForkGroupBoundary<'a> {
    Any,
    Resident,
    At(&'a WorkbenchForkBoundary),
}

impl ForkGroupBoundary<'_> {
    fn matches(self, boundary: Option<&WorkbenchForkBoundary>) -> bool {
        match self {
            Self::Any => true,
            Self::Resident => boundary.is_none(),
            Self::At(expected) => boundary == Some(expected),
        }
    }
}

struct ForkGroup {
    owner: ActorRef,
    completion_boundary: Option<tidepool_runtime::session::WorkbenchForkBoundary>,
    reservations: Vec<ActorPathReservation>,
    // Admission retains budget provenance after the capability is released.
    checkpoint_sponsors: Vec<Option<Vec<ActorRef>>>,
    claimed: usize,
    children: Vec<ActorRef>,
    checkpoint_admissions: HashMap<ActorRef, AdmittedForkCheckpoint>,
    selected_admissions: HashMap<ActorRef, (SessionId, ScopeId, ActorPath)>,
    commit_requested: bool,
    publication: Option<ForkGroupPublication>,
    ready: HashSet<ActorRef>,
    phase: tokio::sync::watch::Sender<ForkGroupPhase>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkGroupPhase {
    Staging,
    Ready,
    Committed,
    Aborted,
}

/// Immutable facts recorded from the admitted lease after actual child scope
/// allocation. Release can revoke new uses without revoking this admission.
#[derive(Clone)]
pub(crate) struct AdmittedForkCheckpoint {
    token: String,
    issuer: ActorRef,
    checkpoint: (SessionId, ScopeId),
    child: (SessionId, ScopeId),
    path: ActorPath,
    pub(crate) context: crate::HostedCheckpointContext,
}

impl AdmittedForkCheckpoint {
    pub(crate) fn matches(&self, descriptor: &crate::ActorDescriptor) -> bool {
        descriptor.checkpoint_token() == Some(self.token.as_str())
            && descriptor.context_parent() == Some(self.issuer)
            && descriptor.actor_path() == Some(&self.path)
            && (
                descriptor.placement().session,
                descriptor.placement().lexical_scope,
            ) == self.child
            && self.checkpoint.0 == self.child.0
    }
}

/// Context selected by the owning group publication transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkGroupPublication {
    Deferred,
    Captured,
}

/// Retained authority for release repair after the registry commits a group.
/// Only the registry constructs it; a failed release cannot request rollback.
pub(crate) struct CommittedForkGroups {
    owner: ActorRef,
    groups: Vec<ForkGroupId>,
    children: Vec<(ForkGroupId, ActorRef)>,
}

impl CommittedForkGroups {
    pub(crate) fn owner(&self) -> ActorRef {
        self.owner
    }
    pub(crate) fn groups(&self) -> &[ForkGroupId] {
        &self.groups
    }
    pub(crate) fn permits_child(&self, group: ForkGroupId, child: ActorRef) -> bool {
        self.children.contains(&(group, child))
    }
}

#[derive(Clone)]
pub struct ForkGroupGate {
    id: ForkGroupId,
    child: ActorRef,
    registry: ForkGroupRegistry,
}

impl ForkGroupGate {
    #[must_use]
    pub fn id(&self) -> ForkGroupId {
        self.id
    }

    pub fn mark_ready(&self) -> Result<(), ForkGroupError> {
        self.registry.mark_ready(self.id, self.child)
    }

    pub async fn wait_committed(&self) -> Result<(), ForkGroupError> {
        self.registry.wait_committed(self.id).await
    }

    pub fn mark_failed(&self) -> Result<(), ForkGroupError> {
        self.registry.mark_failed(self.id, self.child)
    }

    pub fn publication(&self) -> Result<ForkGroupPublication, ForkGroupError> {
        self.registry.committed_publication(self.id, self.child)
    }
}

#[derive(Default)]
struct ForkGroupsState {
    next: u64,
    spawns: HashMap<uuid::Uuid, spawn_admission::SpawnAdmissionRecord>,
    groups: HashMap<ForkGroupId, ForkGroup>,
    cleaned: HashSet<ForkGroupId>,
    parents: HashMap<ActorRef, ActorRef>,
    descendant_limits: HashMap<ActorRef, usize>,
    active: HashSet<ActorRef>,
    cleaning: HashSet<ActorRef>,
    checkpoints: HashMap<String, CheckpointLease>,
    // Release revokes admission immediately, while the exact captured scope
    // remains owned here until the session confirms its retirement.
    released_checkpoints: HashMap<String, ReleasedCheckpoint>,
}

struct ReleasedCheckpoint {
    session: SessionId,
    scope: Option<ScopeId>,
    cleanup_pending: bool,
    retained_scope: Option<std::sync::Weak<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
}

/// Admission ledger for one applicative context-unfold layer.
#[derive(Clone)]
pub struct ForkGroupRegistry {
    lineage: ActorLineageRegistry,
    state: Arc<Mutex<ForkGroupsState>>,
    checkpoint_namespace: uuid::Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, tidepool_bridge_derive::ToHaskell)]
pub enum CheckpointRefusal {
    #[haskell(module = "Tidepool.Effects.Core")]
    NoHostedBoundary,
    #[haskell(module = "Tidepool.Effects.Core")]
    WrongSession,
    #[haskell(module = "Tidepool.Effects.Core")]
    UnavailableCheckpoint,
    #[haskell(module = "Tidepool.Effects.Core")]
    ReleasedCheckpoint,
    #[haskell(module = "Tidepool.Effects.Core")]
    CaptureFailed,
    #[haskell(module = "Tidepool.Effects.Core")]
    ProcessRestartUnsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointPhase {
    Pending,
    Published,
    Failed,
    Released,
    // A branch admitted before release may still finish provider binding.
    ReleasedAfterPublication,
}

/// Checkpoint metadata used to validate a proposed launch, not child admission.
pub(crate) struct CheckpointPreview(CheckpointLease);

impl CheckpointPreview {
    pub(crate) fn lease(&self) -> &CheckpointLease {
        &self.0
    }
}

/// The group reservation and checkpoint installation share admitted together.
pub(crate) struct ClaimedForkChild {
    pub(crate) path: ActorPath,
    pub(crate) checkpoint: Option<(CheckpointLease, Option<HostedCheckpointAttachment>)>,
}

#[derive(Clone)]
pub struct CheckpointLease {
    pub name: String,
    pub issuer: ActorRef,
    budget_sponsors: Vec<ActorRef>,
    pub issuer_role: crate::EffectiveRole,
    pub(crate) issuer_persistence_policy: crate::ActorPersistencePolicy,
    pub issuer_model: Option<crate::Model>,
    pub issuer_effort: Option<crate::ForkEffort>,
    pub issuer_source_layer: crate::CheckpointSourceLayer,
    pub session: SessionId,
    pub scope: ScopeId,
    retained_scope: Option<Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
    pub boundary: WorkbenchForkBoundary,
    host_attachment: Arc<Mutex<Option<HostedCheckpointAttachment>>>,
    phase: tokio::sync::watch::Sender<CheckpointPhase>,
}

impl CheckpointLease {
    pub(crate) fn retained_scope(
        &self,
    ) -> Result<&Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>, CheckpointRefusal> {
        self.retained_scope
            .as_ref()
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)
    }

    #[must_use]
    pub fn host_attachment<T: std::any::Any + Send + Sync>(&self) -> Option<Arc<T>> {
        if matches!(
            *self.phase.borrow(),
            CheckpointPhase::Pending | CheckpointPhase::Failed
        ) {
            return None;
        }
        self.host_attachment.lock().as_ref()?.downcast()
    }
}

impl CheckpointLease {
    pub async fn wait_published(&self) -> Result<(), CheckpointRefusal> {
        let mut phase = self.phase.subscribe();
        loop {
            match *phase.borrow_and_update() {
                CheckpointPhase::Published => return Ok(()),
                CheckpointPhase::ReleasedAfterPublication => return Ok(()),
                CheckpointPhase::Failed => return Err(CheckpointRefusal::CaptureFailed),
                CheckpointPhase::Released => return Err(CheckpointRefusal::ReleasedCheckpoint),
                CheckpointPhase::Pending => {}
            }
            if phase.changed().await.is_err() {
                return Err(CheckpointRefusal::CaptureFailed);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkGroupCleanupOutcome {
    Cleaned,
    Active(Vec<ActorRef>),
}

fn group_members(
    state: &ForkGroupsState,
    id: ForkGroupId,
    owner: ActorRef,
) -> Result<Vec<ActorRef>, ForkGroupError> {
    if state.cleaned.contains(&id) {
        return Ok(Vec::new());
    }
    let group = state.groups.get(&id).ok_or(ForkGroupError::Unknown(id.0))?;
    if group.owner != owner {
        return Err(ForkGroupError::WrongOwner {
            group: id.0,
            actual: owner,
        });
    }
    if *group.phase.borrow() != ForkGroupPhase::Committed {
        return Err(ForkGroupError::NotCommitted(id.0));
    }
    let mut members = state
        .parents
        .keys()
        .copied()
        .filter_map(|candidate| {
            group
                .children
                .iter()
                .copied()
                .find_map(|root| descendant_depth(&state.parents, candidate, root))
                .map(|depth| (depth, candidate))
        })
        .collect::<Vec<_>>();
    members.sort_unstable_by_key(|(depth, actor)| {
        (std::cmp::Reverse(*depth), actor.id, actor.incarnation)
    });
    Ok(members.into_iter().map(|(_, actor)| actor).collect())
}

pub(crate) struct ForkCleanupGuard {
    registry: ForkGroupRegistry,
    actors: HashSet<ActorRef>,
}

impl ForkCleanupGuard {
    pub(crate) fn contains(&self, actor: &ActorRef) -> bool {
        self.actors.contains(actor)
    }
}

impl Drop for ForkCleanupGuard {
    fn drop(&mut self) {
        let mut state = self.registry.state.lock();
        for actor in &self.actors {
            state.cleaning.remove(actor);
        }
    }
}

impl ForkGroupRegistry {
    #[must_use]
    pub fn new(lineage: ActorLineageRegistry) -> Self {
        Self {
            lineage,
            state: Arc::new(Mutex::new(ForkGroupsState {
                next: 1,
                spawns: HashMap::new(),
                groups: HashMap::new(),
                cleaned: HashSet::new(),
                parents: HashMap::new(),
                descendant_limits: HashMap::new(),
                active: HashSet::new(),
                cleaning: HashSet::new(),
                checkpoints: HashMap::new(),
                released_checkpoints: HashMap::new(),
            })),
            checkpoint_namespace: uuid::Uuid::new_v4(),
        }
    }

    /// Keep the captured scope under the existing fork custody ledger until
    /// explicit release or failure. The opaque token cannot move it to another machine.
    pub fn capture_checkpoint(
        &self,
        name: String,
        issuer: ActorRef,
        issuer_role: crate::EffectiveRole,
        issuer_model: Option<crate::Model>,
        issuer_effort: Option<crate::ForkEffort>,
        issuer_source_layer: crate::CheckpointSourceLayer,
        session: SessionId,
        scope: ScopeId,
        boundary: WorkbenchForkBoundary,
    ) -> String {
        self.capture_checkpoint_with_host_attachment(
            name,
            issuer,
            issuer_role,
            issuer_model,
            issuer_effort,
            issuer_source_layer,
            session,
            scope,
            boundary,
            None,
        )
    }

    pub fn capture_checkpoint_with_host_attachment(
        &self,
        name: String,
        issuer: ActorRef,
        issuer_role: crate::EffectiveRole,
        issuer_model: Option<crate::Model>,
        issuer_effort: Option<crate::ForkEffort>,
        issuer_source_layer: crate::CheckpointSourceLayer,
        session: SessionId,
        scope: ScopeId,
        boundary: WorkbenchForkBoundary,
        host_attachment: Option<HostedCheckpointAttachment>,
    ) -> String {
        self.capture_checkpoint_inner(
            name,
            issuer,
            issuer_role,
            issuer_model,
            issuer_effort,
            issuer_source_layer,
            session,
            scope,
            boundary,
            host_attachment,
            None,
            crate::ActorPersistencePolicy::Ephemeral,
        )
    }

    /// Runtime captures retain a detached lexical share independently of the
    /// token's original scope and every later parent settlement.
    pub fn capture_checkpoint_with_retained_scope(
        &self,
        name: String,
        issuer: ActorRef,
        issuer_role: crate::EffectiveRole,
        issuer_model: Option<crate::Model>,
        issuer_effort: Option<crate::ForkEffort>,
        issuer_source_layer: crate::CheckpointSourceLayer,
        session: SessionId,
        scope: ScopeId,
        boundary: WorkbenchForkBoundary,
        host_attachment: Option<HostedCheckpointAttachment>,
        retained_scope: Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
        issuer_persistence_policy: crate::ActorPersistencePolicy,
    ) -> String {
        self.capture_checkpoint_inner(
            name,
            issuer,
            issuer_role,
            issuer_model,
            issuer_effort,
            issuer_source_layer,
            session,
            scope,
            boundary,
            host_attachment,
            Some(retained_scope),
            issuer_persistence_policy,
        )
    }

    fn capture_checkpoint_inner(
        &self,
        name: String,
        issuer: ActorRef,
        issuer_role: crate::EffectiveRole,
        issuer_model: Option<crate::Model>,
        issuer_effort: Option<crate::ForkEffort>,
        issuer_source_layer: crate::CheckpointSourceLayer,
        session: SessionId,
        scope: ScopeId,
        boundary: WorkbenchForkBoundary,
        host_attachment: Option<HostedCheckpointAttachment>,
        retained_scope: Option<Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
        issuer_persistence_policy: crate::ActorPersistencePolicy,
    ) -> String {
        let token = format!("{}:{}", self.checkpoint_namespace, uuid::Uuid::new_v4());
        let (phase, _) = tokio::sync::watch::channel(CheckpointPhase::Pending);
        let mut state = self.state.lock();
        let mut budget_sponsors = Vec::new();
        let mut ancestor = Some(issuer);
        while let Some(actor) = ancestor {
            budget_sponsors.push(actor);
            ancestor = state.parents.get(&actor).copied();
        }
        if let Some(limit) = issuer_role.descendants().maximum_active_children {
            state
                .descendant_limits
                .entry(issuer)
                .and_modify(|old| *old = (*old).min(usize::from(limit)))
                .or_insert(usize::from(limit));
        }
        state.checkpoints.insert(
            token.clone(),
            CheckpointLease {
                name,
                issuer,
                budget_sponsors,
                issuer_role,
                issuer_persistence_policy,
                issuer_model,
                issuer_effort,
                issuer_source_layer,
                session,
                scope,
                retained_scope,
                boundary,
                host_attachment: Arc::new(Mutex::new(host_attachment)),
                phase,
            },
        );
        token
    }

    pub fn checkpoint(
        &self,
        token: &str,
        session: SessionId,
    ) -> Result<CheckpointLease, CheckpointRefusal> {
        let (namespace, _) = token
            .split_once(':')
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)?;
        if namespace != self.checkpoint_namespace.to_string() {
            return Err(CheckpointRefusal::ProcessRestartUnsupported);
        }
        let state = self.state.lock();
        let lease = match state.checkpoints.get(token) {
            Some(lease) => lease.clone(),
            None => {
                return match state.released_checkpoints.get(token) {
                    Some(released) if released.session == session => {
                        Err(CheckpointRefusal::ReleasedCheckpoint)
                    }
                    Some(_) => Err(CheckpointRefusal::WrongSession),
                    None => Err(CheckpointRefusal::UnavailableCheckpoint),
                };
            }
        };
        if lease.session != session {
            return Err(CheckpointRefusal::WrongSession);
        }
        match *lease.phase.borrow() {
            CheckpointPhase::Failed => return Err(CheckpointRefusal::CaptureFailed),
            CheckpointPhase::Released | CheckpointPhase::ReleasedAfterPublication => {
                return Err(CheckpointRefusal::ReleasedCheckpoint)
            }
            CheckpointPhase::Pending | CheckpointPhase::Published => {}
        }
        Ok(lease)
    }

    /// Preview metadata for launch validation. Release may still win before
    /// claim_with_checkpoint atomically admits the group path and installation.
    pub(crate) fn preview_checkpoint(
        &self,
        token: &str,
        session: SessionId,
    ) -> Result<CheckpointPreview, CheckpointRefusal> {
        self.checkpoint_admission_locked(&self.state.lock(), token, session)
            .map(|(lease, _)| CheckpointPreview(lease))
    }

    fn checkpoint_admission_locked(
        &self,
        state: &ForkGroupsState,
        token: &str,
        session: SessionId,
    ) -> Result<(CheckpointLease, Option<HostedCheckpointAttachment>), CheckpointRefusal> {
        let (namespace, _) = token
            .split_once(':')
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)?;
        if namespace != self.checkpoint_namespace.to_string() {
            return Err(CheckpointRefusal::ProcessRestartUnsupported);
        }
        let lease = match state.checkpoints.get(token) {
            Some(lease) => lease,
            None => {
                return match state.released_checkpoints.get(token) {
                    Some(released) if released.session == session => {
                        Err(CheckpointRefusal::ReleasedCheckpoint)
                    }
                    Some(_) => Err(CheckpointRefusal::WrongSession),
                    None => Err(CheckpointRefusal::UnavailableCheckpoint),
                };
            }
        };
        if lease.session != session {
            return Err(CheckpointRefusal::WrongSession);
        }
        match *lease.phase.borrow() {
            CheckpointPhase::Pending | CheckpointPhase::Published => {}
            CheckpointPhase::Failed => return Err(CheckpointRefusal::CaptureFailed),
            CheckpointPhase::Released | CheckpointPhase::ReleasedAfterPublication => {
                return Err(CheckpointRefusal::ReleasedCheckpoint);
            }
        }
        let attachment = lease.host_attachment.lock().clone();
        Ok((lease.clone(), attachment))
    }

    /// Revoke future admissions and return the captured root for retirement.
    /// A retry returns the same scope until retirement is acknowledged.
    /// Admitted children own independent detached scopes.
    pub fn release_checkpoint(
        &self,
        token: &str,
        session: SessionId,
    ) -> Result<Option<ScopeId>, CheckpointRefusal> {
        let (namespace, _) = token
            .split_once(':')
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)?;
        if namespace != self.checkpoint_namespace.to_string() {
            return Err(CheckpointRefusal::ProcessRestartUnsupported);
        }
        let mut state = self.state.lock();
        if let Some(released) = state.released_checkpoints.get(token) {
            return if released.session == session {
                Ok(released.cleanup_pending.then_some(released.scope).flatten())
            } else {
                Err(CheckpointRefusal::WrongSession)
            };
        }
        let lease = state
            .checkpoints
            .get(token)
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)?;
        if lease.session != session {
            return Err(CheckpointRefusal::WrongSession);
        }
        let lease = state.checkpoints.remove(token).expect("checked checkpoint");
        let retained_scope = lease.retained_scope.as_ref().map(Arc::downgrade);
        let phase = *lease.phase.borrow();
        let scope = match phase {
            CheckpointPhase::Pending => {
                lease.phase.send_replace(CheckpointPhase::Released);
                Some(lease.scope)
            }
            CheckpointPhase::Published => {
                lease
                    .phase
                    .send_replace(CheckpointPhase::ReleasedAfterPublication);
                Some(lease.scope)
            }
            // Failed settlement returns the original captured scope for
            // immediate cleanup, but that cleanup can fail. Transfer it to
            // the same retryable release record used by published captures.
            CheckpointPhase::Failed => Some(lease.scope),
            CheckpointPhase::Released | CheckpointPhase::ReleasedAfterPublication => None,
        };
        state.released_checkpoints.insert(
            token.to_owned(),
            ReleasedCheckpoint {
                session,
                scope,
                cleanup_pending: scope.is_some(),
                retained_scope,
            },
        );
        Ok(scope)
    }

    pub fn confirm_checkpoint_release(
        &self,
        token: &str,
        session: SessionId,
        scope: ScopeId,
    ) -> Result<(), CheckpointRefusal> {
        let mut state = self.state.lock();
        let released = state
            .released_checkpoints
            .get_mut(token)
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)?;
        if released.session != session {
            return Err(CheckpointRefusal::WrongSession);
        }
        if released.scope != Some(scope) {
            return Err(CheckpointRefusal::CaptureFailed);
        }
        released.cleanup_pending = false;
        Ok(())
    }

    pub fn pending_release_scopes(&self, session: SessionId) -> Vec<(String, ScopeId)> {
        self.state
            .lock()
            .released_checkpoints
            .iter()
            .filter_map(|(token, released)| {
                (released.session == session && released.cleanup_pending)
                    .then(|| released.scope.map(|scope| (token.clone(), scope)))
                    .flatten()
            })
            .collect()
    }

    pub fn settle_checkpoints(
        &self,
        issuer: ActorRef,
        boundary: &WorkbenchForkBoundary,
        success: bool,
    ) -> Vec<(SessionId, ScopeId)> {
        let mut state = self.state.lock();
        let mut retired = Vec::new();
        let mut attachments = Vec::new();
        let mut scopes = Vec::new();
        for lease in state.checkpoints.values_mut() {
            if lease.issuer == issuer
                && &lease.boundary == boundary
                && *lease.phase.borrow() == CheckpointPhase::Pending
            {
                lease.phase.send_replace(if success {
                    CheckpointPhase::Published
                } else {
                    CheckpointPhase::Failed
                });
                if !success {
                    retired.push((lease.session, lease.scope));
                    attachments.extend(lease.host_attachment.lock().take());
                    scopes.extend(lease.retained_scope.take());
                }
            }
        }
        drop(state);
        drop(attachments);
        drop(scopes);
        retired
    }

    /// Settle the one checkpoint whose token was delivered at an effect
    /// boundary. A later failure of the enclosing workbench only fails tokens
    /// still Pending; it cannot revoke this independently completed capture.
    pub fn settle_checkpoint(
        &self,
        token: &str,
        session: SessionId,
        delivered: bool,
    ) -> Result<Option<ScopeId>, CheckpointRefusal> {
        let mut state = self.state.lock();
        let lease = state
            .checkpoints
            .get_mut(token)
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)?;
        if lease.session != session {
            return Err(CheckpointRefusal::WrongSession);
        }
        let phase = *lease.phase.borrow();
        let mut attachment = None;
        let mut retained_scope = None;
        let result = match phase {
            CheckpointPhase::Pending => {
                lease.phase.send_replace(if delivered {
                    CheckpointPhase::Published
                } else {
                    CheckpointPhase::Failed
                });
                if !delivered {
                    attachment = lease.host_attachment.lock().take();
                    retained_scope = lease.retained_scope.take();
                }
                Ok((!delivered).then_some(lease.scope))
            }
            CheckpointPhase::Published if delivered => Ok(None),
            CheckpointPhase::Failed if !delivered => Ok(None),
            CheckpointPhase::Released | CheckpointPhase::ReleasedAfterPublication => {
                Err(CheckpointRefusal::ReleasedCheckpoint)
            }
            _ => Err(CheckpointRefusal::CaptureFailed),
        };
        drop(state);
        drop(attachment);
        drop(retained_scope);
        result
    }

    pub fn fail_issuer_checkpoints(&self, issuer: ActorRef) {
        let mut state = self.state.lock();
        let mut attachments = Vec::new();
        let mut scopes = Vec::new();
        for lease in state.checkpoints.values_mut() {
            if lease.issuer == issuer && *lease.phase.borrow() == CheckpointPhase::Pending {
                lease.phase.send_replace(CheckpointPhase::Failed);
                attachments.extend(lease.host_attachment.lock().take());
                scopes.extend(lease.retained_scope.take());
            }
        }
        drop(state);
        drop(attachments);
        drop(scopes);
    }

    pub fn failed_checkpoint_scopes(&self, issuer: ActorRef) -> Vec<(SessionId, ScopeId)> {
        self.state
            .lock()
            .checkpoints
            .values()
            .filter(|lease| {
                lease.issuer == issuer && *lease.phase.borrow() == CheckpointPhase::Failed
            })
            .map(|lease| (lease.session, lease.scope))
            .collect()
    }

    /// A published lease keeps its issuing machine resident after the actor
    /// and its ordinary supervision scope retire.
    pub fn retains_session(&self, session: SessionId) -> bool {
        let state = self.state.lock();
        state.checkpoints.values().any(|lease| {
            lease.session == session && *lease.phase.borrow() == CheckpointPhase::Published
        }) || state.released_checkpoints.values().any(|released| {
            released.session == session
                && (released.cleanup_pending
                    || released
                        .retained_scope
                        .as_ref()
                        .is_some_and(|scope| scope.strong_count() != 0))
        })
    }

    pub fn begin(
        &self,
        owner: ActorRef,
        group: ActorPath,
        children: Vec<ActorPathSegment>,
        maximum_active_descendants: impl Into<Option<usize>>,
    ) -> Result<(ForkGroupId, Vec<ActorPathReservation>), ForkGroupError> {
        self.begin_inner(owner, group, children, maximum_active_descendants, None)
    }

    pub(crate) fn begin_at_boundary(
        &self,
        owner: ActorRef,
        group: ActorPath,
        children: Vec<ActorPathSegment>,
        maximum_active_descendants: impl Into<Option<usize>>,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> Result<(ForkGroupId, Vec<ActorPathReservation>), ForkGroupError> {
        self.begin_inner(
            owner,
            group,
            children,
            maximum_active_descendants,
            Some(boundary),
        )
    }

    fn begin_inner(
        &self,
        owner: ActorRef,
        group: ActorPath,
        children: Vec<ActorPathSegment>,
        maximum_active_descendants: impl Into<Option<usize>>,
        completion_boundary: Option<tidepool_runtime::session::WorkbenchForkBoundary>,
    ) -> Result<(ForkGroupId, Vec<ActorPathReservation>), ForkGroupError> {
        let mut state = self.state.lock();
        if state.cleaning.contains(&owner) {
            return Err(ForkGroupError::Cleaning(owner));
        }
        if let Some(maximum) = maximum_active_descendants.into() {
            state
                .descendant_limits
                .entry(owner)
                .and_modify(|limit| *limit = (*limit).min(maximum))
                .or_insert(maximum);
        }
        // Each coordinator owns its subtree ceiling. Ancestor ceilings still count
        // the whole enclosing subtree, including siblings and staged reservations.
        let mut ancestor = Some(owner);
        while let Some(actor) = ancestor {
            if let Some(&maximum) = state.descendant_limits.get(&actor) {
                let active = reserved_descendants(&state, actor)
                    .saturating_add(sponsored_descendants(&state, actor));
                if active.saturating_add(children.len()) > maximum {
                    return Err(ForkGroupError::DescendantBudgetExceeded {
                        coordinator: actor,
                        requested: children.len(),
                        active,
                        maximum,
                    });
                }
            }
            ancestor = state.parents.get(&actor).copied();
        }
        for (&sponsor, &maximum) in &state.descendant_limits {
            if owner == sponsor || is_descendant_of(&state.parents, owner, sponsor) {
                continue;
            }
            if !sponsored_owner(&state, sponsor, owner) {
                continue;
            }
            let active = reserved_descendants(&state, sponsor)
                .saturating_add(sponsored_descendants(&state, sponsor));
            if active.saturating_add(children.len()) > maximum {
                return Err(ForkGroupError::DescendantBudgetExceeded {
                    coordinator: sponsor,
                    requested: children.len(),
                    active,
                    maximum,
                });
            }
        }
        let reservations = self.lineage.reserve_group_children(&group, &children)?;
        let id = ForkGroupId(state.next);
        state.next += 1;
        state.groups.insert(
            id,
            ForkGroup {
                owner,
                completion_boundary,
                reservations: reservations.clone(),
                checkpoint_sponsors: vec![None; reservations.len()],
                claimed: 0,
                children: Vec::with_capacity(reservations.len()),
                checkpoint_admissions: HashMap::new(),
                selected_admissions: HashMap::new(),
                commit_requested: false,
                publication: None,
                ready: HashSet::new(),
                phase: tokio::sync::watch::channel(ForkGroupPhase::Staging).0,
            },
        );
        Ok((id, reservations))
    }

    pub fn claim(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
        path: &ActorPath,
    ) -> Result<ActorPath, ForkGroupError> {
        self.claim_with_checkpoint(id, owner, path, None)
            .map(|claim| claim.path)
    }

    /// Linearize child admission under the same lock as release: only a full
    /// path and budget claim retains the checkpoint share for installation.
    pub(crate) fn claim_with_checkpoint(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
        path: &ActorPath,
        checkpoint: Option<(&str, SessionId)>,
    ) -> Result<ClaimedForkChild, ForkGroupError> {
        let mut state = self.state.lock();
        let checkpoint = checkpoint
            .map(|(token, session)| self.checkpoint_admission_locked(&state, token, session))
            .transpose()
            .map_err(ForkGroupError::Checkpoint)?;
        let sponsors = if let Some((lease, _)) = &checkpoint {
            for &actor in &lease.budget_sponsors {
                if let Some(&maximum) = state.descendant_limits.get(&actor) {
                    let active = reserved_descendants(&state, actor)
                        .saturating_add(sponsored_descendants(&state, actor));
                    let additional = usize::from(
                        owner != actor && !is_descendant_of(&state.parents, owner, actor),
                    );
                    if active.saturating_add(additional) > maximum {
                        return Err(ForkGroupError::DescendantBudgetExceeded {
                            coordinator: actor,
                            requested: additional,
                            active,
                            maximum,
                        });
                    }
                }
            }
            Some(lease.budget_sponsors.clone())
        } else {
            None
        };
        let group = state
            .groups
            .get_mut(&id)
            .ok_or(ForkGroupError::Unknown(id.0))?;
        if group.owner != owner {
            return Err(ForkGroupError::WrongOwner {
                group: id.0,
                actual: owner,
            });
        }
        if group.commit_requested {
            return Err(ForkGroupError::AlreadyCommitted(id.0));
        }
        let reservation = group
            .reservations
            .get(group.claimed)
            .ok_or(ForkGroupError::TooManyChildren(id.0))?;
        if &reservation.allocated != path {
            return Err(ForkGroupError::WrongChildPath {
                group: id.0,
                expected: reservation.allocated.clone(),
                actual: path.clone(),
            });
        }
        group.checkpoint_sponsors[group.claimed] = sponsors;
        group.claimed += 1;
        Ok(ClaimedForkChild {
            path: reservation.allocated.clone(),
            checkpoint,
        })
    }

    pub fn attach_child(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
        child: ActorRef,
    ) -> Result<(), ForkGroupError> {
        let mut state = self.state.lock();
        let group = state
            .groups
            .get_mut(&id)
            .ok_or(ForkGroupError::Unknown(id.0))?;
        if group.owner != owner {
            return Err(ForkGroupError::WrongOwner {
                group: id.0,
                actual: owner,
            });
        }
        group.children.push(child);
        state.parents.insert(child, owner);
        state.active.insert(child);
        Ok(())
    }

    pub fn gate(&self, id: ForkGroupId, child: ActorRef) -> Result<ForkGroupGate, ForkGroupError> {
        let mut state = self.state.lock();
        let attached_owner = {
            let group = state
                .groups
                .get_mut(&id)
                .ok_or(ForkGroupError::Unknown(id.0))?;
            if !group.children.contains(&child) {
                if group.children.len() >= group.claimed {
                    return Err(ForkGroupError::TooManyChildren(id.0));
                }
                group.children.push(child);
                Some(group.owner)
            } else {
                None
            }
        };
        if let Some(owner) = attached_owner {
            state.parents.insert(child, owner);
            state.active.insert(child);
        }
        Ok(ForkGroupGate {
            id,
            child,
            registry: self.clone(),
        })
    }

    pub(crate) fn retain_checkpoint_admission(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
        child: ActorRef,
        descriptor: &crate::ActorDescriptor,
        lease: &CheckpointLease,
        attachment: Option<&HostedCheckpointAttachment>,
    ) -> Result<(), ForkGroupError> {
        let mut state = self.state.lock();
        let group = state
            .groups
            .get_mut(&id)
            .ok_or(ForkGroupError::Unknown(id.0))?;
        if group.owner != owner {
            return Err(ForkGroupError::WrongOwner {
                group: id.0,
                actual: owner,
            });
        }
        let path = descriptor
            .actor_path()
            .ok_or(ForkGroupError::CheckpointAdmissionMismatch { group: id.0, child })?;
        let token = descriptor
            .checkpoint_token()
            .ok_or(ForkGroupError::CheckpointAdmissionMismatch { group: id.0, child })?;
        if group.commit_requested
            || !group.children.contains(&child)
            || group.checkpoint_admissions.contains_key(&child)
            || group.selected_admissions.contains_key(&child)
            || descriptor.fork_group() != Some(id)
            || descriptor.context_parent() != Some(lease.issuer)
            || descriptor.placement().session != lease.session
            || !group.reservations[..group.claimed]
                .iter()
                .any(|reservation| &reservation.allocated == path)
        {
            return Err(ForkGroupError::CheckpointAdmissionMismatch { group: id.0, child });
        }
        group.checkpoint_admissions.insert(
            child,
            AdmittedForkCheckpoint {
                token: token.to_owned(),
                issuer: lease.issuer,
                checkpoint: (lease.session, lease.scope),
                child: (
                    descriptor.placement().session,
                    descriptor.placement().lexical_scope,
                ),
                path: path.clone(),
                context: attachment.map_or(
                    crate::HostedCheckpointContext::DeferredOnly,
                    HostedCheckpointAttachment::context,
                ),
            },
        );
        Ok(())
    }

    pub(crate) fn checkpoint_admission(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
        child: ActorRef,
    ) -> Result<Option<AdmittedForkCheckpoint>, ForkGroupError> {
        let state = self.state.lock();
        let group = state.groups.get(&id).ok_or(ForkGroupError::Unknown(id.0))?;
        if group.owner != owner {
            return Err(ForkGroupError::WrongOwner {
                group: id.0,
                actual: owner,
            });
        }
        if !group.children.contains(&child) {
            return Err(ForkGroupError::Unknown(id.0));
        }
        Ok(group.checkpoint_admissions.get(&child).cloned())
    }

    pub(crate) fn retain_selected_admission(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
        child: ActorRef,
        descriptor: &crate::ActorDescriptor,
    ) -> Result<(), ForkGroupError> {
        let mut state = self.state.lock();
        let group = state
            .groups
            .get_mut(&id)
            .ok_or(ForkGroupError::Unknown(id.0))?;
        if group.owner != owner {
            return Err(ForkGroupError::WrongOwner {
                group: id.0,
                actual: owner,
            });
        }
        let path = descriptor
            .actor_path()
            .ok_or(ForkGroupError::CheckpointAdmissionMismatch { group: id.0, child })?;
        if group.commit_requested
            || !group.children.contains(&child)
            || group.checkpoint_admissions.contains_key(&child)
            || group.selected_admissions.contains_key(&child)
            || descriptor.fork_group() != Some(id)
            || descriptor.context_parent().is_some()
            || descriptor.checkpoint_token().is_some()
            || !group.reservations[..group.claimed]
                .iter()
                .any(|reservation| &reservation.allocated == path)
        {
            return Err(ForkGroupError::CheckpointAdmissionMismatch { group: id.0, child });
        }
        group.selected_admissions.insert(
            child,
            (
                descriptor.placement().session,
                descriptor.placement().lexical_scope,
                path.clone(),
            ),
        );
        Ok(())
    }

    pub(crate) fn completion_boundary(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
    ) -> Result<Option<WorkbenchForkBoundary>, ForkGroupError> {
        let state = self.state.lock();
        let group = state.groups.get(&id).ok_or(ForkGroupError::Unknown(id.0))?;
        if group.owner != owner {
            return Err(ForkGroupError::WrongOwner {
                group: id.0,
                actual: owner,
            });
        }
        Ok(group.completion_boundary.clone())
    }

    pub fn request_commit(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
    ) -> Result<tokio::sync::watch::Receiver<ForkGroupPhase>, ForkGroupError> {
        let mut state = self.state.lock();
        let group = state
            .groups
            .get_mut(&id)
            .ok_or(ForkGroupError::Unknown(id.0))?;
        if group.owner != owner {
            return Err(ForkGroupError::WrongOwner {
                group: id.0,
                actual: owner,
            });
        }
        if group.commit_requested {
            return Err(ForkGroupError::AlreadyCommitted(id.0));
        }
        if group.claimed != group.reservations.len()
            || group.children.len() != group.reservations.len()
        {
            return Err(ForkGroupError::Incomplete {
                group: id.0,
                started: group.children.len(),
                expected: group.reservations.len(),
            });
        }
        group.commit_requested = true;
        tracing::info!(group = id.0, actor = ?owner, "fork group commit requested");
        publish_if_ready(group);
        Ok(group.phase.subscribe())
    }

    fn mark_ready(&self, id: ForkGroupId, child: ActorRef) -> Result<(), ForkGroupError> {
        let mut state = self.state.lock();
        let group = state
            .groups
            .get_mut(&id)
            .ok_or(ForkGroupError::Unknown(id.0))?;
        if !group.children.contains(&child) {
            return Err(ForkGroupError::Unknown(id.0));
        }
        group.ready.insert(child);
        tracing::info!(group = id.0, actor = ?child, "fork child queue ready");
        publish_if_ready(group);
        Ok(())
    }

    fn mark_failed(&self, id: ForkGroupId, child: ActorRef) -> Result<(), ForkGroupError> {
        let state = self.state.lock();
        let group = state.groups.get(&id).ok_or(ForkGroupError::Unknown(id.0))?;
        if !group.children.contains(&child) {
            return Err(ForkGroupError::Unknown(id.0));
        }
        if *group.phase.borrow() != ForkGroupPhase::Committed {
            group.phase.send_replace(ForkGroupPhase::Aborted);
        }
        Ok(())
    }

    async fn wait_committed(&self, id: ForkGroupId) -> Result<(), ForkGroupError> {
        let mut phase = {
            let state = self.state.lock();
            state
                .groups
                .get(&id)
                .ok_or(ForkGroupError::Unknown(id.0))?
                .phase
                .subscribe()
        };
        loop {
            match *phase.borrow() {
                ForkGroupPhase::Committed => return Ok(()),
                ForkGroupPhase::Aborted => return Err(ForkGroupError::Unknown(id.0)),
                ForkGroupPhase::Staging | ForkGroupPhase::Ready => {}
            }
            phase
                .changed()
                .await
                .map_err(|_| ForkGroupError::Unknown(id.0))?;
        }
    }

    fn committed_publication(
        &self,
        id: ForkGroupId,
        child: ActorRef,
    ) -> Result<ForkGroupPublication, ForkGroupError> {
        let state = self.state.lock();
        let group = state.groups.get(&id).ok_or(ForkGroupError::Unknown(id.0))?;
        if !group.children.contains(&child) {
            return Err(ForkGroupError::Unknown(id.0));
        }
        if *group.phase.borrow() != ForkGroupPhase::Committed {
            return Err(ForkGroupError::NotCommitted(id.0));
        }
        group.publication.ok_or(ForkGroupError::NotCommitted(id.0))
    }

    pub(crate) fn children_for_owner(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
    ) -> Result<Vec<ActorRef>, ForkGroupError> {
        let state = self.state.lock();
        let group = state.groups.get(&id).ok_or(ForkGroupError::Unknown(id.0))?;
        if group.owner != owner {
            return Err(ForkGroupError::WrongOwner {
                group: id.0,
                actual: owner,
            });
        }
        Ok(group.children.clone())
    }

    pub fn abort(&self, id: ForkGroupId, owner: ActorRef) -> Result<Vec<ActorRef>, ForkGroupError> {
        let mut state = self.state.lock();
        let group = state
            .groups
            .remove(&id)
            .ok_or(ForkGroupError::Unknown(id.0))?;
        if group.owner != owner {
            state.groups.insert(id, group);
            return Err(ForkGroupError::WrongOwner {
                group: id.0,
                actual: owner,
            });
        }
        if group.commit_requested {
            state.groups.insert(id, group);
            return Err(ForkGroupError::AlreadyCommitted(id.0));
        }
        group.phase.send_replace(ForkGroupPhase::Aborted);
        for child in &group.children {
            state.active.remove(child);
        }
        drop(state);
        self.lineage
            .release_unpublished(group.reservations.iter().map(|r| &r.allocated));
        Ok(group.children)
    }

    pub fn cleanup_failed(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
    ) -> Result<Vec<ActorRef>, ForkGroupError> {
        let mut state = self.state.lock();
        let group = state
            .groups
            .remove(&id)
            .ok_or(ForkGroupError::Unknown(id.0))?;
        if group.owner != owner {
            state.groups.insert(id, group);
            return Err(ForkGroupError::WrongOwner {
                group: id.0,
                actual: owner,
            });
        }
        if *group.phase.borrow() != ForkGroupPhase::Aborted {
            state.groups.insert(id, group);
            return Err(ForkGroupError::AlreadyCommitted(id.0));
        }
        for child in &group.children {
            state.active.remove(child);
        }
        drop(state);
        self.lineage
            .release_unpublished(group.reservations.iter().map(|r| &r.allocated));
        Ok(group.children)
    }

    pub(crate) fn pending_children_at_boundary(
        &self,
        owner: ActorRef,
        boundary: &WorkbenchForkBoundary,
    ) -> HashSet<ActorRef> {
        self.state
            .lock()
            .groups
            .values()
            .filter(|group| {
                group.owner == owner
                    && group.completion_boundary.as_ref() == Some(boundary)
                    && matches!(
                        *group.phase.borrow(),
                        ForkGroupPhase::Staging | ForkGroupPhase::Ready
                    )
            })
            .flat_map(|group| group.children.iter().copied())
            .collect()
    }

    pub(crate) fn ready_groups_at_boundary(
        &self,
        owner: ActorRef,
        boundary: &tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> Vec<(ForkGroupId, Vec<ActorRef>)> {
        let mut groups: Vec<_> = self
            .state
            .lock()
            .groups
            .iter()
            .filter(|(_, group)| {
                group.owner == owner
                    && group.completion_boundary.as_ref() == Some(boundary)
                    && *group.phase.borrow() == ForkGroupPhase::Ready
            })
            .map(|(id, group)| (*id, group.children.clone()))
            .collect();
        groups.sort_by_key(|(id, _)| id.0);
        groups
    }

    pub(crate) fn publish_groups(
        &self,
        ids: &[ForkGroupId],
        owner: ActorRef,
    ) -> Result<CommittedForkGroups, ForkGroupError> {
        self.publish_group_context(ids, owner, ForkGroupPublication::Deferred)
    }

    pub(crate) fn publish_captured_group(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
        boundary: Option<&WorkbenchForkBoundary>,
        descriptors: &[(ActorRef, crate::ActorDescriptor)],
    ) -> Result<CommittedForkGroups, ForkGroupError> {
        let mut state = self.state.lock();
        let group = state
            .groups
            .get_mut(&id)
            .ok_or(ForkGroupError::Unknown(id.0))?;
        if group.owner != owner {
            return Err(ForkGroupError::WrongOwner {
                group: id.0,
                actual: owner,
            });
        }
        if group.completion_boundary.as_ref() != boundary
            || *group.phase.borrow() != ForkGroupPhase::Ready
        {
            return Err(ForkGroupError::NotReady(id.0));
        }
        for child in &group.children {
            let descriptor = descriptors
                .iter()
                .find(|(actor, _)| actor == child)
                .map(|(_, descriptor)| descriptor);
            let valid = descriptor.is_some_and(|descriptor| {
                descriptor.fork_group() == Some(id)
                    && (group
                        .checkpoint_admissions
                        .get(child)
                        .is_some_and(|admission| {
                            admission.context == crate::HostedCheckpointContext::Captured
                                && admission.matches(descriptor)
                        })
                        || group.selected_admissions.get(child).is_some_and(
                            |(session, scope, path)| {
                                descriptor.checkpoint_token().is_none()
                                    && descriptor.context_parent().is_none()
                                    && descriptor.actor_path() == Some(path)
                                    && descriptor.placement().session == *session
                                    && descriptor.placement().lexical_scope == *scope
                            },
                        ))
            });
            if !valid {
                return Err(ForkGroupError::CheckpointAdmissionMismatch {
                    group: id.0,
                    child: *child,
                });
            }
        }
        if descriptors.len() != group.children.len() {
            return Err(ForkGroupError::TooManyChildren(id.0));
        }
        group.publication = Some(ForkGroupPublication::Captured);
        group.phase.send_replace(ForkGroupPhase::Committed);
        Ok(CommittedForkGroups {
            owner,
            groups: vec![id],
            children: group.children.iter().map(|child| (id, *child)).collect(),
        })
    }

    fn publish_group_context(
        &self,
        ids: &[ForkGroupId],
        owner: ActorRef,
        publication: ForkGroupPublication,
    ) -> Result<CommittedForkGroups, ForkGroupError> {
        let mut state = self.state.lock();
        for id in ids {
            let group = state.groups.get(id).ok_or(ForkGroupError::Unknown(id.0))?;
            if group.owner != owner {
                return Err(ForkGroupError::WrongOwner {
                    group: id.0,
                    actual: owner,
                });
            }
            if *group.phase.borrow() != ForkGroupPhase::Ready {
                return Err(ForkGroupError::NotReady(id.0));
            }
        }
        let mut children = Vec::new();
        for id in ids {
            let group = state.groups.get_mut(id).expect("validated group");
            children.extend(group.children.iter().map(|child| (*id, *child)));
            group.publication = Some(publication);
            group.phase.send_replace(ForkGroupPhase::Committed);
            tracing::info!(group = id.0, actor = ?owner, "fork group published");
        }
        Ok(CommittedForkGroups {
            owner,
            groups: ids.to_vec(),
            children,
        })
    }

    pub(crate) fn publish_ready_in_resident(
        &self,
        owner: ActorRef,
    ) -> Result<Vec<ForkGroupId>, ForkGroupError> {
        let mut state = self.state.lock();
        let mut published = Vec::new();
        for (id, group) in &mut state.groups {
            if group.owner == owner
                && group.completion_boundary.is_none()
                && *group.phase.borrow() == ForkGroupPhase::Ready
            {
                group.publication = Some(ForkGroupPublication::Deferred);
                group.phase.send_replace(ForkGroupPhase::Committed);
                published.push(*id);
            }
        }
        published.sort_by_key(|id| id.0);
        Ok(published)
    }

    #[must_use]
    pub(crate) fn has_ready_at_boundary(
        &self,
        owner: ActorRef,
        boundary: &WorkbenchForkBoundary,
    ) -> bool {
        self.has_ready_matching(owner, ForkGroupBoundary::At(boundary))
    }

    #[must_use]
    pub(crate) fn has_ready_in_resident(&self, owner: ActorRef) -> bool {
        self.has_ready_matching(owner, ForkGroupBoundary::Resident)
    }

    fn has_ready_matching(&self, owner: ActorRef, boundary: ForkGroupBoundary<'_>) -> bool {
        self.state.lock().groups.values().any(|group| {
            group.owner == owner
                && boundary.matches(group.completion_boundary.as_ref())
                && *group.phase.borrow() == ForkGroupPhase::Ready
        })
    }

    #[must_use]
    pub fn has_unpublished(&self, owner: ActorRef) -> bool {
        self.state
            .lock()
            .groups
            .values()
            .any(|group| group.owner == owner && *group.phase.borrow() != ForkGroupPhase::Committed)
    }

    pub fn abort_unpublished(&self, owner: ActorRef) -> Vec<ActorRef> {
        self.abort_pending(owner, true, None, ForkGroupBoundary::Any)
    }

    /// Inspect abort obligations without detaching their existing custody.
    #[must_use]
    pub(crate) fn has_abort_work_at_boundary(
        &self,
        owner: ActorRef,
        boundary: &WorkbenchForkBoundary,
    ) -> bool {
        let state = self.state.lock();
        state.groups.values().any(|group| {
            group.owner == owner
                && group.completion_boundary.as_ref() == Some(boundary)
                && *group.phase.borrow() != ForkGroupPhase::Committed
        }) || state.checkpoints.values().any(|lease| {
            lease.issuer == owner
                && &lease.boundary == boundary
                && *lease.phase.borrow() == CheckpointPhase::Pending
        })
    }

    pub(crate) fn abort_unpublished_at_boundary(
        &self,
        owner: ActorRef,
        boundary: &WorkbenchForkBoundary,
    ) -> Vec<ActorRef> {
        self.abort_pending(owner, true, None, ForkGroupBoundary::At(boundary))
    }

    pub(crate) fn abort_incomplete_at_boundary(
        &self,
        owner: ActorRef,
        boundary: &tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> Vec<ActorRef> {
        self.abort_pending(owner, false, None, ForkGroupBoundary::At(boundary))
    }

    pub(crate) fn abort_incomplete_in_resident(
        &self,
        owner: ActorRef,
        selected: Option<&[ForkGroupId]>,
    ) -> Vec<ActorRef> {
        self.abort_pending(owner, false, selected, ForkGroupBoundary::Resident)
    }

    #[must_use]
    pub(crate) fn has_incomplete_at_boundary(
        &self,
        owner: ActorRef,
        boundary: &WorkbenchForkBoundary,
    ) -> bool {
        self.has_incomplete_matching(owner, ForkGroupBoundary::At(boundary))
    }

    #[must_use]
    pub(crate) fn has_incomplete_in_resident(&self, owner: ActorRef) -> bool {
        self.has_incomplete_matching(owner, ForkGroupBoundary::Resident)
    }

    fn has_incomplete_matching(&self, owner: ActorRef, boundary: ForkGroupBoundary<'_>) -> bool {
        self.state.lock().groups.values().any(|group| {
            group.owner == owner
                && boundary.matches(group.completion_boundary.as_ref())
                && matches!(
                    *group.phase.borrow(),
                    ForkGroupPhase::Staging | ForkGroupPhase::Aborted
                )
        })
    }

    pub(crate) fn abort_selected_unpublished(
        &self,
        owner: ActorRef,
        groups: &[ForkGroupId],
    ) -> Vec<ActorRef> {
        self.abort_pending(owner, true, Some(groups), ForkGroupBoundary::Any)
    }

    fn abort_pending(
        &self,
        owner: ActorRef,
        include_ready: bool,
        selected: Option<&[ForkGroupId]>,
        boundary: ForkGroupBoundary<'_>,
    ) -> Vec<ActorRef> {
        let mut state = self.state.lock();
        let ids: Vec<_> = state
            .groups
            .iter()
            .filter_map(|(id, group)| {
                (group.owner == owner
                    && boundary.matches(group.completion_boundary.as_ref())
                    && selected.is_none_or(|groups| groups.contains(id))
                    && *group.phase.borrow() != ForkGroupPhase::Committed
                    && (include_ready || *group.phase.borrow() != ForkGroupPhase::Ready))
                    .then_some(*id)
            })
            .collect();
        let mut removed = Vec::new();
        for id in ids {
            if let Some(group) = state.groups.remove(&id) {
                group.phase.send_replace(ForkGroupPhase::Aborted);
                for child in &group.children {
                    state.active.remove(child);
                }
                removed.push(group);
            }
        }
        drop(state);
        self.lineage.release_unpublished(
            removed
                .iter()
                .flat_map(|group| group.reservations.iter().map(|r| &r.allocated)),
        );
        removed
            .into_iter()
            .flat_map(|group| group.children)
            .collect()
    }

    pub fn retire_actor(&self, actor: ActorRef) {
        self.state.lock().active.remove(&actor);
    }

    pub fn cleanup_committed(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
    ) -> Result<ForkGroupCleanupOutcome, ForkGroupError> {
        let mut state = self.state.lock();
        if state.cleaned.contains(&id) {
            return Ok(ForkGroupCleanupOutcome::Cleaned);
        }
        let group = state.groups.get(&id).ok_or(ForkGroupError::Unknown(id.0))?;
        if group.owner != owner {
            return Err(ForkGroupError::WrongOwner {
                group: id.0,
                actual: owner,
            });
        }
        if *group.phase.borrow() != ForkGroupPhase::Committed {
            return Err(ForkGroupError::NotCommitted(id.0));
        }
        let mut active = state
            .active
            .iter()
            .copied()
            .filter(|actor| {
                group
                    .children
                    .iter()
                    .any(|child| actor == child || is_descendant_of(&state.parents, *actor, *child))
            })
            .collect::<Vec<_>>();
        active.sort_unstable_by_key(|actor| (actor.id, actor.incarnation));
        if !active.is_empty() {
            return Ok(ForkGroupCleanupOutcome::Active(active));
        }
        let Some(group) = state.groups.remove(&id) else {
            return Err(ForkGroupError::Unknown(id.0));
        };
        for child in group.children {
            state.parents.remove(&child);
            if !state.groups.values().any(|group| {
                group
                    .checkpoint_sponsors
                    .iter()
                    .flatten()
                    .any(|sponsors| sponsors.contains(&child))
            }) && !state
                .checkpoints
                .values()
                .any(|lease| lease.budget_sponsors.contains(&child))
            {
                state.descendant_limits.remove(&child);
            }
        }
        state.cleaned.insert(id);
        Ok(ForkGroupCleanupOutcome::Cleaned)
    }

    /// Exact direct and recursive members of one owned committed group,
    /// ordered deepest-first for supervisor cleanup.
    pub fn members(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
    ) -> Result<Vec<ActorRef>, ForkGroupError> {
        group_members(&self.state.lock(), id, owner)
    }

    pub(crate) fn begin_cleanup(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
        inspected: &[ActorRef],
    ) -> Result<ForkCleanupGuard, ForkGroupError> {
        let mut state = self.state.lock();
        let current = group_members(&state, id, owner)?;
        let actors = inspected.iter().copied().collect::<HashSet<_>>();
        // A successful cleanup prefix can already have removed members.
        // New members must never silently enter the inspected scope.
        if current.iter().any(|actor| !actors.contains(actor)) {
            return Err(ForkGroupError::CleanupScopeChanged(id.0));
        }
        // Only current authorized members may be frozen. Extra identities in
        // an authored plan never grant authority over unrelated actors.
        let actors = current.into_iter().collect::<HashSet<_>>();
        if let Some(actor) = actors.iter().find(|actor| state.cleaning.contains(actor)) {
            return Err(ForkGroupError::Cleaning(*actor));
        }
        if state.groups.values().any(|group| {
            actors.contains(&group.owner) && *group.phase.borrow() != ForkGroupPhase::Committed
        }) {
            return Err(ForkGroupError::CleanupAdmissionPending(id.0));
        }
        state.cleaning.extend(actors.iter().copied());
        Ok(ForkCleanupGuard {
            registry: self.clone(),
            actors,
        })
    }

    /// Committed nested groups owned by members of `id`, followed by `id`
    /// itself. The supervisor uses this after retiring the actor tree so group
    /// admission records disappear inside-out as well.
    pub fn cleanup_group_order(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
    ) -> Result<Vec<(ForkGroupId, ActorRef)>, ForkGroupError> {
        let state = self.state.lock();
        if state.cleaned.contains(&id) {
            return Ok(Vec::new());
        }
        let root_group = state.groups.get(&id).ok_or(ForkGroupError::Unknown(id.0))?;
        if root_group.owner != owner {
            return Err(ForkGroupError::WrongOwner {
                group: id.0,
                actual: owner,
            });
        }
        if *root_group.phase.borrow() != ForkGroupPhase::Committed {
            return Err(ForkGroupError::NotCommitted(id.0));
        }
        let roots = &root_group.children;
        let mut nested = state
            .groups
            .iter()
            .filter_map(|(candidate_id, candidate)| {
                if *candidate_id == id || *candidate.phase.borrow() != ForkGroupPhase::Committed {
                    return None;
                }
                roots
                    .iter()
                    .filter_map(|root| descendant_depth(&state.parents, candidate.owner, *root))
                    .max()
                    .map(|depth| (depth, *candidate_id, candidate.owner))
            })
            .collect::<Vec<_>>();
        nested.sort_unstable_by_key(|(depth, group, _)| (std::cmp::Reverse(*depth), group.0));
        let mut ordered = nested
            .into_iter()
            .map(|(_, group, group_owner)| (group, group_owner))
            .collect::<Vec<_>>();
        ordered.push((id, owner));
        Ok(ordered)
    }
}

fn descendant_depth(
    parents: &HashMap<ActorRef, ActorRef>,
    mut actor: ActorRef,
    root: ActorRef,
) -> Option<usize> {
    let mut depth = 0;
    loop {
        if actor == root {
            return Some(depth);
        }
        actor = *parents.get(&actor)?;
        depth = depth.saturating_add(1);
    }
}

fn is_descendant_of(
    parents: &HashMap<ActorRef, ActorRef>,
    mut actor: ActorRef,
    root: ActorRef,
) -> bool {
    loop {
        let Some(parent) = parents.get(&actor).copied() else {
            return false;
        };
        if parent == root {
            return true;
        }
        actor = parent;
    }
}

fn reserved_descendants(state: &ForkGroupsState, root: ActorRef) -> usize {
    let active = state
        .active
        .iter()
        .filter(|actor| is_descendant_of(&state.parents, **actor, root))
        .count();
    let unclaimed = state
        .groups
        .values()
        .filter(|group| group.owner == root || is_descendant_of(&state.parents, group.owner, root))
        .map(|group| {
            group
                .reservations
                .len()
                .saturating_sub(group.children.len())
        })
        .sum::<usize>();
    active + unclaimed
}

fn sponsored_descendants(state: &ForkGroupsState, ancestor: ActorRef) -> usize {
    state
        .groups
        .values()
        .map(|group| {
            group
                .checkpoint_sponsors
                .iter()
                .enumerate()
                .filter_map(|(index, sponsors)| {
                    if !sponsors.as_ref()?.contains(&ancestor) {
                        return None;
                    }
                    if group.owner == ancestor
                        || is_descendant_of(&state.parents, group.owner, ancestor)
                        || sponsored_owner(state, ancestor, group.owner)
                    {
                        return None;
                    }
                    if index >= group.claimed {
                        return None;
                    }
                    match group.children.get(index) {
                        Some(child) if state.active.contains(child) => {
                            Some(1 + reserved_descendants(state, *child))
                        }
                        Some(_) => Some(0),
                        None => Some(1),
                    }
                })
                .sum::<usize>()
        })
        .sum()
}

fn sponsored_owner(state: &ForkGroupsState, sponsor: ActorRef, owner: ActorRef) -> bool {
    state.groups.values().any(|group| {
        group.children.iter().enumerate().any(|(index, child)| {
            group
                .checkpoint_sponsors
                .get(index)
                .and_then(Option::as_ref)
                .is_some_and(|sponsors| {
                    sponsors.contains(&sponsor)
                        && (owner == *child || is_descendant_of(&state.parents, owner, *child))
                })
        })
    })
}

fn publish_if_ready(group: &mut ForkGroup) {
    if group.commit_requested
        && group.ready.len() == group.reservations.len()
        && *group.phase.borrow() == ForkGroupPhase::Staging
    {
        group.phase.send_replace(ForkGroupPhase::Ready);
    }
}

fn lowest_available(
    occupied: impl Fn(&ActorPath) -> bool,
    prefix: &[ActorPathSegment],
    leaf: &ActorPathSegment,
) -> Result<ActorPath, ActorPathError> {
    let candidate = ActorPath::new(
        prefix
            .iter()
            .cloned()
            .chain(std::iter::once(leaf.clone()))
            .collect(),
    )?;
    if !occupied(&candidate) {
        return Ok(candidate);
    }
    for ordinal in 1.. {
        let numbered = leaf.numbered(ordinal)?;
        let candidate = ActorPath::new(
            prefix
                .iter()
                .cloned()
                .chain(std::iter::once(numbered))
                .collect(),
        )?;
        if !occupied(&candidate) {
            return Ok(candidate);
        }
    }
    unreachable!("unbounded numeric suffix space")
}

#[cfg(test)]
#[path = "lineage/captured_tests.rs"]
mod captured_tests;

#[cfg(test)]
#[path = "lineage/capture_lifetime_tests.rs"]
mod capture_lifetime_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorId, ActorRef};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct AttachmentDrop(Arc<AtomicUsize>);

    impl Drop for AttachmentDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn checkpoint_with_attachment(
        groups: &ForkGroupRegistry,
        issuer: ActorRef,
        boundary: WorkbenchForkBoundary,
        scope: ScopeId,
    ) -> (String, Arc<AtomicUsize>) {
        let drops = Arc::new(AtomicUsize::new(0));
        let attachment =
            HostedCheckpointAttachment::new(Arc::new(AttachmentDrop(Arc::clone(&drops))));
        let caller_share = attachment.clone();
        let token = groups.capture_checkpoint_with_host_attachment(
            "captured".into(),
            issuer,
            crate::EffectiveRole::root(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            SessionId(7),
            scope,
            boundary,
            Some(attachment),
        );
        drop(caller_share);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        (token, drops)
    }

    #[test]
    fn undelivered_checkpoint_drops_opaque_host_attachment() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let issuer = ActorRef::first(ActorId(1));
        let (token, drops) = checkpoint_with_attachment(
            &groups,
            issuer,
            WorkbenchForkBoundary::external("thread".into(), "call".into(), "call".into()),
            ScopeId(3),
        );
        let retained_waiter = groups.checkpoint(&token, SessionId(7)).unwrap();
        assert_eq!(
            groups.settle_checkpoint(&token, SessionId(7), false),
            Ok(Some(ScopeId(3)))
        );
        assert!(retained_waiter
            .host_attachment::<AttachmentDrop>()
            .is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            groups.settle_checkpoint(&token, SessionId(7), false),
            Ok(None)
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(matches!(
            groups.checkpoint(&token, SessionId(7)),
            Err(CheckpointRefusal::CaptureFailed)
        ));
    }

    #[test]
    fn checkpoint_claim_linearizes_release_before_child_installation() {
        for release_before_claim in [true, false] {
            let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
            let issuer = ActorRef::first(ActorId(1));
            let (token, drops) = checkpoint_with_attachment(
                &groups,
                issuer,
                WorkbenchForkBoundary::external("thread".into(), "call".into(), "call".into()),
                ScopeId(9),
            );
            let (group, paths) = groups
                .begin(
                    issuer,
                    ActorPath::parse("work").unwrap(),
                    vec![segment("child")],
                    None,
                )
                .unwrap();
            let preview = groups.preview_checkpoint(&token, SessionId(7)).unwrap();
            assert!(matches!(
                groups.claim_with_checkpoint(
                    group,
                    issuer,
                    &paths[0].allocated,
                    Some((&token, SessionId(8)))
                ),
                Err(ForkGroupError::Checkpoint(CheckpointRefusal::WrongSession))
            ));
            assert!(matches!(
                groups.claim_with_checkpoint(
                    group,
                    issuer,
                    &ActorPath::parse("wrong/child").unwrap(),
                    Some((&token, SessionId(7)))
                ),
                Err(ForkGroupError::WrongChildPath { .. })
            ));
            assert_eq!(groups.state.lock().groups[&group].claimed, 0);
            if release_before_claim {
                groups
                    .settle_checkpoint(&token, SessionId(7), true)
                    .unwrap();
                assert_eq!(
                    groups.release_checkpoint(&token, SessionId(7)),
                    Ok(Some(ScopeId(9)))
                );
            }
            let claim = groups.claim_with_checkpoint(
                group,
                issuer,
                &paths[0].allocated,
                Some((&token, SessionId(7))),
            );
            if release_before_claim {
                assert!(matches!(
                    claim,
                    Err(ForkGroupError::Checkpoint(
                        CheckpointRefusal::ReleasedCheckpoint
                    ))
                ));
                let state = groups.state.lock();
                let reservation = &state.groups[&group];
                assert_eq!(
                    reservation.claimed, 0,
                    "preview cannot consume a child reservation"
                );
                assert!(reservation.checkpoint_sponsors[0].is_none());
                drop(state);
                assert_eq!(
                    drops.load(Ordering::SeqCst),
                    0,
                    "preview still owns its metadata share"
                );
                drop(preview);
                assert_eq!(drops.load(Ordering::SeqCst), 1);
            } else {
                let claim = claim.expect("full registry claim wins before release");
                assert_eq!(claim.path, paths[0].allocated);
                let (admitted_lease, admitted_attachment) = claim.checkpoint.unwrap();
                drop(preview);
                groups
                    .settle_checkpoint(&token, SessionId(7), true)
                    .unwrap();
                assert_eq!(
                    groups.release_checkpoint(&token, SessionId(7)),
                    Ok(Some(ScopeId(9)))
                );
                assert!(matches!(
                    groups.preview_checkpoint(&token, SessionId(7)),
                    Err(CheckpointRefusal::ReleasedCheckpoint)
                ));
                assert!(admitted_lease.host_attachment::<AttachmentDrop>().is_some());
                let retained = admitted_attachment
                    .expect("claimed child owns the original opaque attachment")
                    .downcast::<AttachmentDrop>()
                    .expect("installation after release receives the original attachment type");
                assert_eq!(drops.load(Ordering::SeqCst), 0);
                drop(admitted_lease);
                drop(retained);
                assert_eq!(drops.load(Ordering::SeqCst), 1);
            }
            assert_eq!(
                groups.release_checkpoint(&token, SessionId(7)),
                Ok(Some(ScopeId(9)))
            );
            groups
                .confirm_checkpoint_release(&token, SessionId(7), ScopeId(9))
                .unwrap();
            assert_eq!(groups.release_checkpoint(&token, SessionId(7)), Ok(None));
        }
    }

    #[test]
    fn failed_workbench_boundary_drops_only_its_pending_host_attachments() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let issuer = ActorRef::first(ActorId(1));
        let failed_boundary =
            WorkbenchForkBoundary::external("thread".into(), "failed".into(), "failed".into());
        let live_boundary =
            WorkbenchForkBoundary::external("thread".into(), "live".into(), "live".into());
        let (_, failed_drops) =
            checkpoint_with_attachment(&groups, issuer, failed_boundary.clone(), ScopeId(3));
        let (live, live_drops) =
            checkpoint_with_attachment(&groups, issuer, live_boundary.clone(), ScopeId(4));
        assert_eq!(
            groups.settle_checkpoints(issuer, &failed_boundary, false),
            vec![(SessionId(7), ScopeId(3))]
        );
        assert_eq!(failed_drops.load(Ordering::SeqCst), 1);
        assert_eq!(live_drops.load(Ordering::SeqCst), 0);
        groups.settle_checkpoints(issuer, &live_boundary, true);
        assert_eq!(live_drops.load(Ordering::SeqCst), 0);
        assert_eq!(
            groups.release_checkpoint(&live, SessionId(7)),
            Ok(Some(ScopeId(4)))
        );
        assert_eq!(live_drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn issuer_failure_drops_only_pending_host_attachments() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let issuer = ActorRef::first(ActorId(1));
        let other = ActorRef::first(ActorId(2));
        let boundary =
            WorkbenchForkBoundary::external("thread".into(), "call".into(), "call".into());
        let (_, failed_drops) =
            checkpoint_with_attachment(&groups, issuer, boundary.clone(), ScopeId(3));
        let (published, published_drops) = checkpoint_with_attachment(
            &groups,
            issuer,
            WorkbenchForkBoundary::external("thread".into(), "call".into(), "published".into()),
            ScopeId(4),
        );
        let (unrelated, unrelated_drops) =
            checkpoint_with_attachment(&groups, other, boundary, ScopeId(5));
        assert_eq!(
            groups.settle_checkpoint(&published, SessionId(7), true),
            Ok(None)
        );
        groups.fail_issuer_checkpoints(issuer);
        groups.fail_issuer_checkpoints(issuer);
        assert_eq!(failed_drops.load(Ordering::SeqCst), 1);
        assert_eq!(published_drops.load(Ordering::SeqCst), 0);
        assert_eq!(unrelated_drops.load(Ordering::SeqCst), 0);
        assert_eq!(
            groups.release_checkpoint(&published, SessionId(7)),
            Ok(Some(ScopeId(4)))
        );
        assert_eq!(
            groups.release_checkpoint(&unrelated, SessionId(7)),
            Ok(Some(ScopeId(5)))
        );
        assert_eq!(published_drops.load(Ordering::SeqCst), 1);
        assert_eq!(unrelated_drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn checkpoint_waits_for_exact_boundary_and_survives_issuer_retirement() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let issuer = ActorRef::first(ActorId(1));
        let boundary =
            WorkbenchForkBoundary::external("thread".into(), "call".into(), "call".into());
        let token = groups.capture_checkpoint(
            "research".into(),
            issuer,
            crate::EffectiveRole::root(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            SessionId(7),
            ScopeId(3),
            boundary.clone(),
        );
        let lease = groups.checkpoint(&token, SessionId(7)).unwrap();
        assert!(matches!(
            groups.checkpoint(&token, SessionId(8)),
            Err(CheckpointRefusal::WrongSession)
        ));
        assert_eq!(*lease.phase.borrow(), CheckpointPhase::Pending);
        assert!(!groups.retains_session(SessionId(7)));
        assert!(groups
            .settle_checkpoints(
                issuer,
                &WorkbenchForkBoundary::external("thread".into(), "other".into(), "other".into(),),
                true
            )
            .is_empty());
        assert_eq!(*lease.phase.borrow(), CheckpointPhase::Pending);
        groups.settle_checkpoints(issuer, &boundary, true);
        groups.retire_actor(issuer);
        assert!(groups.retains_session(SessionId(7)));
        lease.wait_published().await.unwrap();
        assert_eq!(
            groups.checkpoint(&token, SessionId(7)).unwrap().scope,
            ScopeId(3)
        );
    }

    #[tokio::test]
    async fn failed_checkpoint_refuses_delegation_and_restart_namespace() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let issuer = ActorRef::first(ActorId(1));
        let boundary =
            WorkbenchForkBoundary::external("thread".into(), "call".into(), "call".into());
        let token = groups.capture_checkpoint(
            "research".into(),
            issuer,
            crate::EffectiveRole::root(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            SessionId(7),
            ScopeId(3),
            boundary.clone(),
        );
        let lease = groups.checkpoint(&token, SessionId(7)).unwrap();
        assert_eq!(
            groups.settle_checkpoints(issuer, &boundary, false),
            vec![(SessionId(7), ScopeId(3))]
        );
        assert_eq!(
            lease.wait_published().await,
            Err(CheckpointRefusal::CaptureFailed)
        );
        assert!(matches!(
            groups.checkpoint(&token, SessionId(7)),
            Err(CheckpointRefusal::CaptureFailed)
        ));
        let restarted = ForkGroupRegistry::new(ActorLineageRegistry::default());
        assert!(matches!(
            restarted.checkpoint(&token, SessionId(7)),
            Err(CheckpointRefusal::ProcessRestartUnsupported)
        ));
    }

    #[tokio::test]
    async fn delivered_checkpoint_survives_later_failure_of_its_workbench_boundary() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let issuer = ActorRef::first(ActorId(1));
        let boundary =
            WorkbenchForkBoundary::external("thread".into(), "call".into(), "call".into());
        let capture = |name: &str, scope: ScopeId| {
            groups.capture_checkpoint(
                name.into(),
                issuer,
                crate::EffectiveRole::root(),
                None,
                None,
                crate::CheckpointSourceLayer::default(),
                SessionId(7),
                scope,
                boundary.clone(),
            )
        };
        let delivered = capture("completed item", ScopeId(3));
        let incomplete = capture("failed item", ScopeId(4));
        let lease = groups.checkpoint(&delivered, SessionId(7)).unwrap();
        assert_eq!(
            groups.settle_checkpoint(&delivered, SessionId(7), true),
            Ok(None)
        );
        lease.wait_published().await.unwrap();
        assert_eq!(
            groups.settle_checkpoints(issuer, &boundary, false),
            vec![(SessionId(7), ScopeId(4))]
        );
        assert!(groups.checkpoint(&delivered, SessionId(7)).is_ok());
        assert!(groups.retains_session(SessionId(7)));
        assert_eq!(
            groups.checkpoint(&incomplete, SessionId(7)).err(),
            Some(CheckpointRefusal::CaptureFailed)
        );
        assert_eq!(
            groups.release_checkpoint(&delivered, SessionId(7)),
            Ok(Some(ScopeId(3)))
        );
        assert!(groups.retains_session(SessionId(7)));
        groups
            .confirm_checkpoint_release(&delivered, SessionId(7), ScopeId(3))
            .unwrap();
        assert!(!groups.retains_session(SessionId(7)));
    }

    #[tokio::test]
    async fn release_is_idempotent_and_revokes_pending_or_published_lease() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let issuer = ActorRef::first(ActorId(1));
        let boundary =
            WorkbenchForkBoundary::external("thread".into(), "call".into(), "call".into());
        let capture = || {
            groups.capture_checkpoint(
                "seed".into(),
                issuer,
                crate::EffectiveRole::root(),
                None,
                None,
                crate::CheckpointSourceLayer::default(),
                SessionId(7),
                ScopeId(3),
                boundary.clone(),
            )
        };
        let pending = capture();
        let pending_lease = groups.checkpoint(&pending, SessionId(7)).unwrap();
        assert_eq!(
            groups.release_checkpoint(&pending, SessionId(8)),
            Err(CheckpointRefusal::WrongSession)
        );
        assert_eq!(
            groups.release_checkpoint(&pending, SessionId(7)),
            Ok(Some(ScopeId(3)))
        );
        assert_eq!(
            groups.release_checkpoint(&pending, SessionId(7)),
            Ok(Some(ScopeId(3)))
        );
        groups
            .confirm_checkpoint_release(&pending, SessionId(7), ScopeId(3))
            .unwrap();
        assert_eq!(groups.release_checkpoint(&pending, SessionId(7)), Ok(None));
        groups.settle_checkpoints(issuer, &boundary, true);
        assert_eq!(
            pending_lease.wait_published().await,
            Err(CheckpointRefusal::ReleasedCheckpoint)
        );
        assert!(matches!(
            groups.checkpoint(&pending, SessionId(7)),
            Err(CheckpointRefusal::ReleasedCheckpoint)
        ));
        assert_eq!(
            groups.release_checkpoint(&pending, SessionId(8)),
            Err(CheckpointRefusal::WrongSession)
        );
        let published = capture();
        let published_lease = groups.checkpoint(&published, SessionId(7)).unwrap();
        groups.settle_checkpoints(issuer, &boundary, true);
        assert!(groups.retains_session(SessionId(7)));
        assert_eq!(
            groups.release_checkpoint(&published, SessionId(7)),
            Ok(Some(ScopeId(3)))
        );
        assert!(groups.retains_session(SessionId(7)));
        groups
            .confirm_checkpoint_release(&published, SessionId(7), ScopeId(3))
            .unwrap();
        assert_eq!(
            groups.confirm_checkpoint_release(&published, SessionId(7), ScopeId(3)),
            Ok(())
        );
        assert_eq!(
            groups.confirm_checkpoint_release(&published, SessionId(7), ScopeId(4)),
            Err(CheckpointRefusal::CaptureFailed)
        );
        assert!(!groups.retains_session(SessionId(7)));
        published_lease.wait_published().await.unwrap();
        assert_eq!(
            groups.release_checkpoint(&published, SessionId(7)),
            Ok(None)
        );
        let restarted = ForkGroupRegistry::new(ActorLineageRegistry::default());
        assert_eq!(
            restarted.release_checkpoint(&published, SessionId(7)),
            Err(CheckpointRefusal::ProcessRestartUnsupported)
        );
    }

    #[test]
    fn failed_checkpoint_release_retains_scope_until_retry_is_confirmed() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let issuer = ActorRef::first(ActorId(1));
        let boundary =
            WorkbenchForkBoundary::external("thread".into(), "call".into(), "call".into());
        let token = groups.capture_checkpoint(
            "failed capture".into(),
            issuer,
            crate::EffectiveRole::root(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            SessionId(7),
            ScopeId(3),
            boundary,
        );

        // Failed settlement starts retirement of the original captured scope.
        assert_eq!(
            groups.settle_checkpoint(&token, SessionId(7), false),
            Ok(Some(ScopeId(3)))
        );
        assert_eq!(
            groups.failed_checkpoint_scopes(issuer),
            vec![(SessionId(7), ScopeId(3))]
        );

        // Model a failed first retirement by withholding confirmation.
        // Releasing the token transfers the same obligation into the retryable
        // released-checkpoint record.
        assert_eq!(
            groups.release_checkpoint(&token, SessionId(7)),
            Ok(Some(ScopeId(3)))
        );
        assert!(groups.failed_checkpoint_scopes(issuer).is_empty());
        assert_eq!(
            groups.pending_release_scopes(SessionId(7)),
            vec![(token.clone(), ScopeId(3))]
        );
        assert!(groups.retains_session(SessionId(7)));

        // An unconfirmed retry returns the original scope again. Confirmation
        // is the only operation that consumes the pending retirement.
        assert_eq!(
            groups.release_checkpoint(&token, SessionId(7)),
            Ok(Some(ScopeId(3)))
        );
        groups
            .confirm_checkpoint_release(&token, SessionId(7), ScopeId(3))
            .unwrap();
        assert!(groups.pending_release_scopes(SessionId(7)).is_empty());
        assert!(!groups.retains_session(SessionId(7)));
        assert_eq!(groups.release_checkpoint(&token, SessionId(7)), Ok(None));
    }

    #[test]
    fn delegated_checkpoint_charges_issuer_width_and_recursive_descendants() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let issuer = ActorRef::first(ActorId(1));
        let coordinator = ActorRef::first(ActorId(2));
        let child = ActorRef::first(ActorId(3));
        let token = groups.capture_checkpoint(
            "limited".into(),
            issuer,
            crate::EffectiveRole::root().with_descendant_budget(crate::DescendantBudget {
                maximum_depth: 3,
                maximum_active_children: Some(1),
            }),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            SessionId(7),
            ScopeId(3),
            WorkbenchForkBoundary::external("thread".into(), "call".into(), "call".into()),
        );
        let (first, paths) = groups
            .begin(
                coordinator,
                ActorPath::parse("coordinator/first").unwrap(),
                vec![segment("child")],
                None,
            )
            .unwrap();
        groups
            .claim_with_checkpoint(
                first,
                coordinator,
                &paths[0].allocated,
                Some((&token, SessionId(7))),
            )
            .unwrap();
        groups.attach_child(first, coordinator, child).unwrap();
        let (second, paths) = groups
            .begin(
                coordinator,
                ActorPath::parse("coordinator/second").unwrap(),
                vec![segment("child")],
                None,
            )
            .unwrap();
        assert!(matches!(
            groups.claim_with_checkpoint(
                second,
                coordinator,
                &paths[0].allocated,
                Some((&token, SessionId(7)))
            ),
            Err(ForkGroupError::DescendantBudgetExceeded { maximum: 1, .. })
        ));
        assert!(matches!(
            groups.begin(
                child,
                ActorPath::parse("coordinator/first/child/work").unwrap(),
                vec![segment("nested")],
                None
            ),
            Err(ForkGroupError::DescendantBudgetExceeded { maximum: 1, .. })
        ));
    }

    #[test]
    fn released_checkpoint_keeps_issuer_budget_charged_until_child_cleanup() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let root = ActorRef::first(ActorId(5));
        let issuer = ActorRef::first(ActorId(1));
        let coordinator = ActorRef::first(ActorId(2));
        let first_child = ActorRef::first(ActorId(3));
        let second_child = ActorRef::first(ActorId(4));
        let (issuer_group, paths) = groups
            .begin(
                root,
                ActorPath::parse("root/issuer").unwrap(),
                vec![segment("worker")],
                None,
            )
            .unwrap();
        groups
            .claim(issuer_group, root, &paths[0].allocated)
            .unwrap();
        groups.attach_child(issuer_group, root, issuer).unwrap();
        groups.request_commit(issuer_group, root).unwrap();
        groups
            .gate(issuer_group, issuer)
            .unwrap()
            .mark_ready()
            .unwrap();
        groups.publish_groups(&[issuer_group], root).unwrap();
        let role = crate::EffectiveRole::root().with_descendant_budget(crate::DescendantBudget {
            maximum_depth: 3,
            maximum_active_children: Some(2),
        });
        let capture = |name: &str| {
            groups.capture_checkpoint(
                name.into(),
                issuer,
                role.clone(),
                None,
                None,
                crate::CheckpointSourceLayer::default(),
                SessionId(7),
                ScopeId(if name == "first" { 3 } else { 4 }),
                WorkbenchForkBoundary::external("thread".into(), name.into(), name.into()),
            )
        };
        let first = capture("first");
        let second = capture("second");
        groups.settle_checkpoints(
            issuer,
            &WorkbenchForkBoundary::external("thread".into(), "first".into(), "first".into()),
            true,
        );
        groups.settle_checkpoints(
            issuer,
            &WorkbenchForkBoundary::external("thread".into(), "second".into(), "second".into()),
            true,
        );
        let admit = |token: &str, label: &str, child: ActorRef| {
            let (group, paths) = groups
                .begin(
                    coordinator,
                    ActorPath::parse(label).unwrap(),
                    vec![segment("child")],
                    None,
                )
                .unwrap();
            groups
                .claim_with_checkpoint(
                    group,
                    coordinator,
                    &paths[0].allocated,
                    Some((token, SessionId(7))),
                )
                .unwrap();
            groups.attach_child(group, coordinator, child).unwrap();
            groups.request_commit(group, coordinator).unwrap();
            groups.gate(group, child).unwrap().mark_ready().unwrap();
            groups.publish_groups(&[group], coordinator).unwrap();
            group
        };
        let first_group = admit(&first, "coordinator/first", first_child);
        let _second_group = admit(&second, "coordinator/second", second_child);
        assert_eq!(
            groups.release_checkpoint(&first, SessionId(7)),
            Ok(Some(ScopeId(3)))
        );
        groups.retire_actor(issuer);
        assert_eq!(
            groups.cleanup_committed(issuer_group, root).unwrap(),
            ForkGroupCleanupOutcome::Cleaned
        );

        assert!(matches!(
            groups.begin(
                first_child,
                ActorPath::parse("coordinator/first/child/work").unwrap(),
                vec![segment("nested")],
                None,
            ),
            Err(ForkGroupError::DescendantBudgetExceeded { coordinator, maximum: 2, .. }) if coordinator == issuer
        ));
        let (extra, paths) = groups
            .begin(
                coordinator,
                ActorPath::parse("coordinator/extra").unwrap(),
                vec![segment("child")],
                None,
            )
            .unwrap();
        assert!(matches!(
            groups.claim_with_checkpoint(extra, coordinator, &paths[0].allocated, Some((&second, SessionId(7)))),
            Err(ForkGroupError::DescendantBudgetExceeded { coordinator, maximum: 2, .. }) if coordinator == issuer
        ));
        groups.abort(extra, coordinator).unwrap();

        groups.retire_actor(first_child);
        assert_eq!(
            groups.cleanup_committed(first_group, coordinator).unwrap(),
            ForkGroupCleanupOutcome::Cleaned
        );
        let (freed, paths) = groups
            .begin(
                coordinator,
                ActorPath::parse("coordinator/freed").unwrap(),
                vec![segment("child")],
                None,
            )
            .unwrap();
        groups
            .claim_with_checkpoint(
                freed,
                coordinator,
                &paths[0].allocated,
                Some((&second, SessionId(7))),
            )
            .unwrap();
    }

    #[tokio::test]
    async fn checkpoint_child_marks_group_ready_before_capture_is_published() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let issuer = ActorRef::first(ActorId(1));
        let coordinator = ActorRef::first(ActorId(2));
        let child = ActorRef::first(ActorId(3));
        let boundary =
            WorkbenchForkBoundary::external("thread".into(), "call".into(), "call".into());
        let token = groups.capture_checkpoint(
            "research".into(),
            issuer,
            crate::EffectiveRole::root(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            SessionId(7),
            ScopeId(3),
            boundary.clone(),
        );
        let lease = groups.checkpoint(&token, SessionId(7)).unwrap();
        let (group, paths) = groups
            .begin(
                coordinator,
                ActorPath::parse("coordinator/work").unwrap(),
                vec![segment("child")],
                None,
            )
            .unwrap();
        groups
            .claim_with_checkpoint(
                group,
                coordinator,
                &paths[0].allocated,
                Some((&token, SessionId(7))),
            )
            .unwrap();
        groups.attach_child(group, coordinator, child).unwrap();
        let phase = groups.request_commit(group, coordinator).unwrap();
        let gate = groups.gate(group, child).unwrap();
        gate.mark_ready().unwrap();
        assert_eq!(*phase.borrow(), ForkGroupPhase::Ready);
        groups.publish_groups(&[group], coordinator).unwrap();
        gate.wait_committed().await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), lease.wait_published())
                .await
                .is_err()
        );
        groups.settle_checkpoints(issuer, &boundary, true);
        lease.wait_published().await.unwrap();
    }

    fn segment(value: &str) -> ActorPathSegment {
        ActorPathSegment::new(value).unwrap()
    }

    #[test]
    fn campaign_is_idempotent_per_root_and_collisions_are_named() {
        let registry = ActorLineageRegistry::default();
        let first = registry
            .reserve_campaign(ActorRef::first(ActorId(1)), segment("compiler"))
            .unwrap();
        let same = registry
            .reserve_campaign(ActorRef::first(ActorId(1)), segment("compiler"))
            .unwrap();
        let other = registry
            .reserve_campaign(ActorRef::first(ActorId(2)), segment("compiler"))
            .unwrap();
        assert_eq!(first.allocated, same.allocated);
        assert_eq!(first.allocated.to_string(), "compiler");
        assert_eq!(other.allocated.to_string(), "compiler-1");
    }

    #[test]
    fn repeated_siblings_are_numbered_in_applicative_order() {
        let registry = ActorLineageRegistry::default();
        let parent = ActorPath::parse("compiler").unwrap();
        let paths = registry
            .reserve_children(
                &parent,
                segment("runtime"),
                &[segment("parser"), segment("parser"), segment("tests")],
            )
            .unwrap();
        assert_eq!(
            paths
                .iter()
                .map(|r| r.allocated.to_string())
                .collect::<Vec<_>>(),
            [
                "compiler/runtime/parser-1",
                "compiler/runtime/parser-2",
                "compiler/runtime/tests",
            ]
        );
    }

    #[test]
    fn retained_path_gets_lowest_available_suffix() {
        let registry = ActorLineageRegistry::default();
        registry.retain_external(ActorPath::parse("compiler/runtime/tests").unwrap());
        let paths = registry
            .reserve_children(
                &ActorPath::parse("compiler").unwrap(),
                segment("runtime"),
                &[segment("tests")],
            )
            .unwrap();
        assert_eq!(paths[0].allocated.to_string(), "compiler/runtime/tests-1");
    }

    #[test]
    fn sibling_collisions_include_retained_paths_and_the_current_batch() {
        let registry = ActorLineageRegistry::default();
        let group = ActorPath::parse("compiler/runtime").unwrap();
        for path in ["compiler/runtime/parser-1", "compiler/runtime/parser-1-1"] {
            registry.retain_external(ActorPath::parse(path).unwrap());
        }
        let children = [segment("parser"), segment("parser"), segment("parser-1")];
        let paths = registry.reserve_group_children(&group, &children).unwrap();
        assert_eq!(
            paths
                .iter()
                .map(|r| (r.requested.to_string(), r.allocated.to_string()))
                .collect::<Vec<_>>(),
            [
                (
                    "compiler/runtime/parser-1".to_owned(),
                    "compiler/runtime/parser-1-2".to_owned()
                ),
                (
                    "compiler/runtime/parser-2".to_owned(),
                    "compiler/runtime/parser-2".to_owned()
                ),
                (
                    "compiler/runtime/parser-1".to_owned(),
                    "compiler/runtime/parser-1-3".to_owned()
                ),
            ]
        );
        let next = registry.reserve_group_children(&group, &children).unwrap();
        assert_eq!(
            next.iter()
                .map(|r| r.allocated.to_string())
                .collect::<Vec<_>>(),
            [
                "compiler/runtime/parser-1-4",
                "compiler/runtime/parser-2-1",
                "compiler/runtime/parser-1-5",
            ]
        );
    }

    #[test]
    fn failed_sibling_numbering_does_not_publish_earlier_reservations() {
        let registry = ActorLineageRegistry::default();
        let retained = ActorPath::parse("compiler/runtime/retained").unwrap();
        registry.retain_external(retained.clone());
        let long = segment(&"a".repeat(tidepool_repr::actor_path::MAX_ACTOR_PATH_SEGMENT_BYTES));
        assert!(matches!(
            registry.reserve_children(
                &ActorPath::parse("compiler").unwrap(),
                segment("runtime"),
                &[segment("valid"), long.clone(), long],
            ),
            Err(ActorPathError::SegmentTooLong { .. })
        ));
        assert_eq!(registry.state.lock().occupied, BTreeSet::from([retained]));
        let paths = registry
            .reserve_group_children(
                &ActorPath::parse("compiler/runtime").unwrap(),
                &[segment("valid")],
            )
            .unwrap();
        assert_eq!(paths[0].allocated.to_string(), "compiler/runtime/valid");
    }

    #[test]
    fn failed_collision_suffix_does_not_publish_earlier_reservations() {
        let registry = ActorLineageRegistry::default();
        let group = ActorPath::new(vec![
            segment(&"a".repeat(
                tidepool_repr::actor_path::MAX_ACTOR_PATH_SEGMENT_BYTES
            ));
            4
        ])
        .unwrap();
        let remaining =
            tidepool_repr::actor_path::MAX_ACTOR_PATH_BYTES - group.to_string().len() - 1;
        let long = segment(&"b".repeat(remaining));
        let retained = group.child(long.clone()).unwrap();
        registry.retain_external(retained.clone());
        assert!(matches!(
            registry.reserve_group_children(&group, &[segment("valid"), long]),
            Err(ActorPathError::PathTooLong { .. })
        ));
        assert_eq!(registry.state.lock().occupied, BTreeSet::from([retained]));
        let paths = registry
            .reserve_group_children(&group, &[segment("valid")])
            .unwrap();
        assert_eq!(paths[0].allocated, group.child(segment("valid")).unwrap());
    }

    #[test]
    fn unbounded_groups_exceed_32_without_erasing_a_registered_limit() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let owner = ActorRef::first(ActorId(1));
        let children = || (0..40).map(|i| segment(&format!("child-{i}"))).collect();
        for path in ["first", "second"] {
            groups
                .begin(owner, ActorPath::parse(path).unwrap(), children(), None)
                .unwrap();
        }
        let limited = ActorRef::first(ActorId(2));
        groups
            .begin(
                limited,
                ActorPath::parse("limited").unwrap(),
                vec![segment("leaf")],
                1,
            )
            .unwrap();
        assert!(matches!(
            groups.begin(
                limited,
                ActorPath::parse("extra").unwrap(),
                vec![segment("leaf")],
                None
            ),
            Err(ForkGroupError::DescendantBudgetExceeded { maximum: 1, .. })
        ));
    }

    #[test]
    fn published_group_is_not_reopened_by_late_startup_observations() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let owner = ActorRef::first(ActorId(1));
        let child = ActorRef::first(ActorId(2));
        let (group, reservations) = groups
            .begin(
                owner,
                ActorPath::parse("root/wave").unwrap(),
                vec![segment("child")],
                4,
            )
            .unwrap();
        groups
            .claim(group, owner, &reservations[0].allocated)
            .unwrap();
        groups.attach_child(group, owner, child).unwrap();
        let phase = groups.request_commit(group, owner).unwrap();
        groups.mark_ready(group, child).unwrap();
        groups.publish_groups(&[group], owner).unwrap();
        groups.mark_ready(group, child).unwrap();
        groups.mark_failed(group, child).unwrap();
        assert_eq!(*phase.borrow(), ForkGroupPhase::Committed);
        assert!(groups.abort_unpublished(owner).is_empty());
    }

    #[test]
    fn selected_abort_preserves_other_pending_and_published_groups() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let owner = ActorRef::first(ActorId(1));
        let mut ids = Vec::new();
        for (index, name) in ["failed", "pending", "published"].into_iter().enumerate() {
            let child = ActorRef::first(ActorId(index as u64 + 2));
            let (group, reservations) = groups
                .begin(
                    owner,
                    ActorPath::parse(&format!("root/{name}")).unwrap(),
                    vec![segment("child")],
                    8,
                )
                .unwrap();
            groups
                .claim(group, owner, &reservations[0].allocated)
                .unwrap();
            groups.attach_child(group, owner, child).unwrap();
            groups.request_commit(group, owner).unwrap();
            if name == "published" {
                groups.mark_ready(group, child).unwrap();
                groups.publish_groups(&[group], owner).unwrap();
            }
            ids.push(group);
        }
        assert!(groups
            .abort_incomplete_in_resident(owner, Some(&[ids[2]]))
            .is_empty());
        assert_eq!(
            groups.abort_selected_unpublished(owner, &[ids[0], ids[2]]),
            vec![ActorRef::first(ActorId(2))]
        );
        assert_eq!(
            groups.abort_unpublished(owner),
            vec![ActorRef::first(ActorId(3))]
        );
        assert!(groups.abort_unpublished(owner).is_empty());
    }

    #[test]
    fn fork_boundary_cleanup_keeps_hosted_direct_route_and_resident_siblings_isolated() {
        use tidepool_runtime::session::WorkbenchExecutionId;
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let owner = ActorRef::first(ActorId(1));
        let other_owner = ActorRef::first(ActorId(2));
        let hosted_pending = WorkbenchForkBoundary::external(
            "thread".into(),
            "request-a".into(),
            "reused-call".into(),
        );
        let hosted_ready = WorkbenchForkBoundary::external(
            "thread".into(),
            "request-b".into(),
            "reused-call".into(),
        );
        let direct_pending = WorkbenchForkBoundary::Execution {
            actor_id: owner.id.0,
            incarnation: owner.incarnation.0,
            execution_id: WorkbenchExecutionId::from_digest([1; 16]),
        };
        let direct_ready = WorkbenchForkBoundary::Execution {
            actor_id: owner.id.0,
            incarnation: owner.incarnation.0,
            execution_id: WorkbenchExecutionId::from_digest([2; 16]),
        };
        let route = WorkbenchForkBoundary::Route {
            actor_id: owner.id.0,
            incarnation: owner.incarnation.0,
            watch_id: 1,
        };
        let make_group = |name: &str,
                          owner,
                          child,
                          boundary: Option<WorkbenchForkBoundary>,
                          ready| {
            let path = ActorPath::parse(&format!("root/{name}")).unwrap();
            let (group, reservations) = match boundary {
                Some(boundary) => {
                    groups.begin_at_boundary(owner, path, vec![segment("child")], None, boundary)
                }
                None => groups.begin(owner, path, vec![segment("child")], None),
            }
            .unwrap();
            groups
                .claim(group, owner, &reservations[0].allocated)
                .unwrap();
            groups.attach_child(group, owner, child).unwrap();
            groups.request_commit(group, owner).unwrap();
            if ready {
                groups.mark_ready(group, child).unwrap();
            }
            group
        };
        let children: Vec<_> = (10..18).map(|id| ActorRef::first(ActorId(id))).collect();
        let hosted_a = make_group(
            "hosted-a",
            owner,
            children[0],
            Some(hosted_pending.clone()),
            false,
        );
        let hosted_b = make_group(
            "hosted-b",
            owner,
            children[1],
            Some(hosted_ready.clone()),
            true,
        );
        let direct_a = make_group(
            "direct-a",
            owner,
            children[2],
            Some(direct_pending.clone()),
            false,
        );
        let direct_b = make_group(
            "direct-b",
            owner,
            children[3],
            Some(direct_ready.clone()),
            true,
        );
        let route_group = make_group("route", owner, children[4], Some(route.clone()), false);
        let resident_pending = make_group("resident-pending", owner, children[5], None, false);
        let resident_ready = make_group("resident-ready", owner, children[6], None, true);
        make_group(
            "other-owner",
            other_owner,
            children[7],
            Some(hosted_pending.clone()),
            false,
        );

        groups.mark_failed(direct_a, children[2]).unwrap();

        assert!(!groups.has_ready_at_boundary(owner, &hosted_pending));
        assert!(groups.has_incomplete_at_boundary(owner, &hosted_pending));
        assert!(groups.has_ready_at_boundary(owner, &hosted_ready));
        assert!(!groups.has_incomplete_at_boundary(owner, &hosted_ready));
        assert!(!groups.has_ready_at_boundary(owner, &direct_pending));
        assert!(groups.has_incomplete_at_boundary(owner, &direct_pending));
        assert!(groups.has_ready_at_boundary(owner, &direct_ready));
        assert!(!groups.has_incomplete_at_boundary(owner, &direct_ready));
        assert!(!groups.has_ready_at_boundary(owner, &route));
        assert!(groups.has_incomplete_at_boundary(owner, &route));
        assert!(groups.has_ready_in_resident(owner));
        assert!(groups.has_incomplete_in_resident(owner));

        assert_eq!(
            groups.publish_ready_in_resident(owner).unwrap(),
            vec![resident_ready]
        );
        assert!(!groups.has_ready_in_resident(owner));
        assert!(groups.has_ready_at_boundary(owner, &hosted_ready));
        assert!(groups.has_ready_at_boundary(owner, &direct_ready));
        assert_eq!(
            groups.abort_incomplete_in_resident(
                owner,
                Some(&[resident_pending, hosted_a, direct_a, route_group])
            ),
            vec![children[5]]
        );
        assert!(!groups.has_incomplete_in_resident(owner));
        assert!(groups.has_incomplete_at_boundary(owner, &hosted_pending));
        assert!(groups.has_incomplete_at_boundary(owner, &direct_pending));
        assert!(groups.has_incomplete_at_boundary(owner, &route));

        assert_eq!(
            groups.abort_incomplete_at_boundary(owner, &hosted_pending),
            vec![children[0]]
        );
        assert!(!groups.has_incomplete_at_boundary(owner, &hosted_pending));
        assert!(groups.has_incomplete_at_boundary(other_owner, &hosted_pending));
        assert_eq!(
            groups.ready_groups_at_boundary(owner, &hosted_ready),
            vec![(hosted_b, vec![children[1]])]
        );
        assert_eq!(
            groups.abort_incomplete_at_boundary(owner, &direct_pending),
            vec![children[2]]
        );
        assert_eq!(
            groups.ready_groups_at_boundary(owner, &direct_ready),
            vec![(direct_b, vec![children[3]])]
        );
        assert_eq!(
            groups.abort_incomplete_at_boundary(owner, &route),
            vec![children[4]]
        );
        assert!(groups
            .abort_incomplete_at_boundary(owner, &hosted_pending)
            .is_empty());
        groups.publish_groups(&[hosted_b, direct_b], owner).unwrap();
        assert!(groups.abort_unpublished(owner).is_empty());
        assert_eq!(
            groups.abort_incomplete_at_boundary(other_owner, &hosted_pending),
            vec![children[7]]
        );
    }

    #[test]
    fn output_abort_discards_ready_children_only_at_exact_boundary() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let owner = ActorRef::first(ActorId(1));
        let make = |call: &str, actor: u64| {
            let boundary =
                WorkbenchForkBoundary::external("thread".into(), "turn".into(), call.into());
            let (group, reservations) = groups
                .begin_at_boundary(
                    owner,
                    ActorPath::parse(&format!("root/{call}")).unwrap(),
                    vec![segment("child")],
                    None,
                    boundary.clone(),
                )
                .unwrap();
            let child = ActorRef::first(ActorId(actor));
            groups
                .claim(group, owner, &reservations[0].allocated)
                .unwrap();
            groups.attach_child(group, owner, child).unwrap();
            groups.request_commit(group, owner).unwrap();
            groups.mark_ready(group, child).unwrap();
            (boundary, group, child)
        };
        let (first, _, first_child) = make("first", 2);
        let (second, second_group, second_child) = make("second", 3);
        assert!(groups.has_abort_work_at_boundary(owner, &first));
        assert!(groups.has_abort_work_at_boundary(owner, &second));
        assert!(!groups.has_abort_work_at_boundary(ActorRef::first(ActorId(9)), &first));
        assert_eq!(
            groups.abort_unpublished_at_boundary(owner, &first),
            vec![first_child]
        );
        assert!(groups
            .abort_unpublished_at_boundary(owner, &first)
            .is_empty());
        assert!(!groups.has_abort_work_at_boundary(owner, &first));
        assert!(groups.has_abort_work_at_boundary(owner, &second));
        assert_eq!(
            groups.ready_groups_at_boundary(owner, &second),
            vec![(second_group, vec![second_child])]
        );
    }

    #[test]
    fn completion_boundary_selects_only_its_exact_pending_groups() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let owner = ActorRef::first(ActorId(1));
        let first_boundary = tidepool_runtime::session::WorkbenchForkBoundary::external(
            "thread-a".into(),
            "call-a".into(),
            "call-a".into(),
        );
        let second_boundary = tidepool_runtime::session::WorkbenchForkBoundary::external(
            "thread-a".into(),
            "call-b".into(),
            "call-b".into(),
        );
        let unmatched = tidepool_runtime::session::WorkbenchForkBoundary::external(
            "thread-b".into(),
            "call-a".into(),
            "call-a".into(),
        );

        let make_group = |name: &str,
                          child: ActorRef,
                          boundary: tidepool_runtime::session::WorkbenchForkBoundary,
                          ready: bool| {
            let (group, reservations) = groups
                .begin_at_boundary(
                    owner,
                    ActorPath::parse(&format!("root/{name}")).unwrap(),
                    vec![segment("child")],
                    None,
                    boundary,
                )
                .unwrap();
            groups
                .claim(group, owner, &reservations[0].allocated)
                .unwrap();
            groups.attach_child(group, owner, child).unwrap();
            groups.request_commit(group, owner).unwrap();
            if ready {
                groups.mark_ready(group, child).unwrap();
            }
            group
        };
        let first_child = ActorRef::first(ActorId(2));
        let second_child = ActorRef::first(ActorId(3));
        let _first = make_group("first", first_child, first_boundary.clone(), false);
        let second = make_group("second", second_child, second_boundary.clone(), true);

        assert!(groups
            .abort_incomplete_at_boundary(owner, &unmatched)
            .is_empty());
        assert!(groups
            .ready_groups_at_boundary(owner, &unmatched)
            .is_empty());
        assert_eq!(
            groups.abort_incomplete_at_boundary(owner, &first_boundary),
            vec![first_child]
        );
        assert_eq!(
            groups.ready_groups_at_boundary(owner, &second_boundary),
            vec![(second, vec![second_child])]
        );
        assert!(matches!(
            groups.publish_groups(&[second, _first], owner),
            Err(ForkGroupError::Unknown(_))
        ));
        assert_eq!(
            groups.ready_groups_at_boundary(owner, &second_boundary),
            vec![(second, vec![second_child])],
            "batch publication failure must not partially commit ready groups"
        );
        groups.publish_groups(&[second], owner).unwrap();
        assert!(groups
            .ready_groups_at_boundary(owner, &second_boundary)
            .is_empty());
    }

    #[test]
    fn fork_group_reserves_as_one_batch_and_releases_only_on_abort() {
        let lineage = ActorLineageRegistry::default();
        let groups = ForkGroupRegistry::new(lineage.clone());
        let owner = ActorRef::first(ActorId(1));
        let (group, reservations) = groups
            .begin(
                owner,
                ActorPath::parse("compiler/runtime").unwrap(),
                vec![segment("parser"), segment("parser")],
                4,
            )
            .unwrap();
        assert_eq!(
            reservations[0].allocated.to_string(),
            "compiler/runtime/parser-1"
        );
        assert_eq!(
            reservations[1].allocated.to_string(),
            "compiler/runtime/parser-2"
        );
        groups
            .claim(group, owner, &reservations[0].allocated)
            .unwrap();
        let child = ActorRef::first(ActorId(2));
        groups.attach_child(group, owner, child).unwrap();
        assert!(matches!(
            groups.request_commit(group, owner),
            Err(ForkGroupError::Incomplete { .. })
        ));
        assert_eq!(groups.abort(group, owner).unwrap(), vec![child]);

        let (_, reused) = groups
            .begin(
                owner,
                ActorPath::parse("compiler/runtime").unwrap(),
                vec![segment("parser"), segment("parser")],
                4,
            )
            .unwrap();
        assert_eq!(reused, reservations);
    }

    #[tokio::test]
    async fn fork_group_publishes_only_after_every_child_is_queue_ready() {
        let lineage = ActorLineageRegistry::default();
        let groups = ForkGroupRegistry::new(lineage);
        let owner = ActorRef::first(ActorId(1));
        let (group, reservations) = groups
            .begin(
                owner,
                ActorPath::parse("compiler/runtime").unwrap(),
                vec![segment("parser"), segment("tests")],
                4,
            )
            .unwrap();
        let children = [ActorRef::first(ActorId(2)), ActorRef::first(ActorId(3))];
        for (reservation, child) in reservations.iter().zip(children) {
            groups.claim(group, owner, &reservation.allocated).unwrap();
            groups.attach_child(group, owner, child).unwrap();
        }
        let mut phase = groups.request_commit(group, owner).unwrap();
        assert_eq!(*phase.borrow(), ForkGroupPhase::Staging);
        groups
            .gate(group, children[0])
            .unwrap()
            .mark_ready()
            .unwrap();
        assert_eq!(*phase.borrow(), ForkGroupPhase::Staging);
        groups
            .gate(group, children[1])
            .unwrap()
            .mark_ready()
            .unwrap();
        phase.changed().await.unwrap();
        assert_eq!(*phase.borrow(), ForkGroupPhase::Ready);
        assert_eq!(
            groups.publish_ready_in_resident(owner).unwrap(),
            vec![group]
        );
        phase.changed().await.unwrap();
        assert_eq!(*phase.borrow(), ForkGroupPhase::Committed);
        assert!(matches!(
            groups.abort(group, owner),
            Err(ForkGroupError::AlreadyCommitted(_))
        ));
        assert_eq!(
            groups.cleanup_committed(group, owner).unwrap(),
            ForkGroupCleanupOutcome::Active(children.to_vec())
        );
        for child in children {
            groups.retire_actor(child);
        }
        assert_eq!(
            groups.cleanup_committed(group, owner).unwrap(),
            ForkGroupCleanupOutcome::Cleaned
        );
        assert_eq!(
            groups.cleanup_committed(group, owner).unwrap(),
            ForkGroupCleanupOutcome::Cleaned,
            "a retry observes the durable cleaned tombstone"
        );
    }

    #[test]
    fn recursive_groups_share_one_lineage_descendant_ceiling() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let root = ActorRef::first(ActorId(1));
        let children = [ActorRef::first(ActorId(2)), ActorRef::first(ActorId(3))];
        let (outer, reservations) = groups
            .begin(
                root,
                ActorPath::parse("compiler/outer").unwrap(),
                vec![segment("scaffold"), segment("review")],
                2,
            )
            .unwrap();
        for (reservation, child) in reservations.iter().zip(children) {
            groups.claim(outer, root, &reservation.allocated).unwrap();
            groups.attach_child(outer, root, child).unwrap();
        }

        assert!(matches!(
            groups.begin(
                children[0],
                ActorPath::parse("compiler/inner").unwrap(),
                vec![segment("leaf")],
                None,
            ),
            // The refusal names the coordinator whose ceiling is full — here
            // the root, not the child that asked — because the ceiling counts
            // its whole subtree and freeing the wrong slot does not help.
            Err(ForkGroupError::DescendantBudgetExceeded {
                coordinator,
                active: 2,
                requested: 1,
                maximum: 2,
            }) if coordinator == root
        ));

        groups.retire_actor(children[1]);
        assert!(groups
            .begin(
                children[0],
                ActorPath::parse("compiler/inner").unwrap(),
                vec![segment("leaf")],
                None,
            )
            .is_ok());
    }

    #[test]
    fn narrower_research_limit_counts_its_subtree_not_unrelated_actors() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let root = ActorRef::first(ActorId(1));
        let research = ActorRef::first(ActorId(2));
        let sibling = ActorRef::first(ActorId(3));
        let (outer, reservations) = groups
            .begin(
                root,
                ActorPath::parse("research/outer").unwrap(),
                vec![segment("research"), segment("sibling")],
                3,
            )
            .unwrap();
        for (reservation, child) in reservations.iter().zip([research, sibling]) {
            groups.claim(outer, root, &reservation.allocated).unwrap();
            groups.attach_child(outer, root, child).unwrap();
        }
        let (inner, _) = groups
            .begin(
                research,
                ActorPath::parse("research/inner").unwrap(),
                vec![segment("leaf")],
                1,
            )
            .unwrap();
        assert!(matches!(
            groups.begin(
                research,
                ActorPath::parse("research/extra").unwrap(),
                vec![segment("leaf")],
                1
            ),
            Err(ForkGroupError::DescendantBudgetExceeded {
                active: 1,
                maximum: 1,
                ..
            })
        ));
        // Rejected admission did not reserve another slot; abort releases the first.
        groups.abort(inner, research).unwrap();
        assert!(groups
            .begin(
                research,
                ActorPath::parse("research/retry").unwrap(),
                vec![segment("leaf")],
                1
            )
            .is_ok());
    }

    #[test]
    fn inspected_cleanup_freezes_exact_members_and_releases_admission_on_drop() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let root = ActorRef::first(ActorId(1));
        let child = ActorRef::first(ActorId(2));
        let unrelated = ActorRef::first(ActorId(3));
        let (group, reservations) = groups
            .begin(
                root,
                ActorPath::parse("campaign/wave").unwrap(),
                vec![segment("child")],
                5,
            )
            .unwrap();
        groups
            .claim(group, root, &reservations[0].allocated)
            .unwrap();
        groups.attach_child(group, root, child).unwrap();
        let _phase = groups.request_commit(group, root).unwrap();
        groups.gate(group, child).unwrap().mark_ready().unwrap();
        groups.publish_ready_in_resident(root).unwrap();
        assert!(matches!(
            groups.begin_cleanup(group, root, &[]),
            Err(ForkGroupError::CleanupScopeChanged(_))
        ));
        let guard = groups
            .begin_cleanup(group, root, &[child, unrelated])
            .unwrap();
        assert!(guard.contains(&child));
        assert!(!guard.contains(&unrelated));
        assert!(
            matches!(groups.begin(child, ActorPath::parse("campaign/next").unwrap(), vec![segment("leaf")], 5), Err(ForkGroupError::Cleaning(actor)) if actor == child)
        );
        // A forged extra plan entry cannot freeze an unrelated principal.
        groups
            .begin(
                unrelated,
                ActorPath::parse("other/wave").unwrap(),
                vec![],
                5,
            )
            .unwrap();
        drop(guard);
        groups
            .begin(
                child,
                ActorPath::parse("campaign/next").unwrap(),
                vec![segment("leaf")],
                5,
            )
            .unwrap();
        assert!(matches!(
            groups.begin_cleanup(group, root, &[child]),
            Err(ForkGroupError::CleanupAdmissionPending(_))
        ));
    }

    #[test]
    fn parent_group_cleanup_waits_for_active_grandchildren() {
        let groups = ForkGroupRegistry::new(ActorLineageRegistry::default());
        let root = ActorRef::first(ActorId(1));
        let scaffold = ActorRef::first(ActorId(2));
        let leaf = ActorRef::first(ActorId(3));

        let (outer, outer_reservations) = groups
            .begin(
                root,
                ActorPath::parse("campaign/outer").unwrap(),
                vec![segment("scaffold")],
                3,
            )
            .unwrap();
        groups
            .claim(outer, root, &outer_reservations[0].allocated)
            .unwrap();
        groups.attach_child(outer, root, scaffold).unwrap();
        let _phase = groups.request_commit(outer, root).unwrap();
        groups.gate(outer, scaffold).unwrap().mark_ready().unwrap();
        groups.publish_ready_in_resident(root).unwrap();

        let (inner, inner_reservations) = groups
            .begin(
                scaffold,
                ActorPath::parse("campaign/outer/scaffold/inner").unwrap(),
                vec![segment("leaf")],
                3,
            )
            .unwrap();
        groups
            .claim(inner, scaffold, &inner_reservations[0].allocated)
            .unwrap();
        groups.attach_child(inner, scaffold, leaf).unwrap();
        let _inner_phase = groups.request_commit(inner, scaffold).unwrap();
        groups.gate(inner, leaf).unwrap().mark_ready().unwrap();
        groups.publish_ready_in_resident(scaffold).unwrap();

        // A separate admission can have a misleadingly nested display path.
        // Exact group inspection and cleanup must still exclude it.
        let unrelated = ActorRef::first(ActorId(4));
        let (other, reservations) = groups
            .begin(
                root,
                ActorPath::parse("campaign/outer/scaffold/looks-nested").unwrap(),
                vec![segment("outsider")],
                4,
            )
            .unwrap();
        groups
            .claim(other, root, &reservations[0].allocated)
            .unwrap();
        groups.attach_child(other, root, unrelated).unwrap();
        let _other_phase = groups.request_commit(other, root).unwrap();
        groups.gate(other, unrelated).unwrap().mark_ready().unwrap();
        groups.publish_ready_in_resident(root).unwrap();
        assert!(groups.members(outer, scaffold).is_err());

        assert_eq!(
            groups.members(outer, root).unwrap(),
            vec![leaf, scaffold],
            "cleanup walks recursive descendants deepest-first"
        );
        assert_eq!(
            groups.cleanup_group_order(outer, root).unwrap(),
            vec![(inner, scaffold), (outer, root)]
        );

        groups.retire_actor(scaffold);
        assert_eq!(
            groups.cleanup_committed(outer, root).unwrap(),
            ForkGroupCleanupOutcome::Active(vec![leaf])
        );
        groups.retire_actor(leaf);
        assert_eq!(
            groups.cleanup_committed(outer, root).unwrap(),
            ForkGroupCleanupOutcome::Cleaned
        );
    }
}
