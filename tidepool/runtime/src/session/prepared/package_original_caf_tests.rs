use tidepool_repr::execution_schema::ModuleVersion;

fn compile_package_original_fixture() -> crate::RuntimePreparedTargets {
    tidepool_testing::eval_harness::require_extract();
    tidepool_testing::with_settlement(|settlement| {
        crate::compile_runtime_targets(
            &tidepool_toolchain::artifacts::CompileInvocation {
                source: include_str!("../../../tests/fixtures/PackageOriginalCaf.hs"),
                targets: &["result"],
                include: &[tidepool_testing::eval_harness::prelude_path()],
                fallback_module_name: "PackageOriginalCaf",
            },
            |_, _, _| {},
            settlement,
        )
    })
    .expect("compile the package export through its actual compiler owner")
}

fn package_original_owner(binder: &SymbolIdentity, digest: [u8; 32]) -> ImportOwner {
    ImportOwner::Package {
        unit: binder.unit.clone(),
        module: binder.module.clone(),
        binder: binder.clone(),
        interface_digest: digest,
    }
}

fn package_original_group(
    owner: ImportOwner,
    rep: RuntimeRep,
    entry: Option<Signature>,
) -> CertifiedGroup {
    let ImportOwner::Package { binder, .. } = &owner else {
        panic!("the fixture forwards a package export")
    };
    let mut wire = testing::wire_program();
    wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
    let entry_signature = entry.map(|entry| {
        let id = SignatureId(wire.signatures.len() as u32);
        wire.signatures.push(entry);
        id
    });
    wire.globals.push(GlobalDecl {
        identity: binder.clone(),
        rep,
        entry_signature,
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
        vec![owner],
    )
    .unwrap()
}

fn package_original_target(group: &CertifiedGroup, registry: &ImageRegistry) -> ScopedTargetImage {
    let (binder, _) = group.exported_values().into_iter().next().unwrap();
    let mut wire = testing::wire_program();
    let Group::NonRecursive(entry) = &mut wire.bindings[0] else {
        unreachable!()
    };
    entry.identity.unit = HOME_UNIT.into();
    wire.globals.push(GlobalDecl {
        identity: binder.clone(),
        rep: RuntimeRep::LiftedRef,
        entry_signature: None,
        required_evaluated: false,
        required_generation: None,
    });
    scoped_single_domain_target(
        testing::prepare(wire).unwrap(),
        registry,
        vec![ImportOwner::Source {
            version: group.owner().module_version.clone(),
            binder: binder.clone(),
        }],
    )
}

fn package_original_machine(
    compiled: &crate::RuntimePreparedTargets,
    registry: Arc<ImageRegistry>,
) -> (
    super::super::PersistentSession,
    ProgramId,
    Arc<super::super::RuntimeLexicalScopeLease>,
) {
    let mut session = super::super::PersistentSession::new(None, 1024 * 1024);
    session.set_image_registry(registry);
    let (program, placement) = compiled.install_in(&mut session, "result").unwrap();
    (session, program, placement)
}

#[test]
fn immutable_package_only_original_image_keeps_fresh_machine_cafs_independent() {
    let compiled = compile_package_original_fixture();
    let registry = Arc::new(ImageRegistry::new());
    let (mut session_a, package_a, _placement_a) =
        package_original_machine(&compiled, registry.clone());
    let (mut session_b, package_b, _placement_b) =
        package_original_machine(&compiled, registry.clone());
    let a = session_a.require_prepared().unwrap();
    let b = session_b.require_prepared().unwrap();
    let (binder, package) = a
        .code_exports
        .iter()
        .filter(|(_, export)| {
            export.program == package_a
                && export.interface_digest.is_some()
                && export.entry.is_some()
        })
        .min_by_key(|(binder, _)| (*binder).clone())
        .expect("the actual compiled target must export a certified package callable");
    let binder = binder.clone();
    let digest = package.interface_digest.unwrap();
    assert_ne!(digest, [0; 32]);
    let signature = package.entry.clone();
    let rep = package.handle.rep();
    assert_eq!(rep, RuntimeRep::LiftedRef);
    let package_handle_a = package.handle;
    let package_handle_b = b.code_exports[&binder].handle;
    assert_eq!(b.code_exports[&binder].interface_digest, Some(digest));
    assert_eq!(b.code_exports[&binder].entry, signature);
    let group = package_original_group(package_original_owner(&binder, digest), rep, signature);
    assert!(group
        .imports()
        .iter()
        .all(|owner| matches!(owner, ImportOwner::Package { .. })));
    assert!(group
        .definitions()
        .globals()
        .iter()
        .all(|global| global.required_generation.is_none()));
    let misses = registry.misses();
    let hits = registry.hits();
    let demand_a = DemandedImage::compile(group.clone(), &registry).unwrap();
    let demand_b = DemandedImage::compile(group.clone(), &registry).unwrap();
    assert!(Arc::ptr_eq(demand_a.image(), demand_b.image()));
    let shared = Arc::clone(demand_a.image().definition_facts());
    let mut scopes = tidepool_codegen::scope::ScopeTree::new();
    let scope_a = scopes.mint_isolated();
    let scope_b = scopes.mint_isolated();
    let mut bindings_a = BindingTable::new();
    let mut bindings_b = BindingTable::new();
    let install = |engine: &mut PreparedEngine, demanded, bindings: &mut BindingTable, scope| {
        let mut staged = engine
            .install_certified_turn(
                ScopedTargetImage {
                    demanded: vec![demanded],
                    ..package_original_target(&group, &registry)
                },
                &certified_source_evidence(std::slice::from_ref(&group)),
                &[],
                &BTreeMap::new(),
                &HashMap::new(),
                bindings,
                &HashMap::new(),
            )
            .unwrap();
        let original = staged.groups[0];
        let admission = bindings
            .register_source_install_in(&scopes, scope, std::mem::take(&mut staged.leases))
            .unwrap();
        let target = engine.commit_certified_turn(staged);
        (original, target, admission)
    };
    let (original_a, _, _) = install(a, demand_a, &mut bindings_a, scope_a);
    let (original_b, target_b, admission_b) = install(b, demand_b, &mut bindings_b, scope_b);
    for (engine, original) in [(&*a, original_a), (&*b, original_b)] {
        assert!(Arc::ptr_eq(
            &engine.programs[&original].definitions,
            &shared
        ));
    }
    assert_eq!(registry.misses() - misses, 2);
    assert_eq!(registry.hits() - hits, 2);
    let caf_a = a.machine.retain_top(original_a, ValueId(0)).unwrap();
    let caf_b = b.machine.retain_top(original_b, ValueId(0)).unwrap();
    assert_ne!(
        a.machine.handle_root(caf_a).unwrap().addr(),
        b.machine.handle_root(caf_b).unwrap().addr()
    );
    assert!(!a.machine.handle_is_evaluated(caf_a).unwrap());
    assert!(!b.machine.handle_is_evaluated(caf_b).unwrap());
    let force = |engine: &mut PreparedEngine, original, package| {
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
            panic!("original CAF must return its actual package closure");
        };
        assert_eq!(
            engine.machine.handle_root(*value).unwrap().current(),
            engine.machine.handle_root(package).unwrap().current()
        );
        assert!(engine.machine.release(*value));
    };
    force(a, original_a, package_handle_a);
    assert!(a.machine.handle_is_evaluated(caf_a).unwrap());
    assert!(
        !b.machine.handle_is_evaluated(caf_b).unwrap(),
        "a's shared image must not update b's CAF"
    );
    force(b, original_b, package_handle_b);
    assert!(b.machine.handle_is_evaluated(caf_b).unwrap());
    assert!(a.machine.release(caf_a));
    assert!(b.machine.release(caf_b));
    drop(session_a);
    force(b, original_b, package_handle_b);
    for lease in bindings_b
        .rollback_source_admission(scope_b, &admission_b)
        .unwrap()
    {
        assert!(b.release(lease.handle()));
    }
    assert!(b.unpin(target_b));
    assert!(b.unpin(package_b));
}

