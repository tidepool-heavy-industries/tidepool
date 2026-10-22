use super::*;
use crate::binding_table::source_selections::WorldNativeDomainOwner;
use crate::binding_table::{WorldNativeDomainRetirement, WorldNativeSlotPermit};
use crate::prepared_program::{NativeBindingKey, NativeInstanceAttachment, NativeInstanceLease};
use std::sync::Weak;

/// A non-rooting identity of one actual roots-attached native installation.
pub(super) struct WorldNativeGroupMarker {
    binding: NativeBindingKey,
}

#[derive(Clone)]
pub struct WorldNativeInstanceWitness {
    instance: GroupInstanceId,
    marker: Weak<WorldNativeGroupMarker>,
    domain: Weak<WorldNativeDomainOwner>,
}

impl WorldNativeInstanceWitness {
    pub fn instance(&self) -> GroupInstanceId {
        self.instance
    }
}

pub struct PreparedWorldNativeSlots {
    entries: Vec<(WorldNativeSlotPermit, GroupInstanceId)>,
    domains: HashMap<usize, NativeDomainSlots>,
    groups: HashMap<GroupInstanceId, HashSet<(usize, NativeBindingKey)>>,
}

pub(super) struct NativeDomainSlots {
    owner: Weak<WorldNativeDomainOwner>,
    entries: HashMap<NativeBindingKey, GroupInstanceId>,
}

