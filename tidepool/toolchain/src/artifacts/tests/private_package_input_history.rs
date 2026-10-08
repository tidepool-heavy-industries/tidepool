use super::*;

use crate::artifact_inventory::{CanonicalProducerIdentity, NativeArtifactDemand};
use crate::cache::{CompletedSourceEvidence, DependencyEvidence, ModuleEvidence, SourceEvidence};
use crate::certified_products::{
    AcceptedGlobal, AcceptedGroup, CertifiedModuleReceipt, CertifiedProducts, CertifiedReceipt,
    CertifiedSourceSelection, PackageInterfaceWitness, PendingImportOwner, ProductOrigin,
    ReceiptImportOwner, WorkerExecutionSource,
};
use crate::declaration_context::{ExactDeclarationContext, OriginalCompilerInputs};
use crate::recovery_artifacts::PackageInterfaceValidation;
use sha2::{Digest, Sha256};
use tidepool_repr::execution_schema::{
    testing, Alternative, AlternativePattern, Atom, CaseKind, ExprFrame, GlobalDecl, GlobalId,
    Group, HeapBinding, HeapRhs, InventoryOperation, ResultContract, RuntimeRep, SignatureId,
    SymbolIdentity, TopBinding, ValueId, ValueRef,
};

const MODULE: &str = "PrivatePackageReader";
const ORDINAL: u32 = 7;
const PRODUCER: [u8; 32] = [37; 32];
const PACKAGE_UNIT: &str = "external-package";
const PACKAGE_MODULE: &str = "External.Literals";

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn text(value: impl Into<String>) -> Value {
    Value::Text(value.into())
}

fn array(values: impl IntoIterator<Item = Value>) -> Value {
    Value::Array(values.into_iter().collect())
}

fn encode(value: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut bytes).unwrap();
    bytes
}

fn reader() -> SymbolIdentity {
    SymbolIdentity {
        unit: "main".into(),
        ..testing::identity(MODULE, "reader")
    }
}

fn literal() -> SymbolIdentity {
    SymbolIdentity {
        unit: PACKAGE_UNIT.into(),
        ..testing::identity(PACKAGE_MODULE, "literal")
    }
}

// All inputs here are raw compiler-shaped facts. The production certifier
// issues the canonical interface, original product and native import witness.
fn issue_original(
    root: &Path,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
) -> CertifiedProducts {
    issue_original_with_extra_group(root, packages, None)
}

