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
        Self::new(candidates.into_iter().map(|bundle| (
            bundle.owner.clone(),
            bundle.original_execution.as_ref().map(|proof| Arc::clone(&proof.graph)),
            DependencyKind::Candidate,
        )).chain(originals.iter().map(|original| (
            original.owner().clone(), original.execution_source().cloned(), DependencyKind::Original,
        ))))
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
            result.products.insert(key, DependencyProduct { owner, graph, kind });
        }
        result
    }

    pub(crate) fn kind(&self, owner: &CachedHomeOwner) -> Result<DependencyKind, DependencyRefusal> {
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
        let direct = graph.direct_source_owners(owner)
            .ok_or_else(|| DependencyRefusal::InvalidRoot((owner.unit.clone(), owner.module.clone())))?;
        let mut originals = BTreeMap::new();
        for required in direct {
            if self.kind(required)? == DependencyKind::Original {
                originals.insert((required.unit.clone(), required.module.clone()), required.clone());
            }
        }
        Ok(originals)
    }

    fn selected(&self, owner: &CachedHomeOwner) -> Result<&DependencyProduct, DependencyRefusal> {
        let key = (owner.unit.clone(), owner.module.clone());
        if self.ambiguous.contains(&key) {
            return Err(DependencyRefusal::AmbiguousOwner(key));
        }
        let selected = self.products.get(&key)
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
                owner.unit.clone(), owner.module.clone(), owner.module_version.0,
                owner.skinny_iface_sha256, owner.product_sha256, graph.digest(),
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
                let child = selected.graph.as_ref()
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
