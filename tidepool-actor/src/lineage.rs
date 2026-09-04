//! Atomic allocation of readable actor paths within one retained actor tree.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_repr::{ActorPath, ActorPathError, ActorPathSegment};

use crate::ActorRef;

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

/// One allocator for actor labels and their exact `shoal/<path>` Git projection.
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
        let allocated = lowest_available(&state.occupied, prefix, leaf)?;
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
        let allocated = lowest_available(&state.occupied, &[], &requested)?;
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
        let mut frequencies = BTreeMap::<&ActorPathSegment, usize>::new();
        for child in children {
            *frequencies.entry(child).or_default() += 1;
        }
        let mut ordinals = BTreeMap::<&ActorPathSegment, usize>::new();
        let mut state = self.state.lock();
        let mut provisional = state.occupied.clone();
        let mut reservations = Vec::with_capacity(children.len());
        for child in children {
            let ordinal = ordinals.entry(child).or_default();
            *ordinal += 1;
            let requested_leaf = if frequencies[child] > 1 {
                child.numbered(*ordinal)?
            } else {
                child.clone()
            };
            let requested = group_path.child(requested_leaf.clone())?;
            let allocated = lowest_available(&provisional, group_path.segments(), &requested_leaf)?;
            provisional.insert(allocated.clone());
            reservations.push(ActorPathReservation {
                requested,
                allocated,
            });
        }
        state.occupied = provisional;
        Ok(reservations)
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
        let mut provisional = state.occupied.clone();
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
            let allocated = lowest_available(&provisional, group.segments(), &requested_leaf)?;
            provisional.insert(allocated.clone());
            reservations.push(ActorPathReservation {
                requested,
                allocated,
            });
        }
        state.occupied = provisional;
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
    #[error(
        "fork group would exceed the lineage's active descendant ceiling ({requested} requested, {active} already active or reserved, maximum {maximum})"
    )]
    DescendantBudgetExceeded {
        requested: usize,
        active: usize,
        maximum: usize,
    },
}

struct ForkGroup {
    owner: ActorRef,
    reservations: Vec<ActorPathReservation>,
    claimed: usize,
    children: Vec<ActorRef>,
    commit_requested: bool,
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
}

#[derive(Default)]
struct ForkGroupsState {
    next: u64,
    groups: HashMap<ForkGroupId, ForkGroup>,
    cleaned: HashSet<ForkGroupId>,
    parents: HashMap<ActorRef, ActorRef>,
    active: HashSet<ActorRef>,
}

/// Admission ledger for one applicative context-unfold layer.
#[derive(Clone)]
pub struct ForkGroupRegistry {
    lineage: ActorLineageRegistry,
    state: Arc<Mutex<ForkGroupsState>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkGroupCleanupOutcome {
    Cleaned,
    Active(Vec<ActorRef>),
}

impl ForkGroupRegistry {
    #[must_use]
    pub fn new(lineage: ActorLineageRegistry) -> Self {
        Self {
            lineage,
            state: Arc::new(Mutex::new(ForkGroupsState {
                next: 1,
                groups: HashMap::new(),
                cleaned: HashSet::new(),
                parents: HashMap::new(),
                active: HashSet::new(),
            })),
        }
    }

    pub fn begin(
        &self,
        owner: ActorRef,
        group: ActorPath,
        children: Vec<ActorPathSegment>,
        maximum_active_descendants: usize,
    ) -> Result<(ForkGroupId, Vec<ActorPathReservation>), ForkGroupError> {
        let mut state = self.state.lock();
        let root = lineage_root(&state.parents, owner);
        let active = reserved_descendants(&state, root);
        if active.saturating_add(children.len()) > maximum_active_descendants {
            return Err(ForkGroupError::DescendantBudgetExceeded {
                requested: children.len(),
                active,
                maximum: maximum_active_descendants,
            });
        }
        let reservations = self.lineage.reserve_group_children(&group, &children)?;
        let id = ForkGroupId(state.next);
        state.next += 1;
        state.groups.insert(
            id,
            ForkGroup {
                owner,
                reservations: reservations.clone(),
                claimed: 0,
                children: Vec::with_capacity(reservations.len()),
                commit_requested: false,
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
        group.claimed += 1;
        Ok(reservation.allocated.clone())
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
        publish_if_ready(group);
        Ok(())
    }

    fn mark_failed(&self, id: ForkGroupId, child: ActorRef) -> Result<(), ForkGroupError> {
        let state = self.state.lock();
        let group = state.groups.get(&id).ok_or(ForkGroupError::Unknown(id.0))?;
        if !group.children.contains(&child) {
            return Err(ForkGroupError::Unknown(id.0));
        }
        group.phase.send_replace(ForkGroupPhase::Aborted);
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

    pub fn publish_ready(&self, owner: ActorRef) -> Result<Vec<ForkGroupId>, ForkGroupError> {
        let mut state = self.state.lock();
        let mut published = Vec::new();
        for (id, group) in &mut state.groups {
            if group.owner == owner && *group.phase.borrow() == ForkGroupPhase::Ready {
                group.phase.send_replace(ForkGroupPhase::Committed);
                published.push(*id);
            }
        }
        published.sort_by_key(|id| id.0);
        Ok(published)
    }

    #[must_use]
    pub fn has_ready(&self, owner: ActorRef) -> bool {
        self.state
            .lock()
            .groups
            .values()
            .any(|group| group.owner == owner && *group.phase.borrow() == ForkGroupPhase::Ready)
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
        let mut state = self.state.lock();
        let ids: Vec<_> = state
            .groups
            .iter()
            .filter_map(|(id, group)| {
                (group.owner == owner && *group.phase.borrow() != ForkGroupPhase::Committed)
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
        }
        state.cleaned.insert(id);
        Ok(ForkGroupCleanupOutcome::Cleaned)
    }

    /// Exact direct and recursive members of one owned committed group,
    /// ordered deepest-first for supervisor cleanup.
    pub fn cleanup_members(
        &self,
        id: ForkGroupId,
        owner: ActorRef,
    ) -> Result<Vec<ActorRef>, ForkGroupError> {
        let state = self.state.lock();
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

fn lineage_root(parents: &HashMap<ActorRef, ActorRef>, mut actor: ActorRef) -> ActorRef {
    while let Some(parent) = parents.get(&actor).copied() {
        actor = parent;
    }
    actor
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

fn publish_if_ready(group: &mut ForkGroup) {
    if group.commit_requested && group.ready.len() == group.reservations.len() {
        group.phase.send_replace(ForkGroupPhase::Ready);
    }
}

fn lowest_available(
    occupied: &BTreeSet<ActorPath>,
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
    if !occupied.contains(&candidate) {
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
        if !occupied.contains(&candidate) {
            return Ok(candidate);
        }
    }
    unreachable!("unbounded numeric suffix space")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorId, ActorRef};

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
        assert_eq!(groups.publish_ready(owner).unwrap(), vec![group]);
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
                2,
            ),
            Err(ForkGroupError::DescendantBudgetExceeded {
                active: 2,
                requested: 1,
                maximum: 2,
            })
        ));

        groups.retire_actor(children[1]);
        assert!(groups
            .begin(
                children[0],
                ActorPath::parse("compiler/inner").unwrap(),
                vec![segment("leaf")],
                2,
            )
            .is_ok());
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
        groups.publish_ready(root).unwrap();

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
        groups.publish_ready(scaffold).unwrap();

        assert_eq!(
            groups.cleanup_members(outer, root).unwrap(),
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
