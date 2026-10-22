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

    pub fn world_package_literal_import(
        &self,
        witness: &WorldPackageInstanceWitness,
        permit: &WorldPackageSlotPermit,
    ) -> Result<BatchImport, ExecutionError> {
        self.validate_world_package_witness(witness, permit)?;
        let request = &permit.request;
        let export = &request.image.top_exports[&request.binding];
        if export.rep != tidepool_repr::execution_schema::RuntimeRep::Address
            || !request.image.byte_tops.contains_key(&request.binding)
        {
            return Err(Self::package_request_error(permit));
        }
        Ok(BatchImport::WorldPackageLiteral {
            permit: permit.clone(),
            witness: witness.clone(),
        })
    }

    pub(super) fn validate_world_package_literal_import(
        &self,
        permit: &WorldPackageSlotPermit,
        witness: &WorldPackageInstanceWitness,
        slot: &crate::prepared_program::plan::ImportSlot,
    ) -> Result<(), ExecutionError> {
        self.validate_world_package_witness(witness, permit)?;
        let request = &permit.request;
        let export = &request.image.top_exports[&request.binding];
        let valid = slot.rep == tidepool_repr::execution_schema::RuntimeRep::Address
            && export.rep == slot.rep
            && slot.identity == export.identity
            && slot
                .entry_signature
                .as_ref()
                .is_none_or(|expected| export.entry_signature.as_ref() == Some(expected))
            && (!slot.required_evaluated || export.evaluated)
            && slot.literal_source.is_none()
            && slot.literal.as_ref().is_some_and(|literal| {
                request
                    .image
                    .byte_tops
                    .get(&request.binding)
                    .map(Arc::as_ref)
                    == Some(literal.as_ref())
            });
        if !valid {
            return Err(Self::package_request_error(permit));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding_table::BindingTable;
    use crate::prepared_program::{
        BatchImport, BatchLeaseRequest, BatchProgram, DemandedPackageImage, ImageRegistry,
        NativeExportRequest, PreparedCallOptions, PreparedResult,
    };
    use crate::scope::{ScopeId, ScopeTree};
    use crate::suspension::RealmId;
    use tidepool_repr::execution_schema::{
        testing, Atom, ExprFrame, GlobalDecl, GlobalId, Group, HeapBinding, HeapRhs,
        PackageCodeOwner, PackageGroupCode, ResultContract, RuntimeRep, SignatureId, TopBinding,
        UpdatePolicy, ValueId, ValueRef,
    };

    fn fixture() -> (Arc<DemandedPackageImage>, Arc<CompiledProgram>) {
        fixture_bytes(b"package-slot")
    }

    fn fixture_bytes(bytes: &[u8]) -> (Arc<DemandedPackageImage>, Arc<CompiledProgram>) {
        let mut package = testing::wire_program();
        package.signatures.push(package.signatures[0].clone());
        package.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        package
            .expressions
            .nodes
            .push(package.expressions.nodes[0].clone());
        package.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Local(ValueId(1)))]);
        let Group::NonRecursive(top) = &mut package.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        package.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Fixture", "result"),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Function {
                    signature: SignatureId(1),
                    parameters: vec![],
                    captures: vec![],
                    body: 1,
                },
            },
        }));
        package.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Fixture", "bytes"),
            binding: HeapBinding {
                id: ValueId(2),
                rhs: HeapRhs::Bytes(bytes.to_vec()),
            },
        }));
        let code = PackageGroupCode::admit(
            PackageCodeOwner {
                unit: "fixture".into(),
                module: "Fixture".into(),
                compiler_sha256: [1; 32],
                profile_sha256: [2; 32],
                interface_sha256: [3; 32],
                defining_version_sha256: [4; 32],
            },
            testing::projected_group(package, 3).unwrap(),
        )
        .unwrap();
        let package = Arc::new(
            DemandedPackageImage::prepare(
                code,
                &[],
                &Default::default(),
                &Default::default(),
                &ImageRegistry::new(),
            )
            .unwrap(),
        );
        let mut consumer = testing::wire_program();
        consumer.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        consumer.signatures.push(consumer.signatures[0].clone());
        consumer.globals.push(GlobalDecl {
            identity: testing::identity("Fixture", "entry"),
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(1)),
            required_evaluated: false,
            required_generation: None,
        });
        consumer.expressions.nodes[0] = ExprFrame::Enter {
            callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
            signature: SignatureId(1),
        };
        let consumer = Arc::new(
            CompiledProgram::compile_prepared_definitions(&testing::prepare(consumer).unwrap())
                .unwrap(),
        );
        (package, consumer)
    }

    fn literal_consumer(package: &DemandedPackageImage) -> Arc<CompiledProgram> {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
        let binder = testing::identity("Fixture", "bytes");
        wire.globals.push(GlobalDecl {
            identity: binder.clone(),
            rep: RuntimeRep::Address,
            entry_signature: None,
            required_evaluated: true,
            required_generation: None,
        });
        let code = PackageGroupCode::admit(
            PackageCodeOwner {
                unit: "fixture".into(),
                module: "LiteralConsumer".into(),
                compiler_sha256: [1; 32],
                profile_sha256: [2; 32],
                interface_sha256: [5; 32],
                defining_version_sha256: [6; 32],
            },
            testing::projected_group(wire, 8).unwrap(),
        )
        .unwrap();
        CompiledProgram::prepare_package_group_code(
            &code,
            &[Some(crate::prepared_program::LiteralOwner::Package {
                unit: binder.unit.clone(),
                module: binder.module.clone(),
                binder,
                interface_digest: [3; 32],
            })],
            &package.image().package_literals(|_, _| Some([3; 32])),
            &Default::default(),
            &ImageRegistry::new(),
        )
        .unwrap()
    }

    fn key(byte: u8) -> NativeArtifactBinding {
        NativeArtifactBinding {
            artifact: [byte; 32],
            selection: [2; 32],
        }
    }

    fn permit(
        table: &mut BindingTable,
        tree: &ScopeTree,
        scope: ScopeId,
        package: &Arc<DemandedPackageImage>,
        binding: NativeArtifactBinding,
        value: u32,
    ) -> WorldPackageSlotPermit {
        let domain = table
            .native_domain_selection_in(tree, scope)
            .unwrap()
            .current();
        let request =
            NativeExportRequest::for_package(package.clone(), binding, domain, ValueId(value))
                .unwrap();
        table.package_slot_permit_in(tree, scope, &request).unwrap()
    }

    fn release_scope(
        table: &mut BindingTable,
        machine: &mut PreparedMachine<'_>,
        tree: &mut ScopeTree,
        scope: ScopeId,
    ) {
        for scope in tree.retire(scope) {
            let drained = table.drain_scope_with_sources(scope);
            machine.retire_world_package_domains(table.take_package_domain_retirements());
            assert!(drained.bindings.is_empty());
            for lease in drained.source_instances {
                assert!(machine.release(lease.handle()));
            }
        }
    }

    #[test]
    fn empty_world_parent_retains_package_choice_through_child_abort_and_live_closure() {
        let (package, consumer) = fixture();
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let mut tree = ScopeTree::new();
        let parent = tree.mint_isolated();
        let a = tree.mint_isolated();
        let mut table = BindingTable::new();
        let empty = table.scope_snapshot(&tree, parent).unwrap();
        table
            .seed_world_execution_scope(&tree, parent, a, &empty)
            .unwrap();
        assert_eq!(
            table.scope_snapshot(&tree, parent).unwrap(),
            empty,
            "materializing the implicit parent's lifetime changes no semantic snapshot"
        );
        let first = permit(&mut table, &tree, a, &package, key(1), 0);
        assert!(machine
            .lookup_world_package_slot(&first, None)
            .unwrap()
            .is_none());
        let receipt = machine
            .install_shared_batch_with_leases(
                vec![
                    BatchProgram {
                        image: consumer,
                        imports: vec![BatchImport::Source {
                            group: 1,
                            binding: ValueId(0),
                        }],
                    },
                    BatchProgram {
                        image: package.image().clone(),
                        imports: vec![],
                    },
                ],
                vec![BatchLeaseRequest::for_package(
                    1,
                    package.clone(),
                    key(1),
                    first.domain(),
                    ValueId(0),
                )
                .unwrap()],
            )
            .unwrap();
        let group = receipt.leases[0].instance();
        let staged = machine
            .prepare_world_package_slot_publication(vec![(first.clone(), group)])
            .unwrap();
        let admission = table
            .register_source_instances_in_domains(&tree, a, receipt.source_attachments, &machine)
            .ok()
            .unwrap();
        machine.commit_world_package_slots(staged);
        let result = machine
            .run_entry_retained(
                receipt.programs[0],
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .unwrap();
        let [PreparedResult::Managed(closure)] = result.values.as_slice() else {
            panic!("real PACKAGE CAF returns its own closure")
        };
        let closure = *closure;
        assert!(machine
            .handle_is_evaluated(receipt.leases[0].handle())
            .unwrap());
        for lease in table.rollback_source_admission(a, &admission).unwrap() {
            assert!(machine.release(lease.handle()));
        }
        release_scope(&mut table, &mut machine, &mut tree, a);
        assert!(!table.validate_package_slot_permit(&tree, a, &first));
        machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert!(
            machine.programs.contains_key(&group.program()),
            "retained closure keeps actual CAF group live without table source roots"
        );
        let b = tree.mint_isolated();
        table
            .seed_world_execution_scope(&tree, parent, b, &empty)
            .unwrap();
        let sibling = permit(&mut table, &tree, b, &package, key(1), 1);
        let witness = machine
            .lookup_world_package_slot(&sibling, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            witness.instance(),
            group,
            "implicit live parent preserves domain identity"
        );
        let attachment = machine
            .retain_world_package_export(&witness, &sibling)
            .unwrap();
        assert_eq!(
            machine.handle_current_pointer(closure),
            machine.handle_current_pointer(attachment.lease().handle())
        );
        let staged = machine
            .prepare_world_package_slot_publication(vec![(sibling.clone(), group)])
            .unwrap();
        table
            .register_source_instances_in_domains(&tree, b, vec![attachment], &machine)
            .ok()
            .unwrap();
        machine.commit_world_package_slots(staged);
        let caf = permit(&mut table, &tree, b, &package, key(1), 0);
        let caf_witness = machine
            .lookup_world_package_slot(&caf, None)
            .unwrap()
            .unwrap();
        let caf_root = machine
            .retain_world_package_export(&caf_witness, &caf)
            .unwrap();
        assert!(
            machine
                .handle_is_evaluated(caf_root.lease().handle())
                .unwrap(),
            "sibling reuse preserves prior memoizing CAF update"
        );
        assert!(machine.release(caf_root.lease().handle()));
        let fork = tree.mint_isolated();
        table.seed_scope(&tree, b, fork);
        let fork_permit = permit(&mut table, &tree, fork, &package, key(1), 0);
        let selected = table
            .native_domain_selection_in(&tree, fork)
            .unwrap()
            .roots()
            .values()
            .next()
            .unwrap()
            .clone();
        let fork_witness = machine
            .lookup_world_package_slot(&fork_permit, Some(&selected))
            .unwrap()
            .unwrap();
        assert_eq!(
            fork_witness.instance(),
            group,
            "forked domain first honors inherited physical anchor"
        );
        let fork_publication = machine
            .prepare_world_package_slot_publication(vec![(fork_permit.clone(), group)])
            .unwrap();
        let fork_root = machine
            .retain_world_package_export(&fork_witness, &fork_permit)
            .unwrap();
        table
            .register_source_instances_in_domains(&tree, fork, vec![fork_root], &machine)
            .ok()
            .unwrap();
        machine.commit_world_package_slots(fork_publication);
        let independent = tree.mint_isolated();
        let independent_permit = permit(&mut table, &tree, independent, &package, key(1), 0);
        assert!(
            machine
                .lookup_world_package_slot(&independent_permit, None)
                .unwrap()
                .is_none(),
            "independent vacant domain does not share another CAF"
        );
        let different = permit(&mut table, &tree, b, &package, key(3), 0);
        assert!(
            machine
                .lookup_world_package_slot(&different, None)
                .unwrap()
                .is_none(),
            "exact binding selection remains distinct"
        );
        release_scope(&mut table, &mut machine, &mut tree, parent);
        let stale_child = tree.mint_isolated();
        assert!(
            table
                .seed_world_execution_scope(&tree, parent, stale_child, &empty)
                .is_err(),
            "an immutable snapshot cannot recreate a retired parent domain"
        );
        release_scope(&mut table, &mut machine, &mut tree, b);
        assert!(
            machine.lookup_world_package_slot(&first, None).is_err(),
            "retained snapshot or stale permit cannot revive dead parent domain"
        );
        assert_eq!(
            machine.package_domains.len(),
            1,
            "only the actual fork's alias remains"
        );
        release_scope(&mut table, &mut machine, &mut tree, fork);
        release_scope(&mut table, &mut machine, &mut tree, independent);
        assert!(
            machine.package_domains.is_empty(),
            "dead fork metadata disappears despite surviving closure"
        );
        assert!(machine.programs.contains_key(&group.program()));
        assert!(machine.release(closure));
        machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(machine.residency().programs, 0);
    }

    #[test]
    fn package_slot_commit_refuses_failed_admission_conflicts_and_detached_group() {
        let (package, _) = fixture();
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let mut tree = ScopeTree::new();
        let scope = tree.mint_isolated();
        let mut table = BindingTable::new();
        let address = permit(&mut table, &tree, scope, &package, key(1), 2);
        let groups = machine
            .install_shared_batch(vec![BatchProgram {
                image: package.image().clone(),
                imports: vec![],
            }])
            .unwrap();
        let group = GroupInstanceId::from(groups[0]);
        machine.pin(group.program()).unwrap();
        let failed = machine
            .prepare_world_package_slot_publication(vec![(address.clone(), group)])
            .unwrap();
        drop(failed);
        assert!(
            machine
                .lookup_world_package_slot(&address, None)
                .unwrap()
                .is_none(),
            "cancelled precommit publishes no occupancy"
        );
        let staged = machine
            .prepare_world_package_slot_publication(vec![(address.clone(), group)])
            .unwrap();
        table
            .register_source_instances_in_domains(&tree, scope, vec![], &machine)
            .ok()
            .unwrap();
        machine.commit_world_package_slots(staged);
        let literal_witness = machine
            .lookup_world_package_slot(&address, None)
            .unwrap()
            .unwrap();
        let literal_image = literal_consumer(&package);
        let closed = vec![BatchProgram {
            image: literal_image.clone(),
            imports: vec![machine
                .world_package_literal_import(&literal_witness, &address)
                .unwrap()],
        }];
        assert!(
            BatchProgram::validate_closed(&closed).is_err(),
            "closed recipes still reject physical World imports"
        );
        let installed_literal = machine.install_shared_batch(closed).unwrap()[0];
        let slot = &literal_image.import_slots[0];
        assert_eq!(
            machine.programs[&installed_literal]
                .roots
                .read(slot.slot)
                .unwrap(),
            slot.literal.as_ref().unwrap().as_ptr() as u64,
            "literal imports use the importing image's authenticated bytes"
        );
        let (wrong_bytes, _) = fixture_bytes(b"different-package-slot");
        let before = machine.residency().programs;
        assert!(
            machine
                .install_shared_batch(vec![BatchProgram {
                    image: literal_consumer(&wrong_bytes),
                    imports: vec![machine
                        .world_package_literal_import(&literal_witness, &address)
                        .unwrap()]
                }])
                .is_err(),
            "same identity with different literal bytes refuses before installation"
        );
        assert_eq!(machine.residency().programs, before);
        let value = permit(&mut table, &tree, scope, &package, key(1), 1);
        let witness = machine
            .lookup_world_package_slot(&value, None)
            .unwrap()
            .unwrap();
        assert!(
            machine
                .world_package_literal_import(&witness, &value)
                .is_err(),
            "managed export cannot stand in for PACKAGE literal"
        );
        assert_eq!(
            witness.instance(),
            group,
            "address-only first demand still owns one physical PACKAGE choice"
        );
        let other = machine
            .install_shared_batch_with_leases(
                vec![BatchProgram {
                    image: package.image().clone(),
                    imports: vec![],
                }],
                vec![BatchLeaseRequest::for_package(
                    0,
                    package.clone(),
                    key(1),
                    value.domain(),
                    ValueId(1),
                )
                .unwrap()],
            )
            .unwrap();
        assert!(
            machine
                .lookup_world_package_slot(&value, Some(&other.leases[0]))
                .is_err(),
            "independently selected conflicting group is never overwritten"
        );
        assert_eq!(
            machine
                .lookup_world_package_slot(&value, None)
                .unwrap()
                .unwrap()
                .instance(),
            group
        );
        let root = machine
            .retain_world_package_export(&witness, &value)
            .unwrap();
        let root_handle = root.lease().handle();
        let foreign_scope = tree.mint_isolated();
        let rejected = table
            .register_source_instances_in_domains(&tree, foreign_scope, vec![root], &machine)
            .err()
            .unwrap();
        assert_eq!(
            machine
                .lookup_world_package_slot(&value, None)
                .unwrap()
                .unwrap()
                .instance(),
            group,
            "failed source admission leaves existing occupancy unchanged"
        );
        for root in rejected {
            assert!(machine.release(root.lease().handle()));
        }
        assert!(machine.handle_is_evaluated(root_handle).is_err());
        assert!(machine.release(other.leases[0].handle()));
        let held_marker = witness.marker.upgrade().unwrap();
        machine.detach_roots(group.program()).unwrap();
        assert!(
            witness.marker.upgrade().is_some(),
            "retained marker alone is not liveness authority"
        );
        assert!(
            machine.programs.contains_key(&group.program()),
            "metadata may remain while block roots are detached"
        );
        assert!(machine
            .validate_world_package_witness(&witness, &value)
            .is_err());
        assert!(machine
            .retain_world_package_export(&witness, &value)
            .is_err());
        assert!(machine
            .world_package_literal_import(&literal_witness, &address)
            .is_err());
        assert!(machine.package_domains.is_empty());
        drop(held_marker);
        machine.unpin(group.program());
        release_scope(&mut table, &mut machine, &mut tree, scope);
        release_scope(&mut table, &mut machine, &mut tree, foreign_scope);
        machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(machine.residency().programs, 0);
    }
}
