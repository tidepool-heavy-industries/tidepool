use super::*;
use crate::prepared_program::{NativeExportRequest, SourceInstanceDomain};

/// Live lexical ownership of one source domain. Immutable snapshots carry only
/// the domain's semantic identity, never this lifetime.
#[derive(Debug)]
pub(crate) struct WorldPackageDomainOwner;

/// Authority for exactly one checked PACKAGE export in an actual live scope.
/// It retains neither the scope, a native root, nor a source-domain lifetime.
#[derive(Clone)]
pub struct WorldPackageSlotPermit {
    pub(crate) owner: Weak<WorldPackageDomainOwner>,
    table: Weak<()>,
    scope: ScopeId,
    pub(crate) request: NativeExportRequest,
}

impl WorldPackageSlotPermit {
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
pub struct WorldPackageDomainRetirement {
    pub(crate) owner: Weak<WorldPackageDomainOwner>,
}

impl WorldPackageDomainRetirement {
    pub(crate) fn key(&self) -> usize {
        self.owner.as_ptr() as usize
    }
}

struct DomainMembership {
    owner: Arc<WorldPackageDomainOwner>,
    live_views: usize,
}

#[derive(Default)]
pub(super) struct SourceSelections {
    views: HashMap<ScopeId, SourceSelectionView>,
    implicit: HashSet<ScopeId>,
    domains: HashMap<SourceInstanceDomain, DomainMembership>,
    retired: Vec<WorldPackageDomainRetirement>,
}

pub(super) struct PreparedSourceSelectionReplacement {
    scope: ScopeId,
    view: Option<SourceSelectionView>,
    added: Vec<(SourceInstanceDomain, PreparedDomainOwner)>,
    removed: Vec<SourceInstanceDomain>,
}

enum PreparedDomainOwner {
    Existing(Weak<WorldPackageDomainOwner>),
    Fresh(Arc<WorldPackageDomainOwner>),
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
                        PreparedDomainOwner::Fresh(Arc::new(WorldPackageDomainOwner))
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
                self.retired.push(WorldPackageDomainRetirement {
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
    pub fn package_slot_permit_in(
        &mut self,
        tree: &ScopeTree,
        scope: ScopeId,
        request: &NativeExportRequest,
    ) -> Result<WorldPackageSlotPermit, BindingPromotionError> {
        if !tree.is_live(scope) || !request.is_package() {
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
        Ok(WorldPackageSlotPermit {
            owner: Arc::downgrade(owner),
            table: Arc::downgrade(&self.declaration_preparation_owner),
            scope,
            request: request.clone(),
        })
    }

    pub fn validate_package_slot_permit(
        &self,
        tree: &ScopeTree,
        scope: ScopeId,
        permit: &WorldPackageSlotPermit,
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

    pub fn take_package_domain_retirements(&mut self) -> Vec<WorldPackageDomainRetirement> {
        std::mem::take(&mut self.source_selection.retired)
    }

    pub fn retained_world_package_attachment(
        &self,
        permit: &WorldPackageSlotPermit,
        witness: &crate::prepared_program::WorldPackageInstanceWitness,
    ) -> Option<crate::prepared_program::NativeInstanceAttachment> {
        let token = &self
            .source_instances
            .get(&SourceLeaseKey {
                instance: witness.instance(),
                value: permit.request.value(),
            })?
            .token;
        if token.binding_key()
            != crate::prepared_program::NativeBindingKey::Package(permit.request.package_key()?)
        {
            return None;
        }
        Some(
            crate::prepared_program::NativeInstanceAttachment::from_world_package_slot(
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
}
