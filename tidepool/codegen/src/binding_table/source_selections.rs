use super::*;
use crate::prepared_program::{NativeExportRequest, SourceInstanceDomain};

/// Live lexical ownership of one source domain. Immutable snapshots carry only
/// the domain's semantic identity, never this lifetime.
#[derive(Debug)]
pub(crate) struct WorldNativeDomainOwner;

/// Authority for exactly one checked native export in an actual live scope.
/// It retains neither the scope, a native root, nor a source-domain lifetime.
#[derive(Clone)]
pub struct WorldNativeSlotPermit {
    pub(crate) owner: Weak<WorldNativeDomainOwner>,
    table: Weak<()>,
    scope: ScopeId,
    pub(crate) request: NativeExportRequest,
}

impl WorldNativeSlotPermit {
    pub fn request(&self) -> &NativeExportRequest {
        &self.request
    }
    pub fn domain(&self) -> SourceInstanceDomain {
        self.request.domain()
    }

    pub(crate) fn domain_key(&self) -> usize {
        self.owner.as_ptr() as usize
    }
}

/// Metadata retirement only. Native roots retain their ordinary machine and
/// scope custody; retiring a domain never releases a heap handle.
pub struct WorldNativeDomainRetirement {
    pub(crate) owner: Weak<WorldNativeDomainOwner>,
}

impl WorldNativeDomainRetirement {
    pub(crate) fn key(&self) -> usize {
        self.owner.as_ptr() as usize
    }
}

struct DomainMembership {
    owner: Arc<WorldNativeDomainOwner>,
    live_views: usize,
}

#[derive(Default)]
pub(super) struct SourceSelections {
    views: HashMap<ScopeId, SourceSelectionView>,
    implicit: HashSet<ScopeId>,
    domains: HashMap<SourceInstanceDomain, DomainMembership>,
    retired: Vec<WorldNativeDomainRetirement>,
}

pub(super) struct PreparedSourceSelectionReplacement {
    scope: ScopeId,
    view: Option<SourceSelectionView>,
    added: Vec<(SourceInstanceDomain, PreparedDomainOwner)>,
    removed: Vec<SourceInstanceDomain>,
}

enum PreparedDomainOwner {
    Existing(Weak<WorldNativeDomainOwner>),
    Fresh(Arc<WorldNativeDomainOwner>),
}

impl PreparedSourceSelectionReplacement {
    pub(super) fn set_view(&mut self, view: SourceSelectionView) {
        assert!(self.view.as_ref().is_some_and(|old| old
            .domains
            .keys()
            .all(|domain| view.domains.contains_key(domain))
            && old.domains.size() == view.domains.size()));
        self.view = Some(view);
    }
}

impl std::ops::Deref for SourceSelections {
    type Target = HashMap<ScopeId, SourceSelectionView>;
    fn deref(&self) -> &Self::Target {
        &self.views
    }
}

impl SourceSelections {
    pub(super) fn reserve(&mut self, additional: usize) {
        self.views.reserve(additional);
    }

    pub(super) fn prepare_replace(
        &mut self,
        scope: ScopeId,
        view: Option<SourceSelectionView>,
    ) -> PreparedSourceSelectionReplacement {
        let old = self.views.get(&scope);
        let mut added = Vec::new();
        let mut removed = Vec::new();
        if let Some(view) = &view {
            for domain in view.domains.keys() {
                if old.is_some_and(|old| old.domains.contains_key(domain)) {
                    continue;
                }
                let owner = self
                    .domains
                    .get(domain)
                    .map(|record| {
                        record
                            .live_views
                            .checked_add(1)
                            .expect("source domain membership exhausted");
                        PreparedDomainOwner::Existing(Arc::downgrade(&record.owner))
                    })
                    .unwrap_or_else(|| {
                        PreparedDomainOwner::Fresh(Arc::new(WorldNativeDomainOwner))
                    });
                added.push((*domain, owner));
            }
        }
        if let Some(old) = old {
            for domain in old.domains.keys() {
                if !view
                    .as_ref()
                    .is_some_and(|view| view.domains.contains_key(domain))
                {
                    removed.push(*domain);
                }
            }
        }
        self.views.reserve(usize::from(view.is_some()));
        self.domains.reserve(added.len());
        self.retired.reserve(removed.len());
        PreparedSourceSelectionReplacement {
            scope,
            view,
            added,
            removed,
        }
    }

