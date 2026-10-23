use super::*;

use crate::artifact_inventory::{ArtifactEntry, CanonicalProducerIdentity, NativeArtifactDemand};
use crate::cache::{DependencyEvidence, ModuleEvidence, SourceEvidence};
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
    Group, HeapBinding, HeapRhs, ResultContract, RuntimeRep, SignatureId, SymbolIdentity,
    TopBinding, ValueId, ValueRef,
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
        package_products: Vec::new(),
        package_links: crate::certified_products::PackageLinks::empty(),
        modules,
        targets: BTreeMap::new(),
        packages: packages.clone(),
        finalization,
        source_recipe: WorkerExecutionSource::Ordinary,
    };
    let mut certified = certified_products::certify_products(
        None,
        &packet,
        &parsed,
        &crate::artifacts::admit_compiler_source(
            (&parsed).operation(),
            &evidence_bytes,
            &input,
            &source,
        )
        .unwrap(),
        root,
        &PRODUCER,
        &[root.to_path_buf()],
        None,
    )
    .unwrap();
    crate::declaration_context::issue_certified_product_native_availability(
        CanonicalProducerIdentity::from_producer_bytes(&PRODUCER).sha256(),
        &mut certified,
        None,
        &mut PackageInterfaceValidation::with_inventory(Arc::clone(parsed.operation())),
    )
    .unwrap();
    certified
}

fn select_original_groups(
    certified: &CertifiedProducts,
    validation: &mut PackageInterfaceValidation,
) -> Result<crate::declaration_context::CertifiedArtifactGroupBindings, CompileError> {
    let producer = CanonicalProducerIdentity::from_producer_bytes(&PRODUCER).sha256();
    let view = crate::declaration_context::certified_product_artifact_view_with_validation(
        producer,
        &certified.recovery_products,
        &certified.module_interfaces,
        &certified.value_interfaces,
        None,
        NativeArtifactDemand::AllGroups,
        validation,
    )?;
    crate::declaration_context::certify_issued_artifact_view_groups_with_validation(
        &view,
        &certified.groups,
        None,
        validation,
    )
}

