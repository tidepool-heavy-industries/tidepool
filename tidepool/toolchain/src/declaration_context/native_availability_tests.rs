use super::*;
use crate::certified_products::tests::{
    original_groups_fixture_with_interface, recovered_witness_fixtures,
};
use crate::certified_products::{fixture_finalized_product, fixture_module_core};

const PRODUCER: &[u8] = b"protected-native-availability-fixture";

fn product(module: &str, version: u8) -> CertifiedRecoveryProduct {
    let finalized = fixture_finalized_product(
        original_groups_fixture_with_interface(
            module,
            vec![(7, vec![]), (9001, vec![])],
            version,
            &BTreeMap::new(),
            module.as_bytes().to_vec(),
        ),
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
    );
    recovered_witness_fixtures(&[finalized]).remove(0).product
}

fn type_context(products: &[CertifiedRecoveryProduct]) -> Arc<ExactDeclarationContext> {
    let inventory = ArtifactInventory::default();
    let entries = products
        .iter()
        .map(|product| {
            Arc::new(ArtifactEntry::canonical(
                product.module_interface().unwrap().clone(),
            ))
        })
        .collect::<Vec<_>>();
    let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
    let view = inventory
        .admit_shared(&inventory.empty_view(), entries)
        .unwrap();
    Arc::new(ExactDeclarationContext {
        producer: CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
        inventory: view,
        compiler_projection: projection,
        lexical: vec![],
        template_imports: None,
        original_instance_environment: OriginalInstanceEnvironment::Unknown,
    })
}

fn scope(request: &ExactCompilationRequest) -> Vec<Value> {
    let bytes = std::fs::read(&request.manifest).unwrap();
    let value: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
    let Value::Array(fields) = value else {
        panic!("scope array");
    };
    fields
}

fn dependent_product(
    module: &str,
    version: u8,
    imports: Vec<(u32, Vec<crate::certified_products::PendingImportOwner>)>,
) -> CertifiedRecoveryProduct {
    fixture_finalized_product(
        original_groups_fixture_with_interface(
            module,
            imports,
            version,
            &BTreeMap::new(),
            module.as_bytes().to_vec(),
        ),
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
    )
}

fn source_import(
    product: &CertifiedRecoveryProduct,
    ordinal: u32,
) -> crate::certified_products::PendingImportOwner {
    crate::certified_products::PendingImportOwner::Source {
        owner: product.owner().clone(),
        original_ordinal: ordinal,
        binder: tidepool_repr::execution_schema::testing::identity(
            &product.owner().module,
            &format!("entry_{ordinal}"),
        ),
    }
}

fn partial_context(
    baseline: &CertifiedRecoveryProduct,
    required: &CertifiedRecoveryProduct,
    unused: &CertifiedRecoveryProduct,
    required_group: Option<u32>,
) -> Arc<ExactDeclarationContext> {
    use crate::artifact_inventory::NativeGroupKey;
    let inventory = ArtifactInventory::default();
    let original = |product: &CertifiedRecoveryProduct| {
        Arc::new(
            ArtifactEntry::original_with_validation(
                CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
                product.clone(),
                &mut PackageInterfaceValidation::default(),
            )
            .unwrap(),
        )
    };
    let baseline_entry = original(baseline);
    let required_entry = original(required);
    let required_role = if required_group.is_some() {
        Arc::clone(&required_entry)
    } else {
        Arc::new(ArtifactEntry::canonical(
            required.module_interface().unwrap().clone(),
        ))
    };
    let unused_role = Arc::new(ArtifactEntry::canonical(
        unused.module_interface().unwrap().clone(),
    ));
    let projection = CompilerInputProjection::from_issued_entries(&[
        Arc::clone(&baseline_entry),
        Arc::clone(&required_role),
        Arc::clone(&unused_role),
    ])
    .unwrap();
    let mut groups = BTreeSet::from([NativeGroupKey {
        artifact: baseline_entry.descriptor.id,
        original_ordinal: 7,
    }]);
    if let Some(ordinal) = required_group {
        groups.insert(NativeGroupKey {
            artifact: required_entry.descriptor.id,
            original_ordinal: ordinal,
        });
    }
    let mut entries = vec![baseline_entry, required_entry, unused_role];
    if required_group.is_none() {
        entries.push(required_role);
    }
    let view = inventory
        .admit_recovery_selection(&inventory.empty_view(), entries, &groups)
        .unwrap();
    Arc::new(ExactDeclarationContext {
        producer: CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
        inventory: view,
        compiler_projection: projection,
        lexical: vec![],
        template_imports: None,
        original_instance_environment: OriginalInstanceEnvironment::Unknown,
    })
}