#[test]
fn immutable_package_only_original_refuses_missing_or_changed_proof_atomically() {
    let compiled = compile_package_original_fixture();
    let registry = Arc::new(ImageRegistry::new());
    let (mut session, package_program, _placement) =
        package_original_machine(&compiled, registry.clone());
    let engine = session.require_prepared().unwrap();
    let (package, export) = engine
        .code_exports
        .iter()
        .filter(|(_, export)| {
            export.program == package_program
                && export.interface_digest.is_some()
                && export.entry.is_some()
        })
        .min_by_key(|(binder, _)| (*binder).clone())
        .expect("the actual compiled target must export a certified package callable");
    let package = package.clone();
    let digest = export.interface_digest.unwrap();
    let signature = export.entry.clone();
    let rep = export.handle.rep();
    assert_eq!(rep, RuntimeRep::LiftedRef);
    let mut changed = digest;
    changed[0] ^= 1;
    assert_ne!(digest, [0; 32]);
    assert_ne!(changed, digest);
    let before = engine.residency();
    let roots = engine.persistent_roots_count();
    let programs = engine.programs.len();
    let install = |engine: &mut PreparedEngine, requested, entry| {
        let group = package_original_group(package_original_owner(&package, requested), rep, entry);
        let target = package_original_target(&group, &registry);
        let evidence = certified_source_evidence(std::slice::from_ref(&group));
        engine.install_certified_turn(
            ScopedTargetImage {
                demanded: vec![DemandedImage::compile(group, &registry).unwrap()],
                ..target
            },
            &evidence,
            &[],
            &BTreeMap::new(),
            &HashMap::new(),
            &BindingTable::new(),
            &HashMap::new(),
        )
    };
    let mut absent = PreparedEngine::empty_certified(1024 * 1024, None).unwrap();
    assert!(matches!(install(&mut absent, digest, signature.clone()),
        Err(PreparedRuntimeError::CertifiedPackageOwnerUnavailable { owner, evidence: CertifiedPackageOwnerEvidence::TargetDefinition { present: false, .. } })
        if owner == package_original_owner(&package, digest)));
    assert_eq!(absent.residency().programs, 0);
    for (requested, installed) in [
        (digest, None),
        (digest, Some(changed)),
        (changed, Some(digest)),
        ([0; 32], Some(digest)),
    ] {
        engine
            .code_exports
            .get_mut(&package)
            .unwrap()
            .interface_digest = installed;
        let error = install(engine, requested, signature.clone()).err().unwrap();
        if requested == [0; 32] {
            assert!(
                matches!(error, PreparedRuntimeError::MissingCertifiedOwner(owner) if owner == package_original_owner(&package, requested))
            );
        } else {
            assert!(
                matches!(error, PreparedRuntimeError::CertifiedPackageOwnerUnavailable { owner, evidence: CertifiedPackageOwnerEvidence::RetainedExport { interface_digest } }
                if owner == package_original_owner(&package, requested) && interface_digest == installed)
            );
        }
        assert_eq!(engine.residency(), before);
        assert_eq!(engine.persistent_roots_count(), roots);
        assert_eq!(engine.programs.len(), programs);
        assert_eq!(engine.code_exports[&package].interface_digest, installed);
    }
    engine
        .code_exports
        .get_mut(&package)
        .unwrap()
        .interface_digest = Some(digest);
    let mut mismatched = signature.clone().unwrap();
    mismatched.arguments.push(RuntimeRep::LiftedRef);
    assert!(matches!(install(engine, digest, Some(mismatched)),
        Err(PreparedRuntimeError::Install(ExecutionError::BatchImportContract(evidence)))
        if evidence.owner == Some(package_original_owner(&package, digest))));
    assert_eq!(engine.residency(), before);
    assert_eq!(engine.persistent_roots_count(), roots);
    assert_eq!(engine.programs.len(), programs);
    let mut staged = install(engine, digest, signature).unwrap();
    assert_eq!(
        staged.groups.len(),
        1,
        "the same machine remains usable after all refusals"
    );
    let tokens = std::mem::take(&mut staged.leases);
    engine.abort_certified_turn(staged, tokens).unwrap();
    assert_eq!(engine.residency(), before);
}