fn issue_original_with_extra_group(
    root: &Path,
    packages: &BTreeMap<(String, String), PackageInterfaceWitness>,
    extra_package: Option<SymbolIdentity>,
) -> CertifiedProducts {
    let source = format!("module {MODULE} where\n");
    let input = root.join(format!("{MODULE}.hs"));
    std::fs::write(&input, &source).unwrap();
    let mut imports = vec![(ORDINAL, reader(), literal())];
    if let Some(package) = extra_package {
        imports.push((
            ORDINAL + 1,
            SymbolIdentity {
                unit: "main".into(),
                ..testing::identity(MODULE, "unusedReader")
            },
            package,
        ));
    }
    let groups = imports
        .iter()
        .map(|(ordinal, binder, package)| {
            let mut wire = testing::wire_program();
            let Group::NonRecursive(top) = &mut wire.bindings[0] else {
                unreachable!()
            };
            top.identity = binder.clone();
            wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
            wire.expressions.nodes[0] =
                ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
            wire.globals.push(GlobalDecl {
                identity: package.clone(),
                rep: RuntimeRep::Address,
                entry_signature: None,
                required_evaluated: true,
                required_generation: None,
            });
            testing::projected_group(wire, *ordinal).unwrap()
        })
        .collect();
    let interface = b"private reader interface".to_vec();
    let products =
        tidepool_test_data::prepared_encode::encode_module_products(&[RawModuleProduct {
            unit: "main".into(),
            module: MODULE.into(),
            interface: interface.clone(),
            groups,
        }]);
    let package_roots = encode(&array([
        text("TPPKGROOTS"),
        text("2"),
        array([text("main"), text(MODULE), text(hex(&digest(&interface)))]),
        array(packages.iter().map(|((unit, module), witness)| {
            array([
                text(unit),
                text(module),
                text(witness.selected_path.to_string_lossy()),
                text(hex(&witness.sha256)),
            ])
        })),
        array([]),
    ]));
    let package_bundle = encode(&array([
        text("TPPKGBUNDLES"),
        Value::Integer(1.into()),
        array([array([
            text("main"),
            text(MODULE),
            Value::Bytes(package_roots.clone()),
        ])]),
    ]));
    let parsed =
        certified_products::ParsedModuleProducts::decode(&products, &package_bundle).unwrap();
    let evidence = DependencyEvidence {
        version: 4,
        cache_safe: false,
        selection_complete: false,
        sources: vec![SourceEvidence {
            path: cache::GENERATED_SOURCE.into(),
            sha256: hex(&digest(source.as_bytes())),
        }],
        resolutions: Vec::new(),
        packages: vec![PACKAGE_UNIT.into()],
        modules: vec![ModuleEvidence {
            unit: "main".into(),
            module: MODULE.into(),
            boot: false,
            source: cache::GENERATED_SOURCE.into(),
            imports: Vec::new(),
            product: cache::ProductAvailability::Ready,
        }],
    };
    let completed = CompletedSourceEvidence::from_normalized(evidence.clone(), &source).unwrap();
    let mut worker = evidence;
    worker.sources[0].path = input.clone();
    worker.modules[0].source = input.clone();
    let evidence_bytes = serde_json::to_vec(&worker).unwrap();
    let module = CertifiedModuleReceipt {
        origin: ProductOrigin::Fresh,
        unit: "main".into(),
        module: MODULE.into(),
        module_version: None,
        skinny_iface_sha256: digest(&interface),
        product_sha256: digest(&products),
        source_sha256: digest(source.as_bytes()),
        dependency_witness_sha256: digest(&evidence_bytes),
        groups: imports
            .into_iter()
            .map(|(ordinal, _, package)| {
                let witness = &packages[&(package.unit.clone(), package.module.clone())];
                AcceptedGroup {
                    original_ordinal: ordinal,
                    globals: vec![AcceptedGlobal {
                        identity: package.clone(),
                        rep: RuntimeRep::Address,
                        entry_signature: None,
                        required_evaluated: true,
                        owner: ReceiptImportOwner::Package {
                            unit: package.unit.clone(),
                            module: package.module.clone(),
                            binder: package,
                            interface_digest: witness.sha256,
                        },
                    }],
                }
            })
            .collect(),
        interface_requirements: BTreeMap::new(),
    };
    let modules = vec![module];
    let finalization = certified_products::tests::fixture_finalization_from_products(
        Some(root),
        &modules,
        &parsed,
    );
    let packet = CertifiedReceipt {
        modules,
        targets: BTreeMap::new(),
        packages: packages.clone(),
        finalization,
        source_recipe: WorkerExecutionSource::Ordinary,
    };
    certified_products::certify_products(
        None,
        &packet,
        &parsed,
        &evidence_bytes,
        &input,
        root,
        &completed,
        &source,
        &PRODUCER,
        &[root.to_path_buf()],
        None,
        None,
    )
    .unwrap()
}

fn selected_package_closure(
    current: &BTreeMap<(String, String), PackageInterfaceWitness>,
    certified: &CertifiedProducts,
    validation: &mut PackageInterfaceValidation,
) -> Result<BTreeMap<(String, String), PackageInterfaceWitness>, CompileError> {
    package_availability_with_validation(current, certified, validation)?.select(
        &certified.groups,
        &[],
        &validation.inventory,
    )
}