fn wire_groups(request: &ExactCompilationRequest) -> BTreeMap<String, Vec<u32>> {
    scope(request)[6]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let row = row.as_array().unwrap();
            let module = row[1].as_text().unwrap().to_owned();
            let ordinals = row[6]
                .as_array()
                .unwrap()
                .iter()
                .map(|group| {
                    let ordinal = group.as_array().unwrap()[0].as_integer().unwrap();
                    u32::try_from(ordinal).unwrap()
                })
                .collect();
            (module, ordinals)
        })
        .collect()
}

#[test]
fn private_native_availability_preserves_partial_baseline_and_exact_advertised_closure() {
    let unused = product("UnusedNative", 40);
    let required = product("RequiredNative", 41);
    let baseline = dependent_product(
        "BaselineNative",
        42,
        vec![(7, vec![]), (9001, vec![source_import(&unused, 7)])],
    );
    let extra = product("ExtraNative", 43);
    let root = tempfile::tempdir().unwrap();
    let mut histories = 0;
    for demanded in [7, 9001] {
        let added = dependent_product(
            "AddedNative",
            44,
            vec![
                (7, vec![source_import(&baseline, 7)]),
                (9001, vec![source_import(&required, demanded)]),
            ],
        );
        let recovered = recovered_witness_fixtures(&[
            unused.clone(),
            required.clone(),
            baseline.clone(),
            added,
            extra.clone(),
        ]);
        let get = |module: &str| {
            recovered
                .iter()
                .find(|row| row.product.owner().module == module)
                .unwrap()
                .product
                .clone()
        };
        let baseline = get("BaselineNative");
        let required = get("RequiredNative");
        let added = get("AddedNative");
        for selected in [None, Some(7), Some(9001)] {
            for explicit_required in [false, true] {
                for unrelated in [false, true] {
                    histories += 1;
                    let context = partial_context(&baseline, &required, &unused, selected);
                    let before = context.as_ref().clone();
                    let mut offers = vec![added.clone()];
                    if explicit_required {
                        offers.push(required.clone());
                    }
                    if unrelated {
                        offers.push(extra.clone());
                    }
                    let input = OriginalCompilerInputs::from_native_availability(
                        &context,
                        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
                        &offers,
                    );
                    // Expected membership comes from the explicit history inputs,
                    // independently of the production projection/group maps.
                    let expected = explicit_required || selected == Some(demanded);
                    assert_eq!(input.is_ok(), expected,
                        "demanded={demanded}, selected={selected:?}, private={explicit_required}, extra={unrelated}");
                    if let Ok(input) = input {
                        let request = ExactCompileContext::new(context.clone())
                            .prepare_compilation_with_private_input(
                                &root.path().join(histories.to_string()),
                                PRODUCER,
                                None,
                                Some(input),
                            )
                            .unwrap();
                        let rows = wire_groups(&request);
                        assert_eq!(rows["BaselineNative"], vec![7]);
                        assert_eq!(rows["AddedNative"], vec![7, 9001]);
                        if explicit_required {
                            assert_eq!(rows["RequiredNative"], vec![7, 9001]);
                        } else {
                            assert_eq!(rows["RequiredNative"], vec![demanded]);
                        }
                        assert_eq!(rows.contains_key("ExtraNative"), unrelated);
                        assert!(!rows.contains_key("UnusedNative"));
                        assert_eq!(
                            request.compiler_inputs().metadata.selected_native_groups,
                            before.inventory.metadata_snapshot().selected_native_groups
                        );
                        assert_eq!(request.context().as_ref(), &before);
                    }
                    assert_eq!(context.as_ref(), &before);
                }
            }
        }
    }
    assert_eq!(histories, 24);
}

