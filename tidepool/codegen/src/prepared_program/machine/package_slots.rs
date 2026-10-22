use super::*;
use crate::binding_table::source_selections::WorldPackageDomainOwner;
use crate::binding_table::{WorldPackageDomainRetirement, WorldPackageSlotPermit};
use crate::prepared_program::{
    NativeArtifactBinding, NativeInstanceAttachment, NativeInstanceLease,
};
use std::sync::Weak;

/// A non-rooting identity of one actual roots-attached PACKAGE installation.
pub(super) struct WorldPackageGroupMarker {
    binding: NativeArtifactBinding,
}

#[derive(Clone)]
pub struct WorldPackageInstanceWitness {
    instance: GroupInstanceId,
    marker: Weak<WorldPackageGroupMarker>,
    domain: Weak<WorldPackageDomainOwner>,
}

impl WorldPackageInstanceWitness {
    pub fn instance(&self) -> GroupInstanceId {
        self.instance
    }
}

pub struct PreparedWorldPackageSlots {
    entries: Vec<(WorldPackageSlotPermit, GroupInstanceId)>,
    domains: HashMap<usize, PackageDomainSlots>,
    groups: HashMap<GroupInstanceId, HashSet<(usize, NativeArtifactBinding)>>,
}

pub(super) struct PackageDomainSlots {
    owner: Weak<WorldPackageDomainOwner>,
    entries: HashMap<NativeArtifactBinding, GroupInstanceId>,
}

