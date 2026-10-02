// Native transaction fixtures use the same sealed-export seam as the existing
// package tests. Real GHC package-interface authentication is a separate gate.

use tidepool_repr::execution_schema::ModuleVersion;

fn package_original_owner(digest: [u8; 32]) -> ImportOwner {
    let binder = producer_identity();
    ImportOwner::Package {
        unit: binder.unit.clone(),
        module: binder.module.clone(),
        binder,
        interface_digest: digest,
    }
}

fn package_original_group(digest: [u8; 32]) -> CertifiedGroup {
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    wire.globals.push(GlobalDecl {
        identity: producer_identity(),
        rep: RuntimeRep::LiftedRef,
        entry_signature: Some(SignatureId(0)),
        required_evaluated: false,
        required_generation: None,
    });
    wire.expressions.nodes[0] = ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
    let Group::NonRecursive(top) = &mut wire.bindings[0] else {
        unreachable!()
    };
    top.identity.unit = HOME_UNIT.into();
    top.identity.module = "ImmutableOriginal".into();
    top.identity.occurrence = "cached".into();
    top.binding.rhs = HeapRhs::Thunk {
        signature: SignatureId(0),
        update: UpdatePolicy::Memoize,
        captures: vec![],
        body: 0,
    };
    CertifiedGroup::admit(
        CachedHomeOwner {
            unit: HOME_UNIT.into(),
            module: "ImmutableOriginal".into(),
            module_version: ModuleVersion([1; 32]),
            skinny_iface_sha256: [2; 32],
            product_sha256: [3; 32],
        },
        testing::projected_group(wire, 0).unwrap(),
        vec![package_original_owner(digest)],
    )
    .unwrap()
}

fn package_original_machine(registry: &ImageRegistry) -> (PreparedEngine, ProgramId) {
    let target = CertifiedTargetImage::compile(producer_program(), registry).unwrap();
    let mut engine = PreparedEngine::empty_certified(64 * 1024, None).unwrap();
    let staged = engine
        .install_certified_turn_admitted(
            target,
            &[],
            &BTreeMap::new(),
            vec![],
            &[],
            &BTreeMap::new(),
            &HashMap::new(),
            &BindingTable::new(),
            BTreeMap::new(),
            BTreeMap::from([(producer_identity(), [9; 32])]),
        )
        .unwrap();
    let program = engine.commit_certified_turn(staged);
    (engine, program)
}