fn selected_package_closure(
    current: &BTreeMap<(String, String), PackageInterfaceWitness>,
    certified: &CertifiedProducts,
    groups: &[certified_products::PendingCertifiedGroup],
    validation: &mut PackageInterfaceValidation,
) -> Result<BTreeMap<(String, String), PackageInterfaceWitness>, CompileError> {
    let catalog = package_availability_with_validation(current, certified, validation)?;
    catalog.select(groups, &[], &validation.inventory)
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
    let issued = issue_original(directory.path(), &packages);
    assert_eq!(issued.data().recovery_products.len(), 1);
    assert_eq!(issued.data().groups.len(), 1);
    assert!(matches!(issued.data().groups[0].imports(),
        [ReceiptImportOwner::Package { unit, module, binder, interface_digest }]
        if unit == PACKAGE_UNIT && module == PACKAGE_MODULE
            && binder == &literal() && *interface_digest == witness.sha256));

    let producer = CanonicalProducerIdentity::from_producer_bytes(&PRODUCER).sha256();
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
    let original = &issued.data().recovery_products[0];
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
    let physical = issued
        .compiler_inputs()
        .expect("completed products retain their actual physical inputs")
        .as_ref();
    assert!(physical
        .issued_artifacts()
        .selected_native_groups()
        .is_empty());
    let completed = issued
        .source_selection()
        .produced_source_receipt(
            &crate::declaration_join::ExactModuleIdentity {
                unit: "main".into(),
                module: MODULE.into(),
            },
            physical.issued_artifacts(),
        )
        .unwrap()
        .unwrap();
    let original_id = ArtifactEntry::original(producer, original.clone())
        .unwrap()
        .descriptor
        .id;
    let own_groups = completed
        .groups(crate::artifact_inventory::NativeGroupKey {
            artifact: original_id,
            original_ordinal: ORDINAL,
        })
        .collect::<Vec<_>>();
    assert_eq!(own_groups.len(), 1);
    let initial_binding = own_groups[0];
    let mut target_validation = PackageInterfaceValidation::default();
    let observed = certified_products::AuthenticatedTargetImports::issue(
        &target,
        &accepted,
        &target_validation.inventory,
    )
    .unwrap();
    let initial_target = physical
        .issued_artifacts()
        .inventory()
        .select_native_target(
            issued.physical_cohort().unwrap(),
            observed,
            None,
            None,
            &[],
            &BTreeMap::new(),
            &mut target_validation,
        )
        .unwrap();
    assert_eq!(
        initial_target.artifacts().selected_native_binding_rows().1,
        [initial_binding]
    );
    let closure = issued
        .source_selection()
        .selected_original_closure(initial_target.artifacts())
        .unwrap();
    let published =
        crate::artifact_inventory::CompilerSourceSupport::close_complete_output(physical)
            .unwrap()
            .certify_original_closure(&closure)
            .unwrap();
    let private = OriginalCompilerInputs::from_issued_support(&published);
    let public = Arc::new(
        ExactDeclarationContext::from_authenticated_interfaces(
            producer,
            &crate::artifact_inventory::ArtifactInventory::default().empty_view(),
            crate::artifact_inventory::CompilerInputProjection::default(),
        )
        .unwrap(),
    );
    let public_hash = public.semantic_sha256();
    let empty_request = public
        .prepare_compilation_with_private_input(
            &directory.path().join("empty-private-inputs"),
            &PRODUCER,
            None,
            Some(physical.clone()),
        )
        .unwrap();
    assert!(empty_request.groups.is_empty());
    let empty_inputs = empty_request.compiler_inputs();
    let empty_selection =
        CertifiedSourceSelection::from_compiler_inputs(empty_inputs, &target_validation.inventory)
            .unwrap();
    let empty_inventory = empty_inputs.artifacts().inventory();
    let empty_fixed = empty_inventory
        .bind_segment(
            empty_inputs.artifacts(),
            vec![],
            &empty_selection,
            &mut target_validation,
        )
        .unwrap();
    let observed = certified_products::AuthenticatedTargetImports::issue(
        &target,
        &accepted,
        &target_validation.inventory,
    )
    .unwrap();
    let refusal = empty_inventory
        .select_native_target(
            empty_fixed.captured_stage().physical_cohort(),
            observed,
            None,
            None,
            &[],
            &BTreeMap::new(),
            &mut target_validation,
        )
        .unwrap_err();
    assert!(matches!(refusal, CompileError::ArtifactInventory(error)
        if matches!(error.native_refusal, Some(crate::artifact_inventory::NativeAdmissionRefusal::RootChoiceExplicitEmpty { .. }))));
    let request = public
        .prepare_compilation_with_private_input(
            &directory.path().join("private-inputs"),
            &PRODUCER,
            None,
            Some(private.clone()),
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
    assert_eq!(
        effective.artifacts().selected_native_binding_rows().1,
        [initial_binding]
    );
    let choice = effective
        .compiler_source_support()
        .checked_native_input_decision(
            original_id,
            ArtifactEntry::canonical_artifact_id(original.module_interface()),
            original.owner(),
        )
        .unwrap();
    assert!(
        matches!(choice, crate::artifact_inventory::InputNativeDecision::IssuedChoices(groups)
        if groups.get(&ORDINAL) == Some(&initial_binding))
    );
    let selection =
        CertifiedSourceSelection::from_compiler_inputs(effective, &target_validation.inventory)
            .unwrap();
    let target_inventory = effective.artifacts().inventory();
    let fixed = target_inventory
        .bind_segment(
            effective.artifacts(),
            vec![],
            &selection,
            &mut target_validation,
        )
        .unwrap();

    let observed = certified_products::AuthenticatedTargetImports::issue(
        &target,
        &accepted,
        &target_validation.inventory,
    )
    .unwrap();
    let admitted = target_inventory
        .select_native_target(
            fixed.captured_stage().physical_cohort(),
            observed,
            None,
            None,
            &[],
            &BTreeMap::new(),
            &mut target_validation,
        )
        .unwrap();
    assert!(
        matches!(admitted.imports(), [PendingImportOwner::Source { binder, .. }]
        if binder == &reader())
    );

    // Public inheritance retains availability without selecting a body or
    // granting lexical imports for this original.
    let public_support =
        crate::artifact_inventory::CompilerSourceSupport::close_complete_output(physical).unwrap();
    let public_original = Arc::new(
        ExactDeclarationContext::new(&[], &[], Vec::new())
            .unwrap()
            .extend_issued_compiler_support(&public_support)
            .unwrap(),
    );
    let public_request = public_original
        .prepare_compilation(&directory.path().join("public-original-inputs"), &PRODUCER)
        .unwrap();
    assert!(public_request.groups.is_empty());
    assert!(public_original
        .artifact_view()
        .selected_native_groups()
        .is_empty());
    let public_inputs = public_request.compiler_inputs();
    let expected_original = ArtifactEntry::original(producer, original.clone())
        .unwrap()
        .descriptor
        .id;
    let offered_carriers = public_inputs
        .compiler_source_support()
        .native_offers()
        .iter()
        .filter(|offer| offer.original() == expected_original && offer.is_full())
        .flat_map(|offer| offer.issued_group_carriers().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(offered_carriers.len(), 1);
    let (offered_group, offered_carrier) = offered_carriers.iter().next().unwrap();
    assert_eq!(offered_group.binding.artifact(), expected_original);
    assert_eq!(offered_group.original_ordinal, ORDINAL);
    assert_eq!(offered_carrier.artifact, expected_original);
    let mut public_validation = PackageInterfaceValidation::default();
    let public_selection =
        CertifiedSourceSelection::from_compiler_inputs(public_inputs, &public_validation.inventory)
            .unwrap();
    let inventory = public_inputs.artifacts().inventory();
    let public_fixed = inventory
        .bind_segment(
            public_inputs.artifacts(),
            vec![],
            &public_selection,
            &mut public_validation,
        )
        .unwrap();
    let observed = certified_products::AuthenticatedTargetImports::issue(
        &target,
        &accepted,
        &public_validation.inventory,
    )
    .unwrap();
    let public_before = public_inputs.artifacts().clone();
    let public_original_hash = public_original.semantic_sha256();
    let refused = inventory
        .select_native_target(
            public_fixed.captured_stage().physical_cohort(),
            observed,
            None,
            None,
            &[],
            &BTreeMap::new(),
            &mut public_validation,
        )
        .err()
        .expect("another request's Source imports cannot select an empty public choice");
    assert!(matches!(refused, CompileError::ArtifactInventory(error)
        if matches!(error.native_refusal, Some(crate::artifact_inventory::NativeAdmissionRefusal::RootChoiceExplicitEmpty { .. }))));
    assert!(public_request.groups.is_empty());
    assert_eq!(public_inputs.artifacts(), &public_before);
    assert_eq!(public_original.semantic_sha256(), public_original_hash);
    let inherited = selected_package_closure(
        &BTreeMap::new(),
        &issued,
        request.groups.as_ref(),
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    assert_eq!(
        inherited.get(&(PACKAGE_UNIT.into(), PACKAGE_MODULE.into())),
        Some(&witness)
    );
    assert!(!inherited.contains_key(&arbitrary_owner));
    std::fs::write(&witness.selected_path, b"changed package interface").unwrap();
    assert!(matches!(
        PackageInterfaceValidation::default().verify(&witness.selected_path, &witness.sha256),
        Err(crate::recovery_artifacts::RecoveryArtifactError::DigestMismatch(path))
            if path == witness.selected_path
    ));
    assert_eq!(
        selected_package_closure(
            &BTreeMap::new(),
            &issued,
            request.groups.as_ref(),
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap(),
        inherited,
        "already captured package custody survives mutation of its original path"
    );
    std::fs::remove_file(&witness.selected_path).unwrap();
    assert!(matches!(
        PackageInterfaceValidation::default().verify(&witness.selected_path, &witness.sha256),
        Err(crate::recovery_artifacts::RecoveryArtifactError::Unavailable(path))
            if path == witness.selected_path
    ));
    assert_eq!(
        selected_package_closure(
            &BTreeMap::new(),
            &issued,
            request.groups.as_ref(),
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap(),
        inherited,
        "already captured package custody survives removal of its original path"
    );
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
        request.groups.as_ref(),
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
        request.groups.as_ref(),
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
    let unselected_validation = &mut PackageInterfaceValidation::default();
    let unselected =
        package_availability_with_validation(&BTreeMap::new(), &issued, unselected_validation)
            .unwrap()
            .select(&[], &[], &unselected_validation.inventory)
            .unwrap();
    assert!(unselected.is_empty());

    // Empty demand retains package custody without granting Source execution.
    // The earlier permitted request can also consume a direct package global.
    let empty_private = OriginalCompilerInputs::from_issued_support(&public_support);
    assert!(empty_private
        .issued_artifacts()
        .selected_native_groups()
        .is_empty());
    let late_request = public
        .prepare_compilation_with_private_input(
            &directory.path().join("late-private-inputs"),
            &PRODUCER,
            None,
            Some(empty_private.clone()),
        )
        .unwrap();
    assert!(late_request.groups.is_empty());
    let late_effective = late_request.compiler_inputs();
    let mut late_validation = PackageInterfaceValidation::default();
    let late_selection =
        CertifiedSourceSelection::from_compiler_inputs(late_effective, &late_validation.inventory)
            .unwrap();
    let late_inventory = late_effective.artifacts().inventory();
    let late_fixed = late_inventory
        .bind_segment(
            late_effective.artifacts(),
            vec![],
            &late_selection,
            &mut late_validation,
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
    let observed = certified_products::AuthenticatedTargetImports::issue(
        &late_target,
        &late_accepted,
        &late_validation.inventory,
    )
    .unwrap();
    let refused = late_inventory
        .select_native_target(
            late_fixed.captured_stage().physical_cohort(),
            observed,
            None,
            None,
            &[],
            &catalog,
            &mut late_validation,
        )
        .err()
        .expect("package evidence cannot enlarge an empty Source choice");
    assert!(matches!(refused, CompileError::ArtifactInventory(error)
        if matches!(error.native_refusal, Some(crate::artifact_inventory::NativeAdmissionRefusal::RootChoiceExplicitEmpty { .. }))));
    assert!(late_effective
        .artifacts()
        .selected_native_groups()
        .is_empty());
    let observed = certified_products::AuthenticatedTargetImports::issue(
        &late_target,
        &late_accepted,
        &late_validation.inventory,
    )
    .unwrap();
    let late_admitted = target_inventory
        .select_native_target(
            fixed.captured_stage().physical_cohort(),
            observed,
            None,
            None,
            &[],
            &catalog,
            &mut late_validation,
        )
        .unwrap();
    let late_imports = late_admitted.imports();
    assert_eq!(
        late_admitted.artifacts().selected_native_groups(),
        BTreeSet::from([crate::artifact_inventory::NativeGroupKey {
            artifact: expected_original,
            original_ordinal: ORDINAL,
        }])
    );
    assert!(matches!(
        late_imports,
        [
            PendingImportOwner::Source { .. },
            PendingImportOwner::Package { .. }
        ]
    ));
    let late_groups =
        crate::declaration_context::certify_selected_artifact_view_groups_with_validation(
            late_admitted.artifacts(),
            Some(&request.groups),
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
    assert_eq!(late_groups.len(), 1);
    let selection_operation = PackageInterfaceValidation::default().inventory.clone();
    let direct_only = catalog
        .select(&[], &late_imports[1..], &selection_operation)
        .unwrap();
    assert_eq!(
        direct_only.get(&(PACKAGE_UNIT.into(), PACKAGE_MODULE.into())),
        Some(&witness)
    );
    let native_only = catalog
        .select(&late_groups, &[], &selection_operation)
        .unwrap();
    assert_eq!(
        native_only.get(&(PACKAGE_UNIT.into(), PACKAGE_MODULE.into())),
        Some(&witness)
    );
    let late_closure = catalog
        .select(&late_groups, late_imports, &selection_operation)
        .unwrap();
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
    let archive_root = directory.path().join("two-native-groups");
    std::fs::create_dir_all(&archive_root).unwrap();
    let subset = issue_original_with_extra_group(
        &archive_root,
        &packages,
        Some(SymbolIdentity {
            unit: PACKAGE_UNIT.into(),
            ..testing::identity("External.Unselected", "literal")
        }),
    );
    assert_eq!(subset.groups.len(), 2);
    let selected_validation = &mut PackageInterfaceValidation::default();
    let selected_groups = select_original_groups(subset.data(), selected_validation).unwrap();
    let all_groups = selected_package_closure(
        &BTreeMap::new(),
        &subset,
        selected_groups.groups(),
        selected_validation,
    )
    .unwrap();
    assert!(all_groups.contains_key(&arbitrary_owner));
    let subset_groups = selected_groups
        .groups()
        .iter()
        .filter(|group| group.group().original_ordinal() == ORDINAL)
        .cloned()
        .collect::<Vec<_>>();
    let selected =
        package_availability_with_validation(&BTreeMap::new(), &subset, selected_validation)
            .unwrap()
            .select(&subset_groups, &[], &selected_validation.inventory)
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
        request.groups.as_ref(),
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
    // ImageTarget::acquire_originals uses this same-target digest to
    // admit the package literal token consumed by the original reader group.
    assert_eq!(
        certified_target.interface_digest(PACKAGE_UNIT, PACKAGE_MODULE),
        Some(witness.sha256),
        "private original lost its required package literal witness"
    );
}