#[test]
fn private_native_availability_requires_complete_explicit_same_owner_upgrade() {
    let unused = product("UnusedNative", 50);
    let required = product("RequiredNative", 51);
    let baseline = dependent_product(
        "BaselineNative",
        52,
        vec![(7, vec![]), (9001, vec![source_import(&unused, 7)])],
    );
    let recovered = recovered_witness_fixtures(&[unused, required, baseline]);
    let get = |module: &str| {
        recovered
            .iter()
            .find(|row| row.product.owner().module == module)
            .unwrap()
            .product
            .clone()
    };
    let baseline = get("BaselineNative");
    let unused = get("UnusedNative");
    let context = partial_context(&baseline, &get("RequiredNative"), &unused, None);
    let before = context.as_ref().clone();
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    assert!(OriginalCompilerInputs::from_native_availability(
        &context,
        producer,
        std::slice::from_ref(&baseline),
    )
    .is_err());
    let input =
        OriginalCompilerInputs::from_native_availability(&context, producer, &[baseline, unused])
            .unwrap();
    let root = tempfile::tempdir().unwrap();
    let request = ExactCompileContext::new(context.clone())
        .prepare_compilation_with_private_input(root.path(), PRODUCER, None, Some(input))
        .unwrap();
    let rows = wire_groups(&request);
    assert_eq!(rows["BaselineNative"], vec![7, 9001]);
    assert_eq!(rows["UnusedNative"], vec![7, 9001]);
    assert_eq!(context.as_ref(), &before);
    assert_eq!(
        request.compiler_inputs().metadata.selected_native_groups,
        before.inventory.metadata_snapshot().selected_native_groups
    );
}

#[test]
fn initial_private_native_offer_reaches_wire_without_lexical_or_instance_authority() {
    let native = product("HiddenNative", 23);
    let context = type_context(std::slice::from_ref(&native));
    let before = context.as_ref().clone();
    let compiler = ExactCompileContext::new(context.clone());
    let root = tempfile::tempdir().unwrap();
    let baseline = compiler
        .prepare_compilation_with_authorization(
            &root.path().join("baseline"),
            PRODUCER,
            Some(text("same-authorization")),
        )
        .unwrap();
    let input = OriginalCompilerInputs::from_native_availability(
        &context,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        std::slice::from_ref(&native),
    )
    .unwrap();
    let request = compiler
        .prepare_compilation_with_private_input(
            &root.path().join("native"),
            PRODUCER,
            Some(text("same-authorization")),
            Some(input),
        )
        .unwrap();
    assert_eq!(context.as_ref(), &before);
    assert_eq!(request.context().as_ref(), &before);
    assert_eq!(request.semantic_sha256, baseline.semantic_sha256);
    assert_ne!(request.request_sha256, baseline.request_sha256);
    assert_eq!(
        context.compiler_input_roles(),
        before.compiler_input_roles()
    );
    assert!(context
        .compiler_input_roles()
        .iter()
        .all(|role| role.original().is_none()));
    assert_eq!(
        context.original_instance_environment,
        OriginalInstanceEnvironment::Unknown
    );
    let baseline_scope = scope(&baseline);
    let actual_scope = scope(&request);
    assert!(baseline_scope[6].as_array().unwrap().is_empty());
    assert_eq!(actual_scope[5], baseline_scope[5]);
    assert_eq!(actual_scope[8], baseline_scope[8]);
    let rows = actual_scope[6].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row = rows[0].as_array().unwrap();
    assert_eq!(row[2], text(hex(&native.owner().module_version.0)));
    assert_eq!(row[4], text(hex(&native.owner().product_sha256)));
    assert_eq!(
        row[6]
            .as_array()
            .unwrap()
            .iter()
            .map(|group| { group.as_array().unwrap()[0].clone() })
            .collect::<Vec<_>>(),
        vec![Value::Integer(7.into()), Value::Integer(9001.into())]
    );
    assert!(request.groups.is_empty());
    assert_eq!(
        request.compiler_inputs().metadata.selected_native_groups,
        baseline.compiler_inputs().metadata.selected_native_groups
    );
    let mut command = tidepool_extract_cmd::ExtractCmd::with_bin(
        tidepool_extract_cmd::ResolvedExtractBin::assume_resolved(
            "/request-construction-only/compiler",
        ),
    );
    request
        .apply_to(
            &mut command,
            RetainedGenerationPolicy::PreserveCertifiedDemand,
        )
        .unwrap();
    let argv = command.argv();
    let index = argv
        .iter()
        .position(|arg| arg == "--session-artifacts")
        .unwrap();
    assert_eq!(Path::new(&argv[index + 1]), request.manifest);
    let encoded = command.request_bytes();
    assert_eq!(
        tidepool_extract_cmd::ExtractRequest::decode(&encoded)
            .unwrap()
            .encode(),
        encoded
    );
}