#[test]
fn immutable_package_only_original_image_keeps_fresh_machine_cafs_independent() {
    let registry = ImageRegistry::new();
    let (mut a, _) = package_original_machine(&registry);
    let (mut b, package_b) = package_original_machine(&registry);
    let group = package_original_group([9; 32]);
    assert!(group
        .imports()
        .iter()
        .all(|owner| matches!(owner, ImportOwner::Package { .. })));
    assert!(group
        .definitions()
        .globals()
        .iter()
        .all(|global| global.required_generation.is_none()));
    let demand_a = DemandedImage::compile(group.clone(), &registry).unwrap();
    let demand_b = DemandedImage::compile(group, &registry).unwrap();
    assert!(Arc::ptr_eq(demand_a.image(), demand_b.image()));
    let shared = Arc::clone(demand_a.image().definition_facts());
    assert_eq!((registry.misses(), registry.hits()), (2, 2));
    let install = |engine: &mut PreparedEngine, demanded| {
        let external = HashMap::from([(
            package_original_owner([9; 32]),
            engine.code_exports[&producer_identity()].handle,
        )]);
        engine
            .install_certified_demand(vec![demanded], &external, &BindingTable::new())
            .unwrap()[0]
    };
    let original_a = install(&mut a, demand_a);
    let original_b = install(&mut b, demand_b);
    for (engine, original) in [(&a, original_a), (&b, original_b)] {
        assert!(Arc::ptr_eq(
            &engine.programs[&original].definitions,
            &shared
        ));
    }
    let caf_a = a.machine.retain_top(original_a, ValueId(0)).unwrap();
    let caf_b = b.machine.retain_top(original_b, ValueId(0)).unwrap();
    assert_ne!(
        a.machine.handle_root(caf_a).unwrap().addr(),
        b.machine.handle_root(caf_b).unwrap().addr()
    );
    assert!(!a.machine.handle_is_evaluated(caf_a).unwrap());
    assert!(!b.machine.handle_is_evaluated(caf_b).unwrap());
    let force = |engine: &mut PreparedEngine, original| {
        let result = engine
            .machine
            .run_entry_retained(
                original,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .unwrap();
        let [PreparedResult::Managed(value)] = result.values.as_slice() else {
            panic!("original CAF must return its package constructor");
        };
        let CodegenPreparedOuter::Constructor { identity, fields } =
            engine.machine.inspect_outer(*value, RealmId::ROOT).unwrap();
        assert_eq!(identity, tidepool_repr::DataConId(980));
        assert!(matches!(fields.as_slice(), [PreparedResult::Scalar(99)]));
        assert!(engine.machine.release(*value));
    };
    force(&mut a, original_a);
    assert!(a.machine.handle_is_evaluated(caf_a).unwrap());
    assert!(
        !b.machine.handle_is_evaluated(caf_b).unwrap(),
        "a's shared image must not update b's CAF"
    );
    force(&mut b, original_b);
    assert!(b.machine.handle_is_evaluated(caf_b).unwrap());
    assert!(a.machine.release(caf_a));
    assert!(b.machine.release(caf_b));
    drop(a);
    force(&mut b, original_b);
    assert!(b.unpin(original_b));
    assert!(b.unpin(package_b));
    drop(b);
    assert_eq!((registry.misses(), registry.hits()), (2, 2));
}

#[test]
fn immutable_package_only_original_refuses_missing_or_changed_proof_atomically() {
    let registry = ImageRegistry::new();
    let (mut engine, _) = package_original_machine(&registry);
    let package = producer_identity();
    let handle = engine.code_exports[&package].handle;
    let before = engine.residency();
    let roots = engine.persistent_roots_count();
    let programs = engine.programs.len();
    for (digest, supplied, installed_proof) in [
        ([9; 32], false, Some([9; 32])),
        ([9; 32], true, None),
        ([9; 32], true, Some([8; 32])),
        ([8; 32], true, Some([9; 32])),
        ([0; 32], true, Some([9; 32])),
    ] {
        engine
            .code_exports
            .get_mut(&package)
            .unwrap()
            .interface_digest = installed_proof;
        let owner = package_original_owner(digest);
        let external = if supplied {
            HashMap::from([(owner.clone(), handle)])
        } else {
            HashMap::new()
        };
        let selected = DemandedImage::compile(package_original_group(digest), &registry).unwrap();
        let error = engine
            .install_certified_demand(vec![selected], &external, &BindingTable::new())
            .unwrap_err();
        assert!(
            matches!(error, PreparedRuntimeError::MissingCertifiedOwner(ref rejected) if rejected == &owner)
        );
        assert_eq!(engine.residency(), before);
        assert_eq!(engine.persistent_roots_count(), roots);
        assert_eq!(engine.programs.len(), programs);
        assert_eq!(
            engine.code_exports[&package].interface_digest,
            installed_proof
        );
        assert!(!engine.machine.handle_is_evaluated(handle).unwrap());
    }
    engine
        .code_exports
        .get_mut(&package)
        .unwrap()
        .interface_digest = Some([9; 32]);
    let selected = DemandedImage::compile(package_original_group([9; 32]), &registry).unwrap();
    let installed = engine
        .install_certified_demand(
            vec![selected],
            &HashMap::from([(package_original_owner([9; 32]), handle)]),
            &BindingTable::new(),
        )
        .unwrap();
    assert_eq!(
        installed.len(),
        1,
        "the same machine remains usable after all refusals"
    );
}