    pub(super) fn commit(&mut self, prepared: PreparedSourceSelectionReplacement) {
        self.implicit.remove(&prepared.scope);
        for (domain, owner) in prepared.added {
            let owner = match owner {
                PreparedDomainOwner::Existing(owner) => owner
                    .upgrade()
                    .expect("exclusive prepared domain remains live"),
                PreparedDomainOwner::Fresh(owner) => owner,
            };
            self.domains
                .entry(domain)
                .or_insert(DomainMembership {
                    owner,
                    live_views: 0,
                })
                .live_views += 1;
        }
        for domain in prepared.removed {
            let record = self
                .domains
                .get_mut(&domain)
                .expect("live source domain membership");
            record.live_views -= 1;
            if record.live_views == 0 {
                let record = self
                    .domains
                    .remove(&domain)
                    .expect("retiring source domain");
                self.retired.push(WorldNativeDomainRetirement {
                    owner: Arc::downgrade(&record.owner),
                });
            }
        }
        match prepared.view {
            Some(view) => {
                self.views.insert(prepared.scope, view);
            }
            None => {
                self.views.remove(&prepared.scope);
            }
        }
    }

    pub(super) fn replace(&mut self, scope: ScopeId, view: SourceSelectionView) {
        let prepared = self.prepare_replace(scope, Some(view));
        self.commit(prepared);
    }

    pub(super) fn remove(&mut self, scope: &ScopeId) -> Option<SourceSelectionView> {
        let old = self.views.get(scope).cloned();
        let prepared = self.prepare_replace(*scope, None);
        self.commit(prepared);
        old
    }

    pub(super) fn materialize_scope(&mut self, scope: ScopeId) -> &SourceSelectionView {
        if !self.views.contains_key(&scope) {
            self.implicit.reserve(1);
            self.replace(scope, SourceSelectionView::for_scope(scope));
            self.implicit.insert(scope);
        }
        self.views
            .get(&scope)
            .expect("materialized source selection")
    }

    pub(super) fn select(
        &mut self,
        scope: ScopeId,
        domain: SourceInstanceDomain,
        key: SourceLeaseKey,
    ) {
        self.materialize_scope(scope);
        self.implicit.remove(&scope);
        self.views
            .get_mut(&scope)
            .expect("materialized source selection")
            .add(domain, key);
    }

    pub(super) fn contains_key(&self, scope: &ScopeId) -> bool {
        self.views.contains_key(scope) && !self.implicit.contains(scope)
    }

    pub(super) fn remove_selected(
        &mut self,
        scope: ScopeId,
        domain: SourceInstanceDomain,
        key: &SourceLeaseKey,
    ) {
        let view = self
            .views
            .get_mut(&scope)
            .expect("selected source scope exists");
        let mut record = view
            .domains
            .get(&domain)
            .cloned()
            .expect("selected source domain exists");
        record.selected.remove_mut(key);
        view.domains.insert_mut(domain, record);
    }

    pub(super) fn remove_selected_from_all_domains(
        &mut self,
        scope: ScopeId,
        key: &SourceLeaseKey,
    ) {
        let domains = self
            .views
            .get(&scope)
            .map(|view| view.domains.keys().copied().collect::<Vec<_>>())
            .unwrap_or_default();
        for domain in domains {
            self.remove_selected(scope, domain, key);
        }
    }
}

impl BindingTable {
    pub fn native_slot_permit_in(
        &mut self,
        tree: &ScopeTree,
        scope: ScopeId,
        request: &NativeExportRequest,
    ) -> Result<WorldNativeSlotPermit, BindingPromotionError> {
        if !tree.is_live(scope) {
            return Err(BindingPromotionError::UnverifiableSourceSelection);
        }
        let view = self.source_selection.materialize_scope(scope);
        if !view.domains.contains_key(&request.domain()) {
            return Err(BindingPromotionError::MissingSourceDomain);
        }
        let owner = &self
            .source_selection
            .domains
            .get(&request.domain())
            .expect("live view owns every source domain")
            .owner;
        Ok(WorldNativeSlotPermit {
            owner: Arc::downgrade(owner),
            table: Arc::downgrade(&self.declaration_preparation_owner),
            scope,
            request: request.clone(),
        })
    }

