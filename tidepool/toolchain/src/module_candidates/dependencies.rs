//! Immutable dependency identities shared by cache selection and certification.
//!
//! An original may satisfy a dependency without becoming a candidate root.
//! This proof establishes product compatibility, never lexical visibility.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tidepool_repr::execution_schema::CachedHomeOwner;

use crate::execution_source::CertifiedExecutionSourceGraph;

type OwnerKey = (String, String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DependencyKind {
    Candidate,
    Original,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_source::{
        test_graph_requiring_original, test_graph_with_local_source_dependency,
    };

    #[test]
    fn fresh_dependency_can_use_an_exact_original_without_replacing_it() {
        let root = tempfile::tempdir().unwrap();
        let (graph, owners) = test_graph_with_local_source_dependency(root.path());
        let inventory = CandidateDependencyInventory::new([
            (
                owners[0].clone(),
                Some(Arc::clone(&graph)),
                DependencyKind::Candidate,
            ),
            // The parent's original recipe authenticates this fresh edge;
            // it does not require B to carry a separate original graph.
            (owners[1].clone(), None, DependencyKind::Original),
        ]);
        let direct = inventory.direct_originals(&owners[0], &graph).unwrap();
        assert_eq!(
            direct.into_values().collect::<Vec<_>>(),
            vec![owners[1].clone()]
        );
        assert_eq!(inventory.kind(&owners[1]), Ok(DependencyKind::Original));
    }

    #[test]
    fn dependency_proof_rejects_each_changed_native_identity_component() {
        let root = tempfile::tempdir().unwrap();
        let (graph, owners) = test_graph_with_local_source_dependency(root.path());
        for component in 0..3 {
            let mut other = owners[1].clone();
            match component {
                0 => other.module_version.0[0] ^= 1,
                1 => other.skinny_iface_sha256[0] ^= 1,
                _ => other.product_sha256[0] ^= 1,
            }
            let inventory = CandidateDependencyInventory::new([
                (
                    owners[0].clone(),
                    Some(Arc::clone(&graph)),
                    DependencyKind::Candidate,
                ),
                (other, Some(Arc::clone(&graph)), DependencyKind::Original),
            ]);
            assert_eq!(
                inventory.verify(&owners[0], &graph),
                Err(DependencyRefusal::DifferentOwner((
                    owners[1].unit.clone(),
                    owners[1].module.clone()
                )))
            );
        }
        let without_dependency = CandidateDependencyInventory::new([(
            owners[0].clone(),
            Some(Arc::clone(&graph)),
            DependencyKind::Candidate,
        )]);
        assert!(matches!(
            without_dependency.verify(&owners[0], &graph),
            Err(DependencyRefusal::MissingOwner(_))
        ));
    }

    #[test]
    fn retained_dependency_requires_its_exact_graph_across_receipts() {
        let root = tempfile::tempdir().unwrap();
        let (child_graph, owners) = test_graph_with_local_source_dependency(root.path());
        let parent_graph =
            test_graph_requiring_original(&child_graph, &owners[1], child_graph.digest());
        let inventory = |child| {
            CandidateDependencyInventory::new([
                (
                    owners[0].clone(),
                    Some(Arc::clone(&parent_graph)),
                    DependencyKind::Candidate,
                ),
                (owners[1].clone(), child, DependencyKind::Original),
            ])
        };
        assert!(matches!(
            inventory(None).verify(&owners[0], &parent_graph),
            Err(DependencyRefusal::MissingGraph(_))
        ));
        assert!(matches!(
            inventory(Some(Arc::clone(&parent_graph))).verify(&owners[0], &parent_graph),
            Err(DependencyRefusal::DifferentGraph(_))
        ));
        inventory(Some(child_graph))
            .verify(&owners[0], &parent_graph)
            .unwrap();
    }
}

#[derive(Clone)]
struct DependencyProduct {
    owner: CachedHomeOwner,
    graph: Option<Arc<CertifiedExecutionSourceGraph>>,
    kind: DependencyKind,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DependencyRefusal {
    MissingOwner(OwnerKey),
    AmbiguousOwner(OwnerKey),
    DifferentOwner(OwnerKey),
    MissingGraph(OwnerKey),
    DifferentGraph(OwnerKey),
    DifferentProducer,
    InvalidRoot(OwnerKey),
    ClosureBound,
}

/// A request-local view of already authenticated products. No artifact bytes
/// are copied and nothing survives the owning offer or receipt validation.
pub(crate) struct CandidateDependencyInventory {
    products: BTreeMap<OwnerKey, DependencyProduct>,
    ambiguous: BTreeSet<OwnerKey>,
}

impl CandidateDependencyInventory {
    pub(crate) fn from_candidates<'a>(
        candidates: impl IntoIterator<Item = &'a super::CandidateBundle>,
        originals: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
    ) -> Self {
        Self::new(
            candidates
                .into_iter()
                .map(|bundle| {
                    (
                        bundle.owner.clone(),
                        bundle
                            .original_execution
                            .as_ref()
                            .map(|proof| Arc::clone(&proof.graph)),
                        DependencyKind::Candidate,
                    )
                })
                .chain(originals.iter().map(|original| {
                    (
                        original.owner().clone(),
                        original.execution_source().cloned(),
                        DependencyKind::Original,
                    )
                })),
        )
    }

    pub(crate) fn new(
        products: impl IntoIterator<
            Item = (
                CachedHomeOwner,
                Option<Arc<CertifiedExecutionSourceGraph>>,
                DependencyKind,
            ),
        >,
    ) -> Self {
        let mut result = Self {
            products: BTreeMap::new(),
            ambiguous: BTreeSet::new(),
        };
        for (owner, graph, kind) in products {
            let key = (owner.unit.clone(), owner.module.clone());
            if let Some(previous) = result.products.get(&key) {
                if previous.owner != owner
                    || previous.graph.as_ref().map(|g| g.digest())
                        != graph.as_ref().map(|g| g.digest())
                {
                    result.ambiguous.insert(key);
                    continue;
                }
                // An already selected original stays the owner even if the
                // same immutable product also appeared in an advisory offer.
                if previous.kind == DependencyKind::Original {
                    continue;
                }
            }
            result
                .products
                .insert(key, DependencyProduct { owner, graph, kind });
        }
        result
    }

    pub(crate) fn kind(
        &self,
        owner: &CachedHomeOwner,
    ) -> Result<DependencyKind, DependencyRefusal> {
        Ok(self.selected(owner)?.kind)
    }

    /// Only these direct edges may be normalized from historical source
    /// selection to an original selected by the current lexical context.
    pub(crate) fn direct_originals(
        &self,
        owner: &CachedHomeOwner,
        graph: &Arc<CertifiedExecutionSourceGraph>,
    ) -> Result<BTreeMap<OwnerKey, CachedHomeOwner>, DependencyRefusal> {
        self.verify(owner, graph)?;
        let direct = graph.direct_source_owners(owner).ok_or_else(|| {
            DependencyRefusal::InvalidRoot((owner.unit.clone(), owner.module.clone()))
        })?;
        let mut originals = BTreeMap::new();
        for required in direct {
            if self.kind(required)? == DependencyKind::Original {
                originals.insert(
                    (required.unit.clone(), required.module.clone()),
                    required.clone(),
                );
            }
        }
        Ok(originals)
    }

    fn selected(&self, owner: &CachedHomeOwner) -> Result<&DependencyProduct, DependencyRefusal> {
        let key = (owner.unit.clone(), owner.module.clone());
        if self.ambiguous.contains(&key) {
            return Err(DependencyRefusal::AmbiguousOwner(key));
        }
        let selected = self
            .products
            .get(&key)
            .ok_or_else(|| DependencyRefusal::MissingOwner(key.clone()))?;
        if selected.owner != *owner {
            return Err(DependencyRefusal::DifferentOwner(key));
        }
        Ok(selected)
    }

    /// Fresh dependencies are authenticated by the parent's graph. Retained
    /// dependencies also bind their own original graph, recursively. Products
    /// need not have been emitted in the same compiler transaction.
    pub(crate) fn verify(
        &self,
        owner: &CachedHomeOwner,
        graph: &Arc<CertifiedExecutionSourceGraph>,
    ) -> Result<(), DependencyRefusal> {
        let producer = graph.producer_sha256();
        let mut pending = vec![(owner.clone(), Arc::clone(graph))];
        let mut seen = BTreeSet::new();
        while let Some((owner, graph)) = pending.pop() {
            if !seen.insert((
                owner.unit.clone(),
                owner.module.clone(),
                owner.module_version.0,
                owner.skinny_iface_sha256,
                owner.product_sha256,
                graph.digest(),
            )) {
                continue;
            }
            if seen.len() > 4096 {
                return Err(DependencyRefusal::ClosureBound);
            }
            if graph.producer_sha256() != producer {
                return Err(DependencyRefusal::DifferentProducer);
            }
            if !graph.eligible_execution_root(&owner) {
                return Err(DependencyRefusal::InvalidRoot((owner.unit, owner.module)));
            }
            for required in graph.required_source_owners(&owner) {
                self.selected(&required)?;
            }
            for (required, digest) in graph.required_original_graphs(&owner) {
                let selected = self.selected(&required)?;
                let key = (required.unit.clone(), required.module.clone());
                let child = selected
                    .graph
                    .as_ref()
                    .ok_or_else(|| DependencyRefusal::MissingGraph(key.clone()))?;
                if child.digest() != digest {
                    return Err(DependencyRefusal::DifferentGraph(key));
                }
                pending.push((required, Arc::clone(child)));
            }
        }
        Ok(())
    }
}