impl PreparedMachine<'_> {
    pub(super) fn native_request_error(permit: &WorldNativeSlotPermit) -> ExecutionError {
        ExecutionError::BatchSourceContract(Box::new(
            permit.request.image.top_exports[&permit.request.binding]
                .identity
                .clone(),
        ))
    }

    fn check_world_native_group(
        &self,
        instance: GroupInstanceId,
        permit: &WorldNativeSlotPermit,
    ) -> Result<(), ExecutionError> {
        let request = &permit.request;
        let installed = self
            .programs
            .get(&instance.program())
            .ok_or(ExecutionError::UnknownProgram(instance.program()))?;
        if permit.owner.upgrade().is_none()
            || !request.matches_image_evidence(installed.program.get())
            || !installed.roots_attached
            || !std::ptr::eq(installed.program.get(), request.image.as_ref())
        {
            return Err(Self::native_request_error(permit));
        }
        let top = installed
            .program
            .get()
            .top_exports
            .get(&request.binding)
            .ok_or(ExecutionError::MissingEntry(request.binding))?;
        let expected = &request.image.top_exports[&request.binding];
        if top.identity != expected.identity || top.entry_signature != expected.entry_signature {
            return Err(Self::native_request_error(permit));
        }
        Ok(())
    }

    fn world_native_marker(
        &mut self,
        instance: GroupInstanceId,
        permit: &WorldNativeSlotPermit,
    ) -> Result<Weak<WorldNativeGroupMarker>, ExecutionError> {
        self.check_world_native_group(instance, permit)?;
        let binding = permit.request.binding_key();
        let installed = self
            .programs
            .get_mut(&instance.program())
            .expect("checked native group");
        if let Some(marker) = &installed.native_group {
            if marker.binding != binding {
                return Err(Self::native_request_error(permit));
            }
        } else {
            installed.native_group = Some(Arc::new(WorldNativeGroupMarker { binding }));
        }
        Ok(Arc::downgrade(
            installed.native_group.as_ref().expect("native marker"),
        ))
    }

    /// Reconcile an actual selected anchor before considering lazy occupancy.
    /// Lookup issues no root and publishes no choice; native commit does both.
    pub fn lookup_world_native_slot(
        &mut self,
        permit: &WorldNativeSlotPermit,
        selected: Option<&NativeInstanceLease>,
    ) -> Result<Option<WorldNativeInstanceWitness>, ExecutionError> {
        let binding = permit.request.binding_key();
        if permit.owner.upgrade().is_none() {
            return Err(Self::native_request_error(permit));
        }
        let occupied = self
            .native_domains
            .get(&permit.domain_key())
            .filter(|domain| domain.owner.ptr_eq(&permit.owner))
            .and_then(|domain| domain.entries.get(&binding))
            .copied();
        let instance = if let Some(anchor) = selected {
            if anchor.binding_key() != permit.request.binding_key() {
                return Err(Self::native_request_error(permit));
            }
            self.handle_is_evaluated(anchor.handle())?;
            self.check_world_native_group(anchor.instance(), permit)?;
            if occupied.is_some_and(|other| other != anchor.instance()) {
                return Err(Self::native_request_error(permit));
            }
            anchor.instance()
        } else if let Some(instance) = occupied {
            instance
        } else {
            return Ok(None);
        };
        let marker = self.world_native_marker(instance, permit)?;
        Ok(Some(WorldNativeInstanceWitness {
            instance,
            marker,
            domain: permit.owner.clone(),
        }))
    }

    pub fn validate_world_native_witness(
        &self,
        witness: &WorldNativeInstanceWitness,
        permit: &WorldNativeSlotPermit,
    ) -> Result<(), ExecutionError> {
        self.check_world_native_group(witness.instance, permit)?;
        let installed = &self.programs[&witness.instance.program()];
        let valid = witness.domain.ptr_eq(&permit.owner)
            && installed.native_group.as_ref().is_some_and(|marker| {
                witness.marker.ptr_eq(&Arc::downgrade(marker))
                    && marker.binding == permit.request.binding_key()
            });
        if !valid {
            return Err(Self::native_request_error(permit));
        }
        Ok(())
    }

    /// Reserve every mutable index slot before source custody commits. The
    /// groups may be newly staged, but no domain occupancy is visible yet.
    pub fn prepare_world_native_slot_publication(
        &mut self,
        entries: Vec<(WorldNativeSlotPermit, GroupInstanceId)>,
    ) -> Result<PreparedWorldNativeSlots, ExecutionError> {
        let mut domains = HashMap::<usize, NativeDomainSlots>::new();
        let mut groups = HashMap::<GroupInstanceId, HashSet<(usize, NativeBindingKey)>>::new();
        for (permit, instance) in &entries {
            self.world_native_marker(*instance, permit)?;
            let key = permit.domain_key();
            let binding = permit.request.binding_key();
            let occupied = self
                .native_domains
                .get(&key)
                .and_then(|domain| domain.entries.get(&binding))
                .or_else(|| {
                    domains
                        .get(&key)
                        .and_then(|domain| domain.entries.get(&binding))
                });
            if occupied.is_some_and(|old| old != instance) {
                return Err(Self::native_request_error(permit));
            }
            domains
                .entry(key)
                .or_insert_with(|| NativeDomainSlots {
                    owner: permit.owner.clone(),
                    entries: HashMap::new(),
                })
                .entries
                .insert(binding, *instance);
            groups.entry(*instance).or_default().insert((key, binding));
        }
        self.native_domains.reserve(domains.len());
        self.native_groups.reserve(groups.len());
        for (key, additions) in &domains {
            if let Some(domain) = self.native_domains.get_mut(key) {
                domain.entries.reserve(additions.entries.len());
            }
        }
        for (group, additions) in &groups {
            if let Some(aliases) = self.native_groups.get_mut(group) {
                aliases.reserve(additions.len());
            }
        }
        Ok(PreparedWorldNativeSlots {
            entries,
            domains,
            groups,
        })
    }

    pub fn commit_world_native_slots(&mut self, prepared: PreparedWorldNativeSlots) {
        for (permit, instance) in &prepared.entries {
            assert!(
                self.check_world_native_group(*instance, permit).is_ok(),
                "exclusive native commit remains current"
            );
        }
        for (key, domain) in prepared.domains {
            if let Some(existing) = self.native_domains.get_mut(&key) {
                existing.entries.extend(domain.entries);
            } else {
                self.native_domains.insert(key, domain);
            }
        }
        for (group, aliases) in prepared.groups {
            if let Some(existing) = self.native_groups.get_mut(&group) {
                existing.extend(aliases);
            } else {
                self.native_groups.insert(group, aliases);
            }
        }
    }

    pub fn retire_world_native_domains(
        &mut self,
        retired: impl IntoIterator<Item = WorldNativeDomainRetirement>,
    ) {
        for retired in retired {
            if let Some(domain) = self.native_domains.remove(&retired.key()) {
                for (binding, group) in domain.entries {
                    if let Some(aliases) = self.native_groups.get_mut(&group) {
                        aliases.remove(&(retired.key(), binding));
                        if aliases.is_empty() {
                            self.native_groups.remove(&group);
                        }
                    }
                }
            }
        }
    }

    pub(super) fn invalidate_world_native_group(&mut self, group: GroupInstanceId) {
        if let Some(installed) = self.programs.get_mut(&group.program()) {
            installed.native_group = None;
            installed.roots_attached = false;
        }
        if let Some(aliases) = self.native_groups.remove(&group) {
            for (domain, binding) in aliases {
                if let Some(slots) = self.native_domains.get_mut(&domain) {
                    if slots.entries.get(&binding) == Some(&group) {
                        slots.entries.remove(&binding);
                    }
                    if slots.entries.is_empty() {
                        self.native_domains.remove(&domain);
                    }
                }
            }
        }
    }

    pub fn retain_world_native_export(
        &mut self,
        witness: &WorldNativeInstanceWitness,
        permit: &WorldNativeSlotPermit,
    ) -> Result<NativeInstanceAttachment, ExecutionError> {
        self.validate_world_native_witness(witness, permit)?;
        let handle = self.retain_top(witness.instance.program(), permit.request.binding)?;
        self.handles
            .retain_native_instance(handle.raw, witness.instance.program());
        let lease = permit.request.clone().into_lease(witness.instance, handle);
        Ok(NativeInstanceAttachment::from_world_native_slot(
            lease,
            permit.clone(),
            witness.clone(),
        ))
    }

    pub fn world_native_literal_import(
        &self,
        witness: &WorldNativeInstanceWitness,
        permit: &WorldNativeSlotPermit,
    ) -> Result<BatchImport, ExecutionError> {
        self.validate_world_native_witness(witness, permit)?;
        let request = &permit.request;
        let export = &request.image.top_exports[&request.binding];
        if export.rep != tidepool_repr::execution_schema::RuntimeRep::Address
            || !request.image.byte_tops.contains_key(&request.binding)
        {
            return Err(Self::native_request_error(permit));
        }
        Ok(BatchImport::WorldNativeLiteral {
            permit: permit.clone(),
            witness: witness.clone(),
        })
    }

    pub(super) fn validate_world_native_literal_import(
        &self,
        permit: &WorldNativeSlotPermit,
        witness: &WorldNativeInstanceWitness,
        slot: &crate::prepared_program::plan::ImportSlot,
    ) -> Result<(), ExecutionError> {
        self.validate_world_native_witness(witness, permit)?;
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
            && slot.literal.is_some()
            && match &request.evidence {
                super::batch::LeaseRequestEvidence::Home { key, owner, binder } => {
                    slot.literal_source.as_ref().is_some_and(|source| {
                        &source.owner == owner
                            && source.original_ordinal == key.original_ordinal
                            && &source.binder == binder
                    })
                }
                super::batch::LeaseRequestEvidence::Package { .. } => slot.literal_source.is_none(),
            };
        if !valid {
            return Err(Self::native_request_error(permit));
        }
        BatchProgram::validate_literal_producer(&request.image, slot, request.binding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding_table::BindingTable;
    use crate::prepared_program::{
        BatchImport, BatchLeaseRequest, BatchProgram, DemandedHomeImage, DemandedPackageImage,
        ImageRegistry, NativeArtifactBinding, NativeExportRequest, NativeHomeBinding,
        PreparedCallOptions, PreparedResult,
    };
    use crate::scope::{ScopeId, ScopeTree};
    use crate::suspension::RealmId;
    use tidepool_repr::execution_schema::{
        testing, Atom, CachedHomeOwner, CertifiedGroup, ExprFrame, GlobalDecl, GlobalId, Group,
        HeapBinding, HeapRhs, ImportOwner, ModuleVersion, PackageCodeOwner, PackageGroupCode,
        ResultContract, RuntimeRep, SignatureId, TopBinding, UpdatePolicy, ValueId, ValueRef,
        WireProgram,
    };

    fn fixture() -> (Arc<DemandedPackageImage>, Arc<CompiledProgram>) {
        fixture_bytes(b"package-slot")
    }

    fn fixture_wire(bytes: &[u8]) -> WireProgram {
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
        package
    }

    fn home_owner(version: u8) -> CachedHomeOwner {
        CachedHomeOwner {
            unit: "fixture".into(),
            module: "Fixture".into(),
            module_version: ModuleVersion([version; 32]),
            skinny_iface_sha256: [2; 32],
            product_sha256: [3; 32],
        }
    }

    fn home_image(wire: WireProgram, owner: CachedHomeOwner, ordinal: u32) -> DemandedHomeImage {
        let code = CertifiedGroup::admit(
            owner,
            testing::projected_group(wire, ordinal).unwrap(),
            vec![],
        )
        .unwrap()
        .code_identity();
        DemandedHomeImage::prepare(
            code,
            &[],
            &Default::default(),
            &Default::default(),
            &ImageRegistry::new(),
        )
        .unwrap()
    }

    fn fixture_bytes(bytes: &[u8]) -> (Arc<DemandedPackageImage>, Arc<CompiledProgram>) {
        let package = fixture_wire(bytes);
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
        (package, consumer_fixture())
    }

    fn consumer_fixture() -> Arc<CompiledProgram> {
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
        consumer
    }

    fn literal_consumer(package: &DemandedPackageImage) -> Arc<CompiledProgram> {
        let mut wire = testing::wire_program();
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = testing::identity("LiteralConsumer", "entry");
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

    fn home_literal(bytes: &[u8], owner: CachedHomeOwner, ordinal: u32) -> DemandedHomeImage {
        let mut wire = testing::wire_program();
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = testing::identity("Fixture", "bytes");
        top.binding.rhs = HeapRhs::Bytes(bytes.to_vec());
        home_image(wire, owner, ordinal)
    }

    fn home_literal_consumer(source: &DemandedHomeImage) -> Arc<CompiledProgram> {
        let mut wire = testing::wire_program();
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = testing::identity("LiteralConsumer", "entry");
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
        let mut owner = home_owner(9);
        owner.module = "LiteralConsumer".into();
        let version = source.code().owner().module_version.clone();
        let code = CertifiedGroup::admit(
            owner,
            testing::projected_group(wire, 8).unwrap(),
            vec![ImportOwner::Source {
                version: version.clone(),
                binder: binder.clone(),
            }],
        )
        .unwrap()
        .code_identity();
        CompiledProgram::prepare_source_group_code(
            &code,
            &[Some(crate::prepared_program::LiteralOwner::Source {
                version,
                binder,
            })],
            &Default::default(),
            &source.image().source_literals(),
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
    ) -> WorldNativeSlotPermit {
        let domain = table
            .native_domain_selection_in(tree, scope)
            .unwrap()
            .current();
        let request =
            NativeExportRequest::for_package(package.clone(), binding, domain, ValueId(value))
                .unwrap();
        table.native_slot_permit_in(tree, scope, &request).unwrap()
    }

    enum TestNativeProduct {
        Package(Arc<DemandedPackageImage>),
        Home(DemandedHomeImage),
    }
    impl TestNativeProduct {
        fn image(&self) -> &Arc<CompiledProgram> {
            match self {
                Self::Package(image) => image.image(),
                Self::Home(image) => image.image(),
            }
        }
        fn request(
            &self,
            binding: NativeArtifactBinding,
            domain: SourceInstanceDomain,
            value: ValueId,
        ) -> NativeExportRequest {
            match self {
                Self::Package(image) => {
                    NativeExportRequest::for_package(image.clone(), binding, domain, value)
                }
                Self::Home(image) => NativeExportRequest::for_home_code(
                    image,
                    NativeHomeBinding {
                        issuer: binding,
                        original_ordinal: image.code().original_ordinal(),
                    },
                    domain,
                    value,
                ),
            }
            .unwrap()
        }
        fn lease_request(
            &self,
            group: usize,
            binding: NativeArtifactBinding,
            domain: SourceInstanceDomain,
            value: ValueId,
        ) -> BatchLeaseRequest {
            match self {
                Self::Package(image) => {
                    BatchLeaseRequest::for_package(group, image.clone(), binding, domain, value)
                }
                Self::Home(image) => BatchLeaseRequest::for_home_code(
                    group,
                    image,
                    NativeHomeBinding {
                        issuer: binding,
                        original_ordinal: image.code().original_ordinal(),
                    },
                    domain,
                    value,
                ),
            }
            .unwrap()
        }
    }
    fn native_permit(
        table: &mut BindingTable,
        tree: &ScopeTree,
        scope: ScopeId,
        product: &TestNativeProduct,
        binding: NativeArtifactBinding,
        value: u32,
    ) -> WorldNativeSlotPermit {
        let domain = table
            .native_domain_selection_in(tree, scope)
            .unwrap()
            .current();
        table
            .native_slot_permit_in(
                tree,
                scope,
                &product.request(binding, domain, ValueId(value)),
            )
            .unwrap()
    }

    fn release_scope(
        table: &mut BindingTable,
        machine: &mut PreparedMachine<'_>,
        tree: &mut ScopeTree,
        scope: ScopeId,
    ) {
        for scope in tree.retire(scope) {
            let drained = table.drain_scope_with_sources(scope);
            machine.retire_world_native_domains(table.take_native_domain_retirements());
            assert!(drained.bindings.is_empty());
            for lease in drained.source_instances {
                assert!(machine.release(lease.handle()));
            }
        }
    }

    #[test]
    fn empty_world_parent_retains_package_choice_through_child_abort_and_live_closure() {
        let (package, consumer) = fixture();
        native_caf_history(TestNativeProduct::Package(package), consumer);
    }

    #[test]
    fn home_caf_choice_survives_child_retirement_without_crossing_fork_domains() {
        let image = home_image(fixture_wire(b"home-caf"), home_owner(1), 3);
        native_caf_history(TestNativeProduct::Home(image), consumer_fixture());
    }

    fn native_caf_history(product: TestNativeProduct, consumer: Arc<CompiledProgram>) {
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
        let first = native_permit(&mut table, &tree, a, &product, key(1), 0);
        assert!(machine
            .lookup_world_native_slot(&first, None)
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
                        image: product.image().clone(),
                        imports: vec![],
                    },
                ],
                vec![product.lease_request(1, key(1), first.domain(), ValueId(0))],
            )
            .unwrap();
        let group = receipt.leases[0].instance();
        let staged = machine
            .prepare_world_native_slot_publication(vec![(first.clone(), group)])
            .unwrap();
        let admission = table
            .register_source_instances_in_domains(&tree, a, receipt.source_attachments, &machine)
            .ok()
            .unwrap();
        machine.commit_world_native_slots(staged);
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
            panic!("real native CAF returns its own closure")
        };
        let closure = *closure;
        assert!(machine
            .handle_is_evaluated(receipt.leases[0].handle())
            .unwrap());
        for lease in table.rollback_source_admission(a, &admission).unwrap() {
            assert!(machine.release(lease.handle()));
        }
        release_scope(&mut table, &mut machine, &mut tree, a);
        assert!(!table.validate_native_slot_permit(&tree, a, &first));
        machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert!(
            machine.programs.contains_key(&group.program()),
            "retained closure keeps actual CAF group live without table source roots"
        );
        let b = tree.mint_isolated();
        table
            .seed_world_execution_scope(&tree, parent, b, &empty)
            .unwrap();
        let sibling = native_permit(&mut table, &tree, b, &product, key(1), 1);
        let witness = machine
            .lookup_world_native_slot(&sibling, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            witness.instance(),
            group,
            "implicit live parent preserves domain identity"
        );
        let other = machine
            .install_shared_batch_with_leases(
                vec![BatchProgram {
                    image: product.image().clone(),
                    imports: vec![],
                }],
                vec![product.lease_request(0, key(1), sibling.domain(), ValueId(1))],
            )
            .unwrap();
        assert!(
            machine
                .lookup_world_native_slot(&sibling, Some(&other.leases[0]))
                .is_err(),
            "actual different physical instance cannot replace canonical same-domain choice"
        );
        assert!(machine.release(other.leases[0].handle()));
        let attachment = machine
            .retain_world_native_export(&witness, &sibling)
            .unwrap();
        assert_eq!(
            machine.handle_current_pointer(closure),
            machine.handle_current_pointer(attachment.lease().handle())
        );
        let staged = machine
            .prepare_world_native_slot_publication(vec![(sibling.clone(), group)])
            .unwrap();
        table
            .register_source_instances_in_domains(&tree, b, vec![attachment], &machine)
            .ok()
            .unwrap();
        machine.commit_world_native_slots(staged);
        let caf = native_permit(&mut table, &tree, b, &product, key(1), 0);
        let caf_witness = machine
            .lookup_world_native_slot(&caf, None)
            .unwrap()
            .unwrap();
        let caf_root = machine
            .retain_world_native_export(&caf_witness, &caf)
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
        let fork_permit = native_permit(&mut table, &tree, fork, &product, key(1), 0);
        let selected = table
            .native_domain_selection_in(&tree, fork)
            .unwrap()
            .roots()
            .values()
            .next()
            .unwrap()
            .clone();
        let fork_witness = machine
            .lookup_world_native_slot(&fork_permit, Some(&selected))
            .unwrap()
            .unwrap();
        assert_eq!(
            fork_witness.instance(),
            group,
            "forked domain first honors inherited physical anchor"
        );
        let fork_publication = machine
            .prepare_world_native_slot_publication(vec![(fork_permit.clone(), group)])
            .unwrap();
        let fork_root = machine
            .retain_world_native_export(&fork_witness, &fork_permit)
            .unwrap();
        table
            .register_source_instances_in_domains(&tree, fork, vec![fork_root], &machine)
            .ok()
            .unwrap();
        machine.commit_world_native_slots(fork_publication);
        let independent = tree.mint_isolated();
        let independent_permit = native_permit(&mut table, &tree, independent, &product, key(1), 0);
        assert!(
            machine
                .lookup_world_native_slot(&independent_permit, None)
                .unwrap()
                .is_none(),
            "independent vacant domain does not share another CAF"
        );
        let fresh = machine
            .install_shared_batch_with_leases(
                vec![BatchProgram {
                    image: product.image().clone(),
                    imports: vec![],
                }],
                vec![product.lease_request(0, key(1), independent_permit.domain(), ValueId(0))],
            )
            .unwrap();
        assert_ne!(fresh.leases[0].instance(), group);
        assert!(
            !machine
                .handle_is_evaluated(fresh.leases[0].handle())
                .unwrap(),
            "independent domain receives its own unevaluated memoizing CAF"
        );
        let fresh_slots = machine
            .prepare_world_native_slot_publication(vec![(
                independent_permit.clone(),
                fresh.leases[0].instance(),
            )])
            .unwrap();
        table
            .register_source_instances_in_domains(
                &tree,
                independent,
                fresh.source_attachments,
                &machine,
            )
            .ok()
            .unwrap();
        machine.commit_world_native_slots(fresh_slots);
        let different = native_permit(&mut table, &tree, b, &product, key(3), 0);
        assert!(
            machine
                .lookup_world_native_slot(&different, None)
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
            machine.lookup_world_native_slot(&first, None).is_err(),
            "retained snapshot or stale permit cannot revive dead parent domain"
        );
        release_scope(&mut table, &mut machine, &mut tree, independent);
        assert_eq!(
            machine.native_domains.len(),
            1,
            "only the actual fork's alias remains"
        );
        release_scope(&mut table, &mut machine, &mut tree, fork);
        assert!(
            machine.native_domains.is_empty(),
            "dead fork metadata disappears despite surviving closure"
        );
        assert!(machine.programs.contains_key(&group.program()));
        assert!(machine.release(closure));
        machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(machine.residency().programs, 0);
    }

    #[test]
    fn native_slot_commit_refuses_failed_admission_conflicts_and_detached_group() {
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
            .prepare_world_native_slot_publication(vec![(address.clone(), group)])
            .unwrap();
        drop(failed);
        assert!(
            machine
                .lookup_world_native_slot(&address, None)
                .unwrap()
                .is_none(),
            "cancelled precommit publishes no occupancy"
        );
        let staged = machine
            .prepare_world_native_slot_publication(vec![(address.clone(), group)])
            .unwrap();
        table
            .register_source_instances_in_domains(&tree, scope, vec![], &machine)
            .ok()
            .unwrap();
        machine.commit_world_native_slots(staged);
        let literal_witness = machine
            .lookup_world_native_slot(&address, None)
            .unwrap()
            .unwrap();
        let literal_image = literal_consumer(&package);
        let closed = vec![BatchProgram {
            image: literal_image.clone(),
            imports: vec![machine
                .world_native_literal_import(&literal_witness, &address)
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
                        .world_native_literal_import(&literal_witness, &address)
                        .unwrap()]
                }])
                .is_err(),
            "same identity with different literal bytes refuses before installation"
        );
        assert_eq!(machine.residency().programs, before);
        let value = permit(&mut table, &tree, scope, &package, key(1), 1);
        let witness = machine
            .lookup_world_native_slot(&value, None)
            .unwrap()
            .unwrap();
        assert!(
            machine
                .world_native_literal_import(&witness, &value)
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
                .lookup_world_native_slot(&value, Some(&other.leases[0]))
                .is_err(),
            "independently selected conflicting group is never overwritten"
        );
        assert_eq!(
            machine
                .lookup_world_native_slot(&value, None)
                .unwrap()
                .unwrap()
                .instance(),
            group
        );
        let root = machine
            .retain_world_native_export(&witness, &value)
            .unwrap();
        let root_handle = root.lease().handle();
        let foreign_scope = tree.mint_isolated();
        let rejected = table
            .register_source_instances_in_domains(&tree, foreign_scope, vec![root], &machine)
            .err()
            .unwrap();
        assert_eq!(
            machine
                .lookup_world_native_slot(&value, None)
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
            .validate_world_native_witness(&witness, &value)
            .is_err());
        assert!(machine
            .retain_world_native_export(&witness, &value)
            .is_err());
        assert!(machine
            .world_native_literal_import(&literal_witness, &address)
            .is_err());
        assert!(machine.native_domains.is_empty());
        drop(held_marker);
        machine.unpin(group.program());
        release_scope(&mut table, &mut machine, &mut tree, scope);
        release_scope(&mut table, &mut machine, &mut tree, foreign_scope);
        machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(machine.residency().programs, 0);
    }

    #[test]
    fn home_literal_reuse_checks_original_producer_owner_ordinal_and_bytes() {
        let image = home_literal(b"home-literal", home_owner(1), 3);
        let mut machine = PreparedMachine::empty(PreparedMachineOptions {
            nursery_bytes: 4096,
        })
        .unwrap();
        let mut tree = ScopeTree::new();
        let scope = tree.mint_isolated();
        let mut table = BindingTable::new();
        let domain = table
            .native_domain_selection_in(&tree, scope)
            .unwrap()
            .current();
        let binding = NativeHomeBinding {
            issuer: key(1),
            original_ordinal: 3,
        };
        let request =
            NativeExportRequest::for_home_code(&image, binding, domain, ValueId(0)).unwrap();
        let permit = table.native_slot_permit_in(&tree, scope, &request).unwrap();
        let group = GroupInstanceId::from(
            machine
                .install_shared_batch(vec![BatchProgram {
                    image: image.image().clone(),
                    imports: vec![],
                }])
                .unwrap()[0],
        );
        machine.pin(group.program()).unwrap();
        let staged = machine
            .prepare_world_native_slot_publication(vec![(permit.clone(), group)])
            .unwrap();
        table
            .register_source_instances_in_domains(&tree, scope, vec![], &machine)
            .ok()
            .unwrap();
        machine.commit_world_native_slots(staged);
        let witness = machine
            .lookup_world_native_slot(&permit, None)
            .unwrap()
            .unwrap();
        let consumer = home_literal_consumer(&image);
        let installed = machine
            .install_shared_batch(vec![BatchProgram {
                image: consumer.clone(),
                imports: vec![machine
                    .world_native_literal_import(&witness, &permit)
                    .unwrap()],
            }])
            .unwrap()[0];
        let slot = &consumer.import_slots[0];
        assert!(slot.literal_source.is_some());
        assert_eq!(
            machine.programs[&installed].roots.read(slot.slot).unwrap(),
            slot.literal.as_ref().unwrap().as_ptr() as u64
        );
        let before = machine.residency().programs;
        for other in [
            home_literal(b"home-literal", home_owner(2), 3),
            home_literal(b"home-literal", home_owner(1), 4),
            home_literal(b"changed", home_owner(1), 3),
        ] {
            assert!(machine.install_shared_batch(vec![BatchProgram { image:home_literal_consumer(&other), imports:vec![machine.world_native_literal_import(&witness,&permit).unwrap()] }]).is_err(),"equal bytes cannot replace original HOME producer owner or ordinal; equal owner cannot replace bytes");
            assert_eq!(machine.residency().programs, before);
            let other_request = NativeExportRequest::for_home_code(
                &other,
                NativeHomeBinding {
                    issuer: key(1),
                    original_ordinal: other.code().original_ordinal(),
                },
                domain,
                ValueId(0),
            )
            .unwrap();
            let other_permit = table
                .native_slot_permit_in(&tree, scope, &other_request)
                .unwrap();
            assert!(
                machine
                    .lookup_world_native_slot(&other_permit, None)
                    .is_err(),
                "occupied exact binding never accepts another image"
            );
        }
        assert!(NativeExportRequest::for_home_code(
            &image,
            NativeHomeBinding {
                issuer: key(1),
                original_ordinal: 4
            },
            domain,
            ValueId(0)
        )
        .is_err());
        let held = witness.marker.upgrade().unwrap();
        machine.detach_roots(group.program()).unwrap();
        assert!(machine
            .world_native_literal_import(&witness, &permit)
            .is_err());
        drop(held);
        machine.unpin(group.program());
        release_scope(&mut table, &mut machine, &mut tree, scope);
        machine.collect_major(machine.quiesce().unwrap()).unwrap();
        assert_eq!(machine.residency().programs, 0);
    }
}