    pub fn validate_native_slot_permit(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
        permit: &WorldNativeSlotPermit,
    ) -> bool {
        tree.is_live(scope)
            && permit.scope == scope
            && permit
                .table
                .ptr_eq(&Arc::downgrade(&self.declaration_preparation_owner))
            && self
                .source_selection
                .get(&scope)
                .is_some_and(|view| view.domains.contains_key(&permit.domain()))
            && self
                .source_selection
                .domains
                .get(&permit.domain())
                .is_some_and(|record| permit.owner.ptr_eq(&Arc::downgrade(&record.owner)))
    }

    pub fn take_native_domain_retirements(&mut self) -> Vec<WorldNativeDomainRetirement> {
        std::mem::take(&mut self.source_selection.retired)
    }

    pub fn retained_world_native_attachment(
        &self,
        permit: &WorldNativeSlotPermit,
        witness: &crate::prepared_program::WorldNativeInstanceWitness,
    ) -> Option<crate::prepared_program::NativeInstanceAttachment> {
        let token = &self
            .source_instances
            .get(&SourceLeaseKey {
                instance: witness.instance(),
                value: permit.request.value(),
            })?
            .token;
        if token.binding_key() != permit.request.binding_key() {
            return None;
        }
        Some(
            crate::prepared_program::NativeInstanceAttachment::from_world_native_slot(
                token.clone(),
                permit.clone(),
                witness.clone(),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn prepared_snapshot_does_not_retain_retired_live_domain() {
        let mut selections = SourceSelections::default();
        let parent = ScopeId(1);
        let child = ScopeId(2);
        let view = SourceSelectionView::for_scope(parent);
        selections.replace(parent, view.clone());
        let original = Arc::downgrade(&selections.domains[&view.current].owner);
        let uncommitted = selections.prepare_replace(child, Some(view.clone()));
        selections.remove(&parent);
        assert!(original.upgrade().is_none(), "neither semantic snapshots nor uncommitted existing-domain preparation keep a live domain owner");
        drop(uncommitted);
        selections.replace(parent, view.clone());
        let replacement = Arc::downgrade(&selections.domains[&view.current].owner);
        assert!(
            !original.ptr_eq(&replacement),
            "same semantic domain coordinates cannot resurrect retired identity"
        );
        assert_eq!(selections.get(&parent), Some(&view));
    }

    proptest! {
        #[test]
        fn live_domain_memberships_match_recomputed_scope_views(
            actions in prop::collection::vec((1u64..6, any::<bool>(), prop::collection::btree_set(0u64..7, 1..5)), 1..100)
        ) {
            let mut selections = SourceSelections::default();
            let mut model = HashMap::<ScopeId, std::collections::BTreeSet<SourceInstanceDomain>>::new();
            let mut snapshots = Vec::new();
            for (scope, remove, domains) in actions {
                let scope = ScopeId(scope);
                let before = selections.domains.iter().map(|(domain, record)| (*domain, Arc::downgrade(&record.owner))).collect::<HashMap<_, _>>();
                if remove {
                    selections.remove(&scope);
                    model.remove(&scope);
                } else {
                    let domains = domains.into_iter().map(|slot| SourceInstanceDomain::in_view(BindingTipId(1), slot)).collect::<std::collections::BTreeSet<_>>();
                    let view = SourceSelectionView { current: *domains.first().unwrap(), domains: domains.iter().map(|domain| (*domain, SourceDomainRecord::empty())).collect() };
                    snapshots.push(view.clone());
                    let prepared = selections.prepare_replace(scope, Some(view));
                    selections.commit(prepared);
                    model.insert(scope, domains);
                }
                let mut recomputed = HashMap::<SourceInstanceDomain, usize>::new();
                for domains in model.values() { for domain in domains { *recomputed.entry(*domain).or_default() += 1; } }
                prop_assert_eq!(selections.domains.len(), recomputed.len());
                for (domain, count) in &recomputed {
                    prop_assert_eq!(selections.domains[domain].live_views, *count);
                    if let Some(old) = before.get(domain) {
                        prop_assert!(old.ptr_eq(&Arc::downgrade(&selections.domains[domain].owner)));
                    }
                }
                let retired = selections.retired.drain(..).collect::<Vec<_>>();
                let removed = before.iter().filter(|(domain, _)| !recomputed.contains_key(*domain)).collect::<Vec<_>>();
                prop_assert_eq!(retired.len(), removed.len());
                for (_, old) in removed {
                    prop_assert!(old.upgrade().is_none(), "retained semantic snapshots cannot retain retired owners");
                    prop_assert!(retired.iter().any(|retired| retired.owner.ptr_eq(old)));
                }
                std::hint::black_box(&snapshots);
            }
        }
    }
}