impl PreparedMachine<'_> {
    pub(super) fn package_request_error(permit: &WorldPackageSlotPermit) -> ExecutionError {
        ExecutionError::BatchSourceContract(Box::new(
            permit.request.image.top_exports[&permit.request.binding]
                .identity
                .clone(),
        ))
    }

    fn check_world_package_group(
        &self,
        instance: GroupInstanceId,
        permit: &WorldPackageSlotPermit,
    ) -> Result<(), ExecutionError> {
        let request = &permit.request;
        let installed = self
            .programs
            .get(&instance.program())
            .ok_or(ExecutionError::UnknownProgram(instance.program()))?;
        if permit.owner.upgrade().is_none()
            || !request.is_package()
            || !installed.roots_attached
            || !std::ptr::eq(installed.program.get(), request.image.as_ref())
        {
            return Err(Self::package_request_error(permit));
        }
        let top = installed
            .program
            .get()
            .top_exports
            .get(&request.binding)
            .ok_or(ExecutionError::MissingEntry(request.binding))?;
        let expected = &request.image.top_exports[&request.binding];
        if top.identity != expected.identity || top.entry_signature != expected.entry_signature {
            return Err(Self::package_request_error(permit));
        }
        Ok(())
    }

    fn world_package_marker(
        &mut self,
        instance: GroupInstanceId,
        permit: &WorldPackageSlotPermit,
    ) -> Result<Weak<WorldPackageGroupMarker>, ExecutionError> {
        self.check_world_package_group(instance, permit)?;
        let binding = permit
            .request
            .package_key()
            .ok_or_else(|| Self::package_request_error(permit))?;
        let installed = self
            .programs
            .get_mut(&instance.program())
            .expect("checked package group");
        if let Some(marker) = &installed.package_group {
            if marker.binding != binding {
                return Err(Self::package_request_error(permit));
            }
        } else {
            installed.package_group = Some(Arc::new(WorldPackageGroupMarker { binding }));
        }
        Ok(Arc::downgrade(
            installed.package_group.as_ref().expect("package marker"),
        ))
    }

    /// Reconcile an actual selected anchor before considering lazy occupancy.
    /// Lookup issues no root and publishes no choice; native commit does both.
    pub fn lookup_world_package_slot(
        &mut self,
        permit: &WorldPackageSlotPermit,
        selected: Option<&NativeInstanceLease>,
    ) -> Result<Option<WorldPackageInstanceWitness>, ExecutionError> {
        let binding = permit
            .request
            .package_key()
            .ok_or_else(|| Self::package_request_error(permit))?;
        if permit.owner.upgrade().is_none() {
            return Err(Self::package_request_error(permit));
        }
        let occupied = self
            .package_domains
            .get(&permit.domain_key())
            .filter(|domain| domain.owner.ptr_eq(&permit.owner))
            .and_then(|domain| domain.entries.get(&binding))
            .copied();
        let instance = if let Some(anchor) = selected {
            if anchor.binding_key() != permit.request.key() {
                return Err(Self::package_request_error(permit));
            }
            self.handle_is_evaluated(anchor.handle())?;
            self.check_world_package_group(anchor.instance(), permit)?;
            if occupied.is_some_and(|other| other != anchor.instance()) {
                return Err(Self::package_request_error(permit));
            }
            anchor.instance()
        } else if let Some(instance) = occupied {
            instance
        } else {
            return Ok(None);
        };
        let marker = self.world_package_marker(instance, permit)?;
        Ok(Some(WorldPackageInstanceWitness {
            instance,
            marker,
            domain: permit.owner.clone(),
        }))
    }

    pub fn validate_world_package_witness(
        &self,
        witness: &WorldPackageInstanceWitness,
        permit: &WorldPackageSlotPermit,
    ) -> Result<(), ExecutionError> {
        self.check_world_package_group(witness.instance, permit)?;
        let installed = &self.programs[&witness.instance.program()];
        let valid = witness.domain.ptr_eq(&permit.owner)
            && installed.package_group.as_ref().is_some_and(|marker| {
                witness.marker.ptr_eq(&Arc::downgrade(marker))
                    && Some(marker.binding) == permit.request.package_key()
            });
        if !valid {
            return Err(Self::package_request_error(permit));
        }
        Ok(())
    }

    /// Reserve every mutable index slot before source custody commits. The
    /// groups may be newly staged, but no domain occupancy is visible yet.
    pub fn prepare_world_package_slot_publication(
        &mut self,
        entries: Vec<(WorldPackageSlotPermit, GroupInstanceId)>,
    ) -> Result<PreparedWorldPackageSlots, ExecutionError> {
        let mut domains = HashMap::<usize, PackageDomainSlots>::new();
        let mut groups = HashMap::<GroupInstanceId, HashSet<(usize, NativeArtifactBinding)>>::new();
        for (permit, instance) in &entries {
            self.world_package_marker(*instance, permit)?;
            let key = permit.domain_key();
            let binding = permit
                .request
                .package_key()
                .expect("checked PACKAGE request");
            let occupied = self
                .package_domains
                .get(&key)
                .and_then(|domain| domain.entries.get(&binding))
                .or_else(|| {
                    domains
                        .get(&key)
                        .and_then(|domain| domain.entries.get(&binding))
                });
            if occupied.is_some_and(|old| old != instance) {
                return Err(Self::package_request_error(permit));
            }
            domains
                .entry(key)
                .or_insert_with(|| PackageDomainSlots {
                    owner: permit.owner.clone(),
                    entries: HashMap::new(),
                })
                .entries
                .insert(binding, *instance);
            groups.entry(*instance).or_default().insert((key, binding));
        }
        self.package_domains.reserve(domains.len());
        self.package_groups.reserve(groups.len());
        for (key, additions) in &domains {
            if let Some(domain) = self.package_domains.get_mut(key) {
                domain.entries.reserve(additions.entries.len());
            }
        }
        for (group, additions) in &groups {
            if let Some(aliases) = self.package_groups.get_mut(group) {
                aliases.reserve(additions.len());
            }
        }
        Ok(PreparedWorldPackageSlots {
            entries,
            domains,
            groups,
        })
    }

    pub fn commit_world_package_slots(&mut self, prepared: PreparedWorldPackageSlots) {
        for (permit, instance) in &prepared.entries {
            assert!(
                self.check_world_package_group(*instance, permit).is_ok(),
                "exclusive package commit remains current"
            );
        }
        for (key, domain) in prepared.domains {
            if let Some(existing) = self.package_domains.get_mut(&key) {
                existing.entries.extend(domain.entries);
            } else {
                self.package_domains.insert(key, domain);
            }
        }
        for (group, aliases) in prepared.groups {
            if let Some(existing) = self.package_groups.get_mut(&group) {
                existing.extend(aliases);
            } else {
                self.package_groups.insert(group, aliases);
            }
        }
    }

    pub fn retire_world_package_domains(
        &mut self,
        retired: impl IntoIterator<Item = WorldPackageDomainRetirement>,
    ) {
        for retired in retired {
            if let Some(domain) = self.package_domains.remove(&retired.key()) {
                for (binding, group) in domain.entries {
                    if let Some(aliases) = self.package_groups.get_mut(&group) {
                        aliases.remove(&(retired.key(), binding));
                        if aliases.is_empty() {
                            self.package_groups.remove(&group);
                        }
                    }
                }
            }
        }
    }

    pub(super) fn invalidate_world_package_group(&mut self, group: GroupInstanceId) {
        if let Some(installed) = self.programs.get_mut(&group.program()) {
            installed.package_group = None;
            installed.roots_attached = false;
        }
        if let Some(aliases) = self.package_groups.remove(&group) {
            for (domain, binding) in aliases {
                if let Some(slots) = self.package_domains.get_mut(&domain) {
                    if slots.entries.get(&binding) == Some(&group) {
                        slots.entries.remove(&binding);
                    }
                    if slots.entries.is_empty() {
                        self.package_domains.remove(&domain);
                    }
                }
            }
        }
    }

    pub fn retain_world_package_export(
        &mut self,
        witness: &WorldPackageInstanceWitness,
        permit: &WorldPackageSlotPermit,
    ) -> Result<NativeInstanceAttachment, ExecutionError> {
        self.validate_world_package_witness(witness, permit)?;
        let handle = self.retain_top(witness.instance.program(), permit.request.binding)?;
        self.handles
            .retain_native_instance(handle.raw, witness.instance.program());
        let lease = permit.request.clone().into_lease(witness.instance, handle);
        Ok(NativeInstanceAttachment::from_world_package_slot(
            lease,
            permit.clone(),
            witness.clone(),
        ))
    }
}