#[test]
fn private_original_package_witness_survives_sparse_literal_target() {
    let directory = tempfile::tempdir().unwrap();
    let package_path = directory.path().join("External.hi");
    let package_bytes = b"external package interface";
    std::fs::write(&package_path, package_bytes).unwrap();
    let witness = PackageInterfaceWitness {
        selected_path: package_path,
        sha256: digest(package_bytes),
    };
    let arbitrary_path = directory.path().join("Unselected.hi");
    std::fs::write(&arbitrary_path, b"unselected package interface").unwrap();
    let arbitrary_owner = (PACKAGE_UNIT.into(), "External.Unselected".into());
    let packages = BTreeMap::from([
        (
            (PACKAGE_UNIT.into(), PACKAGE_MODULE.into()),
            witness.clone(),
        ),
        (
            arbitrary_owner.clone(),
            PackageInterfaceWitness {
                selected_path: arbitrary_path,
                sha256: digest(b"unselected package interface"),
            },
        ),
    ]);
    let mut issued = issue_original(directory.path(), &packages);
    assert_eq!(issued.recovery_products.len(), 1);
    assert_eq!(issued.groups.len(), 1);
    assert!(matches!(issued.groups[0].imports(),
        [PendingImportOwner::Package { unit, module, binder, interface_digest }]
        if unit == PACKAGE_UNIT && module == PACKAGE_MODULE
            && binder == &literal() && *interface_digest == witness.sha256));

    let producer = CanonicalProducerIdentity::from_producer_bytes(&PRODUCER).sha256();
    let view = crate::declaration_context::certified_product_artifact_view_with_validation(
        producer,
        &issued.recovery_products,
        &issued.module_interfaces,
        &issued.value_interfaces,
        None,
        NativeArtifactDemand::AllGroups,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    let private = OriginalCompilerInputs::from_selection(&issued.source_selection, &view).unwrap();
    let public = Arc::new(
        ExactDeclarationContext::new(&[], &[], Vec::new())
            .unwrap()
            .extend_checked_original_products(producer, &[])
            .unwrap(),
    );
    let public_hash = public.semantic_sha256();
    let request = public
        .prepare_compilation(&directory.path().join("public-inputs"), &PRODUCER)
        .unwrap()
        .in_program_context_with_private_input(
            &directory.path().join("private-inputs"),
            public.clone(),
            &private,
        )
        .unwrap();
    assert!(public.recovery_products().is_empty());
    assert!(public.lexical_graph().is_empty());
    assert!(public.compiler_input_roles().is_empty());
    assert_eq!(request.groups.len(), 1);
    assert!(matches!(
        request.groups[0].imports(),
        [PendingImportOwner::Package { .. }]
    ));
    let effective = request.compiler_inputs();
    let selection = CertifiedSourceSelection::from_compiler_projection(
        &effective.projection,
        &effective.metadata,
        &InventoryOperation::new(Default::default()),
    )
    .unwrap();

    let mut target_wire = testing::wire_program();
    target_wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
    target_wire.expressions.nodes[0] = ExprFrame::Call {
        callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
        signature: SignatureId(0),
        arguments: Vec::new(),
    };
    target_wire.globals.push(GlobalDecl {
        identity: reader(),
        rep: RuntimeRep::LiftedRef,
        entry_signature: Some(SignatureId(0)),
        required_evaluated: true,
        required_generation: None,
    });
    target_wire.bindings.push(Group::NonRecursive(TopBinding {
        identity: literal(),
        binding: HeapBinding {
            id: ValueId(1),
            rhs: HeapRhs::Bytes(b"literal\0".to_vec()),
        },
    }));
    let signature = target_wire.signatures[0].clone();
    let mut late_wire = target_wire.clone();
    let target = Arc::new(testing::prepare(target_wire).unwrap());
    let original = &issued.recovery_products[0];
    let accepted = vec![AcceptedGlobal {
        identity: reader(),
        rep: RuntimeRep::LiftedRef,
        entry_signature: Some(signature),
        required_evaluated: true,
        owner: ReceiptImportOwner::Source {
            unit: "main".into(),
            module: MODULE.into(),
            module_version: Some(original.owner().module_version.clone()),
            original_ordinal: ORDINAL,
            binder: reader(),
        },
    }];
    let imports = certified_products::certify_target_available_owners_with_validation(
        &target,
        &accepted,
        &issued.recovery_products,
        request.groups.as_ref(),
        &selection,
        &BTreeMap::new(),
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert!(
        matches!(imports.as_slice(), [PendingImportOwner::Source { binder, .. }]
        if binder == &reader())
    );

    // Public inheritance and a current package receipt independently reach
    // the same consumer. Neither grants lexical imports for this original.
    let public_original = Arc::new(
        ExactDeclarationContext::new(&[], &[], Vec::new())
            .unwrap()
            .extend_checked_original_products(producer, &issued.recovery_products)
            .unwrap(),
    );
    let public_request = public_original
        .prepare_compilation(&directory.path().join("public-original-inputs"), &PRODUCER)
        .unwrap();
    assert_eq!(public_request.groups[0].owner(), issued.groups[0].owner());
    assert_eq!(
        public_request.groups[0].imports(),
        issued.groups[0].imports()
    );
    let inherited = selected_package_closure(
        &BTreeMap::new(),
        &issued,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert_eq!(
        inherited.get(&(PACKAGE_UNIT.into(), PACKAGE_MODULE.into())),
        Some(&witness)
    );
    assert!(!inherited.contains_key(&arbitrary_owner));
    std::fs::write(&witness.selected_path, b"changed package interface").unwrap();
    assert!(selected_package_closure(
        &BTreeMap::new(),
        &issued,
        &mut PackageInterfaceValidation::default(),
    )
    .is_err());
    std::fs::write(&witness.selected_path, package_bytes).unwrap();
    let conflicting_path = directory.path().join("Conflicting.hi");
    std::fs::write(&conflicting_path, package_bytes).unwrap();
    let conflicting = BTreeMap::from([(
        (PACKAGE_UNIT.into(), PACKAGE_MODULE.into()),
        PackageInterfaceWitness {
            selected_path: conflicting_path,
            sha256: witness.sha256,
        },
    )]);
    assert!(selected_package_closure(
        &conflicting,
        &issued,
        &mut PackageInterfaceValidation::default(),
    )
    .is_err());
    let current = BTreeMap::from([(
        (PACKAGE_UNIT.into(), PACKAGE_MODULE.into()),
        witness.clone(),
    )]);
    let current = selected_package_closure(
        &current,
        &issued,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    let current_target = certified_products::certify_target_package_interfaces_with_validation(
        &target,
        &current,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert_eq!(
        current_target.interface_digest(PACKAGE_UNIT, PACKAGE_MODULE),
        Some(witness.sha256)
    );
    assert!(!current.contains_key(&arbitrary_owner));

    // Custody without a selected native package edge does not grant a target
    // package witness, even when the original still owns that witness.
    let selected_groups = std::mem::take(&mut issued.groups);
    let unselected = selected_package_closure(
        &BTreeMap::new(),
        &issued,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert!(unselected.is_empty());

    // A source root and a direct package global can both be admitted before
    // the first native group is selected. Availability is not target evidence.
    let empty_view = view
        .inventory()
        .admit_recovery_selection(
            &view.inventory().empty_view(),
            view.entries(),
            &BTreeSet::new(),
        )
        .unwrap();
    assert!(empty_view.selected_native_groups().is_empty());
    let empty_private =
        OriginalCompilerInputs::from_selection(&issued.source_selection, &empty_view).unwrap();
    let late_request = public
        .prepare_compilation(&directory.path().join("late-public-inputs"), &PRODUCER)
        .unwrap()
        .in_program_context_with_private_input(
            &directory.path().join("late-private-inputs"),
            public.clone(),
            &empty_private,
        )
        .unwrap();
    assert!(late_request.groups.is_empty());
    let late_effective = late_request.compiler_inputs();
    let late_selection = CertifiedSourceSelection::from_compiler_projection(
        &late_effective.projection,
        &late_effective.metadata,
        &InventoryOperation::new(Default::default()),
    )
    .unwrap();
    late_wire.globals.push(GlobalDecl {
        identity: literal(),
        rep: RuntimeRep::Address,
        entry_signature: None,
        required_evaluated: true,
        required_generation: None,
    });
    late_wire
        .expressions
        .nodes
        .push(ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(
            GlobalId(1),
        ))]));
    late_wire.expressions.nodes.push(ExprFrame::Case {
        scrutinee: 0,
        binder: ValueId(2),
        scrutinee_results: ResultContract::Returns(vec![RuntimeRep::Address]),
        kind: CaseKind::Polymorphic,
        alternatives: vec![Alternative {
            pattern: AlternativePattern::Default,
            binders: Vec::new(),
            body: 1,
        }],
    });
    let Group::NonRecursive(top) = &mut late_wire.bindings[0] else {
        unreachable!()
    };
    let HeapRhs::Function { body, .. } = &mut top.binding.rhs else {
        unreachable!()
    };
    *body = 2;
    let late_target = Arc::new(testing::prepare(late_wire).unwrap());
    let mut late_accepted = accepted.clone();
    late_accepted.push(AcceptedGlobal {
        identity: literal(),
        rep: RuntimeRep::Address,
        entry_signature: None,
        required_evaluated: true,
        owner: ReceiptImportOwner::Package {
            unit: PACKAGE_UNIT.into(),
            module: PACKAGE_MODULE.into(),
            binder: literal(),
            interface_digest: witness.sha256,
        },
    });
    let empty_packages = BTreeMap::new();
    let catalog = package_availability_with_validation(
        &empty_packages,
        &issued,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    let late_imports = certified_products::certify_target_available_owners_with_validation(
        &late_target,
        &late_accepted,
        &issued.recovery_products,
        &issued.groups,
        &late_selection,
        &catalog.interfaces,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert!(matches!(
        late_imports.as_slice(),
        [
            PendingImportOwner::Source { .. },
            PendingImportOwner::Package { .. }
        ]
    ));
    let demanded_view =
        crate::declaration_context::certified_product_artifact_view_with_validation(
            producer,
            &issued.recovery_products,
            &issued.module_interfaces,
            &issued.value_interfaces,
            Some(&late_effective.artifacts),
            NativeArtifactDemand::CertifiedTargetImports(&late_imports),
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
    let late_groups = crate::declaration_context::certify_artifact_view_groups_with_validation(
        &demanded_view,
        &issued.groups,
        late_request.groups.as_ref(),
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert_eq!(late_groups.len(), 1);
    let direct_only = catalog.select(&[], &late_imports[1..]).unwrap();
    assert_eq!(
        direct_only.get(&(PACKAGE_UNIT.into(), PACKAGE_MODULE.into())),
        Some(&witness)
    );
    let native_only = catalog.select(&late_groups, &[]).unwrap();
    assert_eq!(
        native_only.get(&(PACKAGE_UNIT.into(), PACKAGE_MODULE.into())),
        Some(&witness)
    );
    let late_closure = catalog.select(&late_groups, &late_imports).unwrap();
    let late_interfaces = certified_products::certify_target_package_interfaces_with_validation(
        &late_target,
        &late_closure,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert_eq!(
        late_interfaces.interface_digest(PACKAGE_UNIT, PACKAGE_MODULE),
        Some(witness.sha256)
    );
    assert!(!late_closure.contains_key(&arbitrary_owner));
    assert!(late_request.context().lexical_graph().is_empty());
    assert!(late_request.context().compiler_input_roles().is_empty());
    issued.groups = selected_groups;
    let archive_root = directory.path().join("two-native-groups");
    std::fs::create_dir_all(&archive_root).unwrap();
    let mut subset = issue_original_with_extra_group(
        &archive_root,
        &packages,
        Some(SymbolIdentity {
            unit: PACKAGE_UNIT.into(),
            ..testing::identity("External.Unselected", "literal")
        }),
    );
    assert_eq!(subset.groups.len(), 2);
    let all_groups = selected_package_closure(
        &BTreeMap::new(),
        &subset,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert!(all_groups.contains_key(&arbitrary_owner));
    subset
        .groups
        .retain(|group| group.group().original_ordinal() == ORDINAL);
    let selected = selected_package_closure(
        &BTreeMap::new(),
        &subset,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert_eq!(
        selected.get(&(PACKAGE_UNIT.into(), PACKAGE_MODULE.into())),
        Some(&witness)
    );
    assert!(!selected.contains_key(&arbitrary_owner));

    // The later output has only a source global and an external Bytes binding.
    // Its sparse module receipt cannot repeat the private reader's imports,
    // and Bytes does not issue a local package export witness in the worker.
    let inherited = selected_package_closure(
        &BTreeMap::new(),
        &issued,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert!(!inherited.contains_key(&arbitrary_owner));
    let certified_target = certified_products::certify_target_package_interfaces_with_validation(
        &target,
        &inherited,
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert!(certified_target.matches_target(&target));
    assert_eq!(public.semantic_sha256(), public_hash);
    assert!(request.context().lexical_graph().is_empty());
    assert!(request.context().compiler_input_roles().is_empty());
    // CertifiedTargetImage::compile_originals uses this same-target digest to
    // admit the package literal token consumed by the original reader group.
    assert_eq!(
        certified_target.interface_digest(PACKAGE_UNIT, PACKAGE_MODULE),
        Some(witness.sha256),
        "private original lost its required package literal witness"
    );
}