#[test]
fn private_native_offer_refuses_core_drift_and_retained_resume_ambiguity() {
    let first = product("AmbiguousNative", 23);
    let second = product("AmbiguousNative", 24);
    assert_eq!(first.module_interface(), second.module_interface());
    assert_ne!(first.owner(), second.owner());
    let context = type_context(std::slice::from_ref(&first));
    let mut changed = context.as_ref().clone();
    let interface =
        fixture_module_core(first.module_interface().unwrap(), b"changed-Core".to_vec());
    let entry = Arc::new(ArtifactEntry::canonical(interface));
    changed.inventory = changed
        .inventory
        .inventory()
        .admit_shared(
            &changed.inventory.inventory().empty_view(),
            vec![Arc::clone(&entry)],
        )
        .unwrap();
    changed.compiler_projection = CompilerInputProjection::from_issued_entries(&[entry]).unwrap();
    assert!(OriginalCompilerInputs::from_native_availability(
        &changed,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        std::slice::from_ref(&first),
    )
    .is_err());
    let entries = [&first, &second]
        .into_iter()
        .map(|product| {
            Arc::new(
                ArtifactEntry::original_with_validation(
                    CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
                    product.clone(),
                    &mut PackageInterfaceValidation::default(),
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let mut ambiguous = context.as_ref().clone();
    ambiguous.inventory = ambiguous
        .inventory
        .inventory()
        .admit_shared(&ambiguous.inventory, entries.clone())
        .unwrap();
    assert!(ambiguous
        .inventory
        .metadata_snapshot()
        .ambiguous_native_owners
        .contains(&identity("fixture", "AmbiguousNative"),));
    assert!(OriginalCompilerInputs::from_native_availability(
        &ambiguous,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        std::slice::from_ref(&first),
    )
    .is_err());
    // An issued winner remains explicit even when other versions stay in custody.
    ambiguous.compiler_projection =
        CompilerInputProjection::from_issued_entries(&[Arc::clone(&entries[0])]).unwrap();
    assert!(OriginalCompilerInputs::from_native_availability(
        &ambiguous,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        std::slice::from_ref(&first),
    )
    .is_ok());
    assert!(OriginalCompilerInputs::from_native_availability(
        &ambiguous,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        std::slice::from_ref(&second),
    )
    .is_err());
}

#[test]
fn availability_refuses_wrong_external_group_before_advertising_binders() {
    use crate::certified_products::{
        certify_candidate_original_with_validation, OriginalNativeCandidate, PendingImportOwner,
    };
    let required = product("RequiredNative", 30);
    let wrong = fixture_finalized_product(
        original_groups_fixture_with_interface(
            "DependentNative",
            vec![(
                7,
                vec![PendingImportOwner::Source {
                    owner: required.owner().clone(),
                    original_ordinal: 999,
                    binder: tidepool_repr::execution_schema::testing::identity(
                        "RequiredNative",
                        "entry_7",
                    ),
                }],
            )],
            31,
            &BTreeMap::new(),
            b"DependentNative".to_vec(),
        ),
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
    );
    let wrong = certify_candidate_original_with_validation(
        OriginalNativeCandidate {
            owner: wrong.owner().clone(),
            product: crate::module_candidates::CandidateProduct::decode(
                wrong.product_bytes().to_vec(),
            )
            .unwrap(),
            certification_bytes: wrong.certification_bytes().to_vec(),
            module_interface: wrong.module_interface().unwrap().clone(),
            execution_source: None,
        },
        &mut PackageInterfaceValidation::default(),
    )
    .unwrap();
    let context = type_context(&[wrong.clone(), required.clone()]);
    assert!(OriginalCompilerInputs::from_native_availability(
        &context,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        &[wrong, required],
    )
    .is_err());
}
