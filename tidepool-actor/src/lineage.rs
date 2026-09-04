//! Atomic allocation of readable actor paths within one retained actor tree.

use std::collections::{BTreeMap, BTreeSet};
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

    pub fn retain_external(&self, path: ActorPath) -> bool {
        self.state.lock().occupied.insert(path)
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
}
