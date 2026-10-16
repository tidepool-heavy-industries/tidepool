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
    Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            producer: CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
            inventory: view,
            compiler_projection: projection,
            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        },
    ))
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
    let mut binder = tidepool_repr::execution_schema::testing::identity(
        &product.owner().module,
        &format!("entry_{ordinal}"),
    );
    binder.unit = product.owner().unit.clone();
    crate::certified_products::PendingImportOwner::Source {
        owner: product.owner().clone(),
        original_ordinal: ordinal,
        binder,
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
    Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            producer: CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256(),
            inventory: view,
            compiler_projection: projection,
            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        },
    ))
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
                    let ordinal = group.as_integer().unwrap();
                    u32::try_from(ordinal).unwrap()
                })
                .collect();
            (module, ordinals)
        })
        .collect()
}

fn wire_mode(request: &ExactCompilationRequest, module: &str) -> Value {
    scope(request)[6]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row.as_array().unwrap()[1] == text(module))
        .unwrap()
        .as_array()
        .unwrap()[8]
        .clone()
}
fn full_mode() -> Value {
    Value::Array(vec![text("full-carrier")])
}
fn issued_mode(ordinals: &[u32]) -> Value {
    Value::Array(vec![
        text("issued-groups"),
        Value::Array(
            ordinals
                .iter()
                .map(|ordinal| Value::Integer((*ordinal).into()))
                .collect(),
        ),
    ])
}

#[test]
fn private_complete_product_prefers_carried_dependency_proof_over_selected_fallback() {
    use crate::artifact_inventory::{NativeArtifactDemand, NativeOriginalRequirement};

    let native = product("CarriedDependency", 75);
    let consumer = dependent_product(
        "PrivateConsumer",
        76,
        vec![(7, vec![source_import(&native, 7)])],
    );
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let recovered = recovered_witness_fixtures(&[native, consumer]);
    let native = &recovered
        .iter()
        .find(|row| row.product.owner().module == "CarriedDependency")
        .unwrap()
        .product;
    let consumer = &recovered
        .iter()
        .find(|row| row.product.owner().module == "PrivateConsumer")
        .unwrap()
        .product;
    let native_entry =
        Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
    let consumer_entry = ArtifactEntry::original(producer.sha256(), consumer.clone()).unwrap();
    let canonical = ArtifactEntry::canonical(native.module_interface().unwrap().clone());
    let inventory = ArtifactInventory::default();
    let base = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            vec![native_entry.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let groups = native_entry
        .native_group_ordinals
        .iter()
        .map(|original_ordinal| NativeGroupKey {
            artifact: native_entry.descriptor.id,
            original_ordinal: *original_ordinal,
        })
        .collect();
    let make = |module: &str, version| {
        let marker = product(module, version);
        inventory
            .admit_recovery_selection(
                &base,
                vec![Arc::new(ArtifactEntry::canonical(
                    marker.module_interface().unwrap().clone(),
                ))],
                &groups,
            )
            .unwrap()
    };
    let left = make("LeftIssuance", 77);
    let right = make("RightIssuance", 78);
    let proof = |view: &ArtifactView| {
        NativeOriginalRequirement::from_issued_group_custody(
            native_entry.descriptor.id,
            canonical.descriptor.id,
            view,
        )
        .unwrap()
        .unwrap()
    };
    let left_proof = proof(&left);
    let right_proof = proof(&right);
    let dependency = |proof: &NativeOriginalRequirement, view: &ArtifactView| {
        proof
            .issued_groups(view)
            .unwrap()
            .into_iter()
            .find(|group| {
                group.binding.artifact == native_entry.descriptor.id && group.original_ordinal == 7
            })
            .unwrap()
    };
    let left_group = dependency(&left_proof, &left);
    let right_group = dependency(&right_proof, &right);
    assert_ne!(left_group, right_group);
    assert_eq!(
        left_proof.selection.graph_selection().namespace,
        right_proof.selection.graph_selection().namespace,
        "the distinct group scopes share one original carrier"
    );
    let view = left
        .merge(&right)
        .unwrap()
        .retain_original_custody(&right)
        .unwrap();
    let baseline = CompilerInputProjection::from_issued_entries(&[native_entry.clone()]).unwrap();
    let context = Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            producer: producer.sha256(),
            inventory: view.clone(),
            compiler_projection: baseline.interface_only(),
            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        },
    ));
    let root = tempfile::tempdir().unwrap();
    for (index, supplied, expected) in [
        (0, vec![left_proof.clone()], left_group),
        (1, vec![left_proof.clone(), left_proof.clone()], left_group),
        (2, Vec::new(), right_group),
    ] {
        let input = OriginalCompilerInputs::issue_complete_native_availability(
            &view,
            &baseline,
            producer.sha256(),
            std::slice::from_ref(consumer),
            &supplied,
        )
        .unwrap();
        let issued = input
            .native_original_requirements
            .iter()
            .find(|proof| proof.original == consumer_entry.descriptor.id)
            .unwrap();
        assert_eq!(
            dependency(issued, &input.artifacts),
            expected,
            "carried availability chooses the private dependency before selected-state fallback"
        );
        assert_eq!(
            input.artifacts.selected_native_groups(),
            view.selected_native_groups(),
            "private availability preserves execution demand"
        );
        let request = ExactCompileContext::new(context.clone())
            .prepare_compilation_with_private_input(
                &root.path().join(index.to_string()),
                PRODUCER,
                None,
                Some(input),
            )
            .unwrap();
        let retained = request
            .compiler_inputs()
            .native_original_requirements
            .iter()
            .find(|proof| proof.original == consumer_entry.descriptor.id)
            .unwrap();
        assert_eq!(
            dependency(retained, &request.compiler_inputs().artifacts),
            expected
        );
        assert_eq!(wire_mode(&request, "PrivateConsumer"), full_mode());
        assert_eq!(wire_mode(&request, "CarriedDependency"), issued_mode(&[7]));
    }
    assert!(matches!(
        OriginalCompilerInputs::issue_complete_native_availability(
            &view,
            &baseline,
            producer.sha256(),
            std::slice::from_ref(consumer),
            &[left_proof, right_proof],
        ),
        Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact }
                if artifact == native_entry.descriptor.id)
    ));
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
                        assert!(rows["AddedNative"].is_empty());
                        assert_eq!(wire_mode(&request, "BaselineNative"), issued_mode(&[7]));
                        assert_eq!(wire_mode(&request, "AddedNative"), full_mode());
                        if explicit_required {
                            assert_eq!(
                                rows["RequiredNative"],
                                selected.into_iter().collect::<Vec<_>>()
                            );
                            assert_eq!(wire_mode(&request, "RequiredNative"), full_mode());
                        } else {
                            assert_eq!(rows["RequiredNative"], vec![demanded]);
                            assert_eq!(
                                wire_mode(&request, "RequiredNative"),
                                issued_mode(&[demanded])
                            );
                        }
                        assert_eq!(rows.contains_key("ExtraNative"), unrelated);
                        assert!(!rows.contains_key("UnusedNative"));
                        assert_eq!(
                            request
                                .compiler_inputs()
                                .metadata
                                .retained
                                .selected_native_groups,
                            before
                                .facts
                                .inventory
                                .metadata_snapshot()
                                .selected_native_groups
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
    assert_eq!(rows["BaselineNative"], vec![7]);
    assert!(rows["UnusedNative"].is_empty());
    assert_eq!(wire_mode(&request, "BaselineNative"), full_mode());
    assert_eq!(wire_mode(&request, "UnusedNative"), full_mode());
    assert_eq!(context.as_ref(), &before);
    assert_eq!(
        request
            .compiler_inputs()
            .metadata
            .retained
            .selected_native_groups,
        before
            .facts
            .inventory
            .metadata_snapshot()
            .selected_native_groups
    );
}

#[test]
fn selected_authored_private_offer_carries_issued_type_only_dependency_groups() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let helper = fixture_finalized_product(
        crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
            "main",
            "TypeOnlyHelper",
            vec![(7, vec![]), (9001, vec![])],
            73,
            &BTreeMap::new(),
            b"TypeOnlyHelper".to_vec(),
            &BTreeMap::new(),
            &[],
        ),
        producer.sha256(),
    );
    let module = tidepool_repr::SessionModule::lib(tidepool_repr::Generation(1)).module_name();
    let authored = crate::certified_products::fixture_native_declaration_product(
        crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
            "main",
            &module,
            vec![(7, vec![source_import(&helper, 7)]), (9001, vec![])],
            74,
            &BTreeMap::new(),
            b"issued authored interface".to_vec(),
            &BTreeMap::new(),
            &[],
        ),
        producer.sha256(),
        1,
    );
    let recovered = recovered_witness_fixtures(&[authored, helper]);
    let entries = recovered
        .iter()
        .map(|row| {
            Arc::new(ArtifactEntry::original(producer.sha256(), row.product.clone()).unwrap())
        })
        .collect::<Vec<_>>();
    let inventory = ArtifactInventory::default();
    let full = inventory
        .admit_shared(&inventory.empty_view(), entries.clone())
        .unwrap();
    let owners = entries
        .iter()
        .map(|entry| entry.descriptor.owner.clone())
        .collect::<Vec<_>>();
    let narrowed = full.interface_projection(&owners).unwrap();
    let projection = CompilerInputProjection::from_interface_view(&narrowed).unwrap();
    let context = Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            producer: producer.sha256(),
            inventory: narrowed,
            compiler_projection: projection,
            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        },
    ));
    let before = context.as_ref().clone();
    assert!(context.facts.inventory.selected_native_groups().is_empty());
    let authored_id = entries
        .iter()
        .find(|entry| entry.descriptor.owner.module == module)
        .unwrap()
        .descriptor
        .id;
    let helper_id = entries
        .iter()
        .find(|entry| entry.descriptor.owner.module == "TypeOnlyHelper")
        .unwrap()
        .descriptor
        .id;
    let canonical = ArtifactEntry::canonical(
        recovered
            .iter()
            .find(|row| row.product.owner().module == module)
            .unwrap()
            .product
            .module_interface()
            .unwrap()
            .clone(),
    )
    .descriptor
    .id;
    let proof = crate::artifact_inventory::NativeOriginalRequirement::from_issued_group_custody(
        authored_id,
        canonical,
        &context.facts.inventory,
    )
    .unwrap()
    .expect("complete authored census was issued before demotion");
    let groups = &proof.selection.graph_selection().native_groups;
    assert!(groups
        .iter()
        .any(|group| group.binding.artifact == helper_id && group.original_ordinal == 7));
    assert!(
        !groups
            .iter()
            .any(|group| group.binding.artifact == helper_id && group.original_ordinal == 9001),
        "unrelated helper body is outside the authored closure"
    );
    let input =
        OriginalCompilerInputs::from_selected_authored_declarations(&context, producer, &[])
            .expect("private offer preserves its already-issued exact dependency closure")
            .expect("authored body is privately reusable");
    let root = tempfile::tempdir().unwrap();
    let request = ExactCompileContext::new(context.clone())
        .prepare_compilation_with_private_input(root.path(), PRODUCER, None, Some(input))
        .unwrap();
    let rows = wire_groups(&request);
    assert_eq!(rows[&module], Vec::<u32>::new());
    assert_eq!(rows["TypeOnlyHelper"], Vec::<u32>::new());
    let fields = scope(&request);
    let native = fields[6].as_array().unwrap();
    let mode = |module: &str| {
        native
            .iter()
            .find(|row| row.as_array().unwrap()[1] == text(module))
            .unwrap()
            .as_array()
            .unwrap()[8]
            .clone()
    };
    assert_eq!(mode(&module), Value::Array(vec![text("full-carrier")]));
    assert_eq!(
        mode("TypeOnlyHelper"),
        Value::Array(vec![
            text("issued-groups"),
            Value::Array(vec![Value::Integer(7.into())])
        ])
    );
    assert!(
        request
            .artifacts
            .iter()
            .find(|artifact| artifact.interface.module == "TypeOnlyHelper")
            .unwrap()
            .product
            .is_none(),
        "transport availability does not upgrade compiler roles"
    );
    assert!(!request.input_transport_files().is_empty());
    let retained = request
        .compiler_inputs()
        .native_original_requirements
        .iter()
        .find(|requirement| requirement.original == authored_id)
        .unwrap();
    assert_eq!(&retained.selection, &proof.selection);
    assert!(request
        .compiler_inputs()
        .metadata
        .retained
        .artifacts
        .contains_key(&helper_id));
    assert!(request
        .compiler_inputs()
        .metadata
        .retained
        .selected_native_groups
        .is_empty());
    let helper_owner = entries
        .iter()
        .find(|entry| entry.descriptor.owner.module == "TypeOnlyHelper")
        .unwrap()
        .descriptor
        .owner
        .clone();
    assert!(
        request
            .compiler_inputs()
            .projection
            .snapshot_roles()
            .get(&helper_owner)
            .unwrap()
            .original()
            .is_none(),
        "dependency body custody does not grant an original namespace role"
    );
    assert_eq!(context.as_ref(), &before);
}

#[test]
fn selected_authored_availability_refuses_source_original_spelling_with_full_groups() {
    let native = product("Tidepool.Session.Lib.G1", 23);
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let mut context = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .extend_checked_original_products(producer.sha256(), std::slice::from_ref(&native))
        .unwrap();
    {
        let updated = context.facts.compiler_projection.interface_only();
        context = context.rebuild(|facts| facts.compiler_projection = updated);
    }
    assert!(context
        .compiler_input_roles()
        .iter()
        .all(|role| role.original().is_none()));
    let keys = context.facts.inventory.selected_native_groups();
    assert_eq!(
        keys.iter()
            .map(|key| key.original_ordinal)
            .collect::<Vec<_>>(),
        vec![7, 9001]
    );
    assert!(
        OriginalCompilerInputs::from_selected_authored_declarations(&context, producer, &[])
            .unwrap()
            .is_none()
    );
    // A separately issued configured offer remains legitimate. Full custody
    // and a session-like module name cannot issue that offer implicitly.
    assert!(OriginalCompilerInputs::from_native_availability(
        &context,
        producer,
        std::slice::from_ref(&native),
    )
    .is_ok());
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
        context.facts.original_instance_environment,
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
    assert!(row[6].as_array().unwrap().is_empty());
    assert_eq!(row[8], full_mode());
    assert!(request.groups.is_empty());
    assert_eq!(
        request
            .compiler_inputs()
            .metadata
            .retained
            .selected_native_groups,
        baseline
            .compiler_inputs()
            .metadata
            .retained
            .selected_native_groups
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
    {
        let updated = changed
            .facts
            .inventory
            .inventory()
            .admit_shared(
                &changed.facts.inventory.inventory().empty_view(),
                vec![Arc::clone(&entry)],
            )
            .unwrap();
        changed = changed.rebuild(|facts| facts.inventory = updated);
    }
    {
        let updated = CompilerInputProjection::from_issued_entries(&[entry]).unwrap();
        changed = changed.rebuild(|facts| facts.compiler_projection = updated);
    }
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
    {
        let updated = ambiguous
            .facts
            .inventory
            .inventory()
            .admit_shared(&ambiguous.facts.inventory, entries.clone())
            .unwrap();
        ambiguous = ambiguous.rebuild(|facts| facts.inventory = updated);
    }
    assert!(ambiguous
        .facts
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
    {
        let updated =
            CompilerInputProjection::from_issued_entries(&[Arc::clone(&entries[0])]).unwrap();
        ambiguous = ambiguous.rebuild(|facts| facts.compiler_projection = updated);
    }
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
fn selected_authored_availability_preserves_exact_native_winner_histories() {
    // This component oracle uses the existing finalized/native proof fixtures.
    // Genuine compiler G1-to-G2 execution is covered by the authored fixture.
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let module = tidepool_repr::SessionModule::lib(tidepool_repr::Generation(1)).module_name();
    for length in 1..=4 {
        let products = (1..=length + 1)
            .map(|version| {
                crate::certified_products::fixture_native_declaration_product(
                    crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
                        "main",
                        &module,
                        vec![(7, vec![]), (9001, vec![])],
                        version,
                        &BTreeMap::new(),
                        b"shared canonical interface".to_vec(),
                        &BTreeMap::new(),
                        &[],
                    ),
                    producer.sha256(),
                    1,
                )
            })
            .collect::<Vec<_>>();
        let products = recovered_witness_fixtures(&products)
            .into_iter()
            .map(|verified| verified.product)
            .collect::<Vec<_>>();
        let current = &products[0];
        assert!(products
            .iter()
            .all(|product| product.module_interface() == current.module_interface()));
        let entries = products
            .iter()
            .map(|product| {
                Arc::new(ArtifactEntry::original(producer.sha256(), product.clone()).unwrap())
            })
            .collect::<Vec<_>>();
        let current_native = entries[0].descriptor.id;
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.descriptor.id)
                .collect::<BTreeSet<_>>()
                .len(),
            products.len()
        );
        let mut context = type_context(std::slice::from_ref(current)).as_ref().clone();
        {
            let updated = context
                .facts
                .inventory
                .inventory()
                .admit_shared_with_demand(
                    &context.facts.inventory,
                    entries.clone(),
                    crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
                )
                .expect("same-canonical body variants retain legal byte custody");
            context = context.rebuild(|facts| facts.inventory = updated);
        }
        context.compiler_metadata_snapshot().unwrap();
        let custody = context.facts.inventory.selected_native_groups();
        assert!(custody.is_empty());
        assert!(
            OriginalCompilerInputs::from_selected_authored_declarations(&context, producer, &[],)
                .unwrap()
                .is_none(),
            "body custody alone does not grant a selected authored root"
        );
        assert!(
            OriginalCompilerInputs::from_native_availability(
                &context,
                producer,
                std::slice::from_ref(current),
            )
            .is_err(),
            "a canonical-only role cannot choose among retained native bodies"
        );
        let mut unissued = context.clone();
        {
            let updated = CompilerInputProjection::default();
            unissued = unissued.rebuild(|facts| facts.compiler_projection = updated);
        }
        assert!(
            OriginalCompilerInputs::from_native_availability(
                &unissued,
                producer,
                std::slice::from_ref(current),
            )
            .is_err(),
            "absence of an issuing role preserves ambiguous custody refusal"
        );

        {
            let updated =
                CompilerInputProjection::from_issued_entries(&[Arc::clone(&entries[0])]).unwrap();
            context = context.rebuild(|facts| facts.compiler_projection = updated);
        }
        context.compiler_metadata_snapshot().unwrap();
        let private = OriginalCompilerInputs::from_native_availability(
            &context,
            producer,
            std::slice::from_ref(current),
        )
        .expect("an exact issued native winner distinguishes same-canonical bodies");
        assert!(
            OriginalCompilerInputs::from_native_availability(
                &context,
                producer,
                std::slice::from_ref(&products[1]),
            )
            .is_err(),
            "a canonical-equal competing body cannot replace the native winner"
        );
        let root = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(Arc::new(context.clone()))
            .prepare_compilation_with_private_input(root.path(), PRODUCER, None, Some(private))
            .unwrap();
        assert_eq!(
            request.compiler_original_products().unwrap(),
            vec![current.clone()]
        );
        assert_eq!(
            request
                .compiler_inputs()
                .metadata
                .retained
                .selected_native_groups,
            custody
        );
        let wire = scope(&request);
        let rows = wire[6].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            &rows[0].as_array().unwrap()[..5],
            &[
                text(&current.owner().unit),
                text(&current.owner().module),
                text(hex(&current.owner().module_version.0)),
                text(hex(&current.owner().skinny_iface_sha256)),
                text(hex(&current.owner().product_sha256)),
            ]
        );

        // The inventory issuer explicitly selects complete original censuses;
        // the compiler issuer still names exactly one of those native bodies.
        {
            let updated = context
                .facts
                .inventory
                .inventory()
                .admit_shared(&context.facts.inventory, entries)
                .unwrap();
            context = context.rebuild(|facts| facts.inventory = updated);
        }
        context.compiler_metadata_snapshot().unwrap();
        let groups = context.facts.inventory.selected_native_groups();
        assert_eq!(groups.len(), products.len() * 2);
        let dependencies = context.facts.inventory.dependencies();
        let private =
            OriginalCompilerInputs::from_selected_authored_declarations(&context, producer, &[])
                .unwrap()
                .expect("fully selected current authored body has an exact issuer");
        assert_eq!(
            private
                .projection
                .roles()
                .iter()
                .filter_map(|role| role.original())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([current_native])
        );
        let offer = crate::artifacts::ModuleCandidateOffer::select_in_context(
            PRODUCER,
            &[],
            &root.path().join("selected-exact-offer"),
            Arc::new(ExactCompileContext::new(Arc::new(context.clone()))),
        )
        .unwrap();
        let wire: Value = ciborium::de::from_reader(
            std::fs::read(offer.exact_scope_path().unwrap())
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        let rows = wire.as_array().unwrap()[6].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            &rows[0].as_array().unwrap()[..5],
            &[
                text(&current.owner().unit),
                text(&current.owner().module),
                text(hex(&current.owner().module_version.0)),
                text(hex(&current.owner().skinny_iface_sha256)),
                text(hex(&current.owner().product_sha256)),
            ]
        );
        assert_eq!(context.facts.inventory.selected_native_groups(), groups);
        assert_eq!(context.facts.inventory.dependencies(), dependencies);
        let mut canonical_only = context.clone();
        {
            let updated = canonical_only.facts.compiler_projection.interface_only();
            canonical_only = canonical_only.rebuild(|facts| facts.compiler_projection = updated);
        }
        assert!(
            OriginalCompilerInputs::from_selected_authored_declarations(
                &canonical_only,
                producer,
                &[],
            )
            .is_err(),
            "full body selections cannot choose an ambiguous canonical-only namespace"
        );
        let different = fixture_finalized_product(
            crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
                "main",
                &module,
                vec![(7, vec![]), (9001, vec![])],
                240,
                &BTreeMap::new(),
                b"different canonical interface".to_vec(),
                &BTreeMap::new(),
                &[],
            ),
            producer.sha256(),
        );
        let different = recovered_witness_fixtures(&[different]).remove(0).product;
        assert_ne!(different.module_interface(), current.module_interface());
        let different = Arc::new(ArtifactEntry::original(producer.sha256(), different).unwrap());
        let error = context
            .facts
            .inventory
            .inventory()
            .admit_shared_with_demand(
                &context.facts.inventory,
                vec![different],
                crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap_err();
        assert!(
            matches!(
                error,
                CompileError::ArtifactInventory(
                    crate::artifact_inventory::ArtifactInventoryError {
                        failure:
                            crate::artifact_inventory::ArtifactInventoryFailure::OwnerConflict { .. },
                        ..
                    }
                )
            ),
            "a differing canonical carrier cannot join this reachable owner namespace"
        );
        assert_eq!(context.facts.inventory.selected_native_groups(), groups);
        assert_eq!(context.facts.inventory.dependencies(), dependencies);
    }
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

// The existing native-origin fixture supplies genuine certificates for narrowed
// selections and the authenticated G2-to-G1 native dependency.
pub(crate) fn assert_selected_authored_private_inputs(
    certificate: &Arc<CertifiedAuthoredDeclaration>,
    dependent: &CertifiedAuthoredDeclaration,
    generation: u64,
    root: &Path,
    producer_bytes: &[u8],
) {
    let owner = certificate.product().owner();
    let mut native_context = ExactDeclarationContext::new(
        std::slice::from_ref(certificate),
        &[],
        vec![ExactLexicalNode {
            owner: identity(&owner.unit, &owner.module),
            imports: Vec::new(),
        }],
    )
    .unwrap();
    let projection = native_context.facts.compiler_projection.interface_only();
    native_context = native_context.rebuild(|facts| facts.compiler_projection = projection);
    let producer = CanonicalProducerIdentity::from_producer_bytes(producer_bytes);
    let authored_root = native_context.authored_native_root(generation).unwrap();
    let selected_before = native_context.facts.inventory.selected_native_groups();
    let assert_exact_offer = |context: &ExactDeclarationContext, name: &str, expected: bool| {
        let before = context.facts.inventory.selected_native_groups();
        let offer = crate::artifacts::ModuleCandidateOffer::select_in_context(
            producer_bytes,
            &[root.to_path_buf()],
            &root.join(name),
            Arc::new(ExactCompileContext::new(Arc::new(context.clone()))),
        )
        .expect("actual exact offer preserves the issued canonical context");
        let scope: Value = ciborium::de::from_reader(
            std::fs::read(offer.exact_scope_path().unwrap())
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        let owner = certificate.product().owner();
        let originals = scope.as_array().unwrap()[6].as_array().unwrap();
        let offered = originals
            .iter()
            .map(|row| row.as_array().unwrap())
            .filter(|row| row[0] == text(&owner.unit) && row[1] == text(&owner.module))
            .collect::<Vec<_>>();
        if expected {
            let [row] = offered.as_slice() else {
                panic!("complete selected authored original must have one private offer");
            };
            assert_eq!(
                &row[..5],
                &[
                    text(&owner.unit),
                    text(&owner.module),
                    text(hex(&owner.module_version.0)),
                    text(hex(&owner.skinny_iface_sha256)),
                    text(hex(&owner.product_sha256)),
                ]
            );
        } else {
            assert!(
                offered.is_empty(),
                "custody or partial demand grants no full original offer"
            );
        }
        assert_eq!(context.facts.inventory.selected_native_groups(), before);
    };
    let authored_groups = selected_before
        .iter()
        .filter(|group| group.artifact == authored_root)
        .copied()
        .collect::<BTreeSet<_>>();
    assert!(
        authored_groups.len() >= 2,
        "the real fixture must issue independent original groups"
    );
    assert!(matches!(
        native_context.compiler_metadata_snapshot().unwrap().entries[&identity(
            &certificate.product().owner().unit,
            &certificate.product().owner().module
        )]
            .payload,
        ArtifactPayload::Canonical(_)
    ));
    let native =
        OriginalCompilerInputs::from_selected_authored_declarations(&native_context, producer, &[])
            .unwrap()
            .expect("actual authored selection supplies private native availability");
    let native_request = ExactCompileContext::new(Arc::new(native_context.clone()))
        .prepare_compilation_with_private_input(
            &root.join("selected-authored-scope"),
            producer_bytes,
            None,
            Some(native),
        )
        .unwrap();
    assert!(native_request
        .compiler_original_products()
        .unwrap()
        .iter()
        .any(|product| product.owner() == certificate.product().owner()));
    assert_eq!(native_request.context().as_ref(), &native_context);
    assert_eq!(
        native_request
            .compiler_inputs()
            .metadata
            .retained
            .selected_native_groups,
        selected_before
    );
    assert_exact_offer(&native_context, "full-authored-exact-offer", true);
    // Source-role demotion preserves the issuer's complete native group graph
    // as custody. A later private offer must keep that graph without restoring
    // lexical visibility or selecting executable groups in the captured context.
    let mut captured = native_context.clone();
    let interface_roles = captured.facts.compiler_projection.interface_only();
    let (custody, projection) = captured
        .facts
        .inventory
        .inventory()
        .empty_view()
        .merge_compiler_inputs(
            &CompilerInputProjection::default(),
            &captured.facts.inventory,
            &interface_roles,
        )
        .unwrap();
    captured = captured.rebuild(|facts| {
        facts.inventory = custody;
        facts.compiler_projection = projection;
    });
    assert!(captured.facts.inventory.selected_native_groups().is_empty());
    assert!(captured
        .compiler_input_roles()
        .iter()
        .all(|role| role.original().is_none()));
    let before_custody = captured.clone();
    let private =
        OriginalCompilerInputs::from_selected_authored_declarations(&captured, producer, &[])
            .unwrap()
            .expect("complete issued authored group custody keeps private availability");
    let captured_request = ExactCompileContext::new(Arc::new(captured.clone()))
        .prepare_compilation_with_private_input(
            &root.join("custodied-authored-scope"),
            producer_bytes,
            None,
            Some(private),
        )
        .unwrap();
    assert!(captured_request
        .compiler_original_products()
        .unwrap()
        .iter()
        .any(|product| product.owner() == certificate.product().owner()));
    assert_eq!(captured_request.context().as_ref(), &before_custody);
    assert!(captured_request
        .compiler_inputs()
        .metadata
        .retained
        .selected_native_groups
        .is_empty());
    assert_exact_offer(&captured, "custodied-authored-exact-offer", true);
    assert_eq!(captured, before_custody);
    // Fresh request-type authentication extends only interfaces. The issued
    // parent's complete body graph must survive that handoff as private custody.
    let fresh = ExactDeclarationContext::new(&[], &[], Vec::new())
        .unwrap()
        .extend_interface_context(&native_context)
        .unwrap();
    assert!(fresh.facts.inventory.selected_native_groups().is_empty());
    assert!(fresh
        .compiler_input_roles()
        .iter()
        .all(|role| role.original().is_none()));
    assert_exact_offer(&fresh, "fresh-request-type-authored-offer", true);
    // Reuse the genuine certificate and independently remove execution
    // roots. Neither type custody nor an incomplete group selection can
    // supply the complete authored original to another compiler request.
    let mut valid_sparse_refusals = 0;
    for (selection_index, omitted) in std::iter::once(None)
        .chain(authored_groups.iter().map(Some))
        .enumerate()
    {
        let selected = match omitted {
            None => BTreeSet::new(),
            Some(omitted) => selected_before
                .iter()
                .filter(|group| *group != omitted)
                .copied()
                .collect(),
        };
        let mut partial = native_context.clone();
        let view = partial
            .facts
            .inventory
            .inventory()
            .admit_recovery_selection(
                &partial.facts.inventory.inventory().empty_view(),
                partial.facts.inventory.entries(),
                &selected,
            );
        let Ok(view) = view else {
            assert!(
                omitted.is_some(),
                "removing every native root must retain valid type custody"
            );
            // Removing a required dependency is refused by exact closure
            // admission before private compiler input can be constructed.
            continue;
        };
        {
            let updated = view;
            partial = partial.rebuild(|facts| facts.inventory = updated);
        }
        assert_eq!(partial.facts.inventory.selected_native_groups(), selected);
        let private =
            OriginalCompilerInputs::from_selected_authored_declarations(&partial, producer, &[])
                .unwrap();
        assert!(private.as_ref().is_none_or(|private| private
            .projection
            .roles()
            .iter()
            .all(|role| role.original() != Some(authored_root))));
        assert_exact_offer(
            &partial,
            &format!("partial-authored-exact-offer-{selection_index}"),
            false,
        );
        if omitted.is_some()
            && partial
                .facts
                .inventory
                .selected_native_groups()
                .iter()
                .any(|group| group.artifact == authored_root)
        {
            valid_sparse_refusals += 1;
        }
    }
    assert!(
        valid_sparse_refusals > 0,
        "a valid nonempty sparse selection must reach private offer refusal"
    );

    let dependent_native = ArtifactEntry::original(producer.sha256(), dependent.product().clone())
        .unwrap()
        .descriptor
        .id;
    assert!(
        dependent
            .artifact_view()
            .dependencies()
            .iter()
            .any(|(from, to, edge)| {
                *from == dependent_native
                    && *to == authored_root
                    && matches!(
                        edge,
                        crate::artifact_inventory::ArtifactDependency::NativeGroup { .. }
                    )
            }),
        "the historical child must be a compiler-authenticated exact dependency"
    );
}

#[test]
fn independent_original_without_issued_groups_keeps_empty_availability() {
    let native = product("UnavailableOriginal", 83);
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), native).unwrap());
    let inventory = ArtifactInventory::default();
    let artifacts = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            vec![entry.clone()],
            crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    assert!(artifacts.selected_native_groups().is_empty());
    let context = Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            producer: producer.sha256(),
            inventory: artifacts,
            compiler_projection: CompilerInputProjection::from_issued_entries(&[entry]).unwrap(),
            lexical: Default::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        },
    ));
    let root = tempfile::tempdir().unwrap();
    let request = ExactCompileContext::new(context)
        .prepare_compilation(root.path(), PRODUCER)
        .unwrap();
    assert_eq!(wire_mode(&request, "UnavailableOriginal"), issued_mode(&[]));
    assert!(wire_groups(&request)["UnavailableOriginal"].is_empty());
    assert!(request.groups.is_empty());
}

#[test]
fn dependency_execution_graph_roots_stay_private_and_malformed_roots_refuse() {
    let native = product("DependencyGraph", 84);
    let owner = native.owner();
    let root = Value::Array(vec![
        text(&owner.unit),
        text(&owner.module),
        text(hex(&owner.module_version.0)),
        text(hex(&owner.skinny_iface_sha256)),
        text(hex(&owner.product_sha256)),
        text(hex(&[7; 32])),
    ]);
    let scope = Value::Array(vec![Value::Array(vec![]), Value::Array(vec![root.clone()])]);
    let context = type_context(std::slice::from_ref(&native));
    let metadata = context.compiler_metadata_snapshot().unwrap();
    assert!(compiler_execution_scope(&scope, &metadata)
        .unwrap()
        .as_array()
        .unwrap()[1]
        .as_array()
        .unwrap()
        .is_empty());
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), native).unwrap());
    let inventory = ArtifactInventory::default();
    let full = inventory
        .admit_shared(&inventory.empty_view(), vec![entry])
        .unwrap();
    let compiler_metadata = CompilerInputProjection::from_issued_entries(&full.entries())
        .unwrap()
        .project_metadata(full.metadata_snapshot())
        .unwrap();
    assert_eq!(
        compiler_execution_scope(&scope, &compiler_metadata).unwrap(),
        scope
    );
    for malformed in [
        Value::Array(vec![]),
        Value::Array(vec![text("short")]),
        Value::Array(vec![Value::Integer(1.into()); 6]),
    ] {
        let scope = Value::Array(vec![Value::Array(vec![]), Value::Array(vec![malformed])]);
        assert!(compiler_execution_scope(&scope, &metadata).is_err());
    }
    let malformed = Value::Array(vec![
        Value::Array(vec![text("not a graph row")]),
        Value::Array(vec![root]),
    ]);
    assert!(compiler_execution_scope(&malformed, &metadata).is_err());
}

#[test]
fn private_transport_preserves_independent_closures_until_exact_demand() {
    use crate::artifact_inventory::{
        ExactArtifactSelection, NativeArtifactDemand, NativeOriginalRequirement,
    };

    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let root = tempfile::tempdir().unwrap();
    let mut histories = 0;
    for same_root in [false, true] {
        for overlap in [false, true] {
            if same_root && !overlap {
                continue;
            }
            let dependency = product("OverlappingDependency", 80);
            let left = dependent_product(
                "IndependentLeft",
                81,
                vec![(7, vec![source_import(&dependency, 7)])],
            );
            let right = dependent_product(
                "IndependentRight",
                82,
                vec![(
                    7,
                    vec![source_import(&dependency, if overlap { 7 } else { 9001 })],
                )],
            );
            let recovered = recovered_witness_fixtures(&[dependency, left, right]);
            let get = |module: &str| {
                recovered
                    .iter()
                    .find(|row| row.product.owner().module == module)
                    .unwrap()
                    .product
                    .clone()
            };
            let products = [
                get("OverlappingDependency"),
                get("IndependentLeft"),
                get(if same_root {
                    "IndependentLeft"
                } else {
                    "IndependentRight"
                }),
            ];
            let entries = products.each_ref().map(|product| {
                Arc::new(ArtifactEntry::original(producer.sha256(), product.clone()).unwrap())
            });
            let interfaces = CompilerInputProjection::from_issued_entries(&entries)
                .unwrap()
                .interface_only();
            for shared_carrier in [false, true] {
                let inventory = ArtifactInventory::default();
                let base_entries = if shared_carrier {
                    entries.to_vec()
                } else {
                    products
                        .iter()
                        .map(|product| {
                            Arc::new(ArtifactEntry::canonical(
                                product.module_interface().unwrap().clone(),
                            ))
                        })
                        .collect()
                };
                let base = inventory
                    .admit_shared_with_demand(
                        &inventory.empty_view(),
                        base_entries,
                        NativeArtifactDemand::ScopeInterfaces,
                    )
                    .unwrap();
                // Extra canonical owners distinguish the issuance scopes even
                // when both sides offer exactly the same original root.
                let issue = |index: usize, marker: &str, version| {
                    let marker = product(marker, version);
                    inventory
                        .admit_shared_with_demand(
                            &base,
                            vec![
                                entries[0].clone(),
                                entries[index].clone(),
                                Arc::new(ArtifactEntry::canonical(
                                    marker.module_interface().unwrap().clone(),
                                )),
                            ],
                            NativeArtifactDemand::CertifiedTargetImports(&[source_import(
                                &products[index],
                                7,
                            )]),
                        )
                        .unwrap()
                };
                let sources = [issue(1, "LeftScope", 83), issue(2, "RightScope", 84)];
                let proofs = std::array::from_fn::<_, 2, _>(|index| {
                    let canonical = ArtifactEntry::canonical(
                        products[index + 1].module_interface().unwrap().clone(),
                    );
                    NativeOriginalRequirement::from_issued_group_custody(
                        entries[index + 1].descriptor.id,
                        canonical.descriptor.id,
                        &sources[index],
                    )
                    .unwrap()
                    .unwrap()
                });
                let expected = std::array::from_fn::<_, 2, _>(|index| {
                    proofs[index]
                        .issued_group_carriers(&sources[index])
                        .unwrap()
                });
                let child = |index: usize| {
                    expected[index]
                        .iter()
                        .find(|(group, _)| group.binding.artifact == entries[0].descriptor.id)
                        .map(|(group, carrier)| (*group, *carrier))
                        .unwrap()
                };
                assert_eq!(child(0).1 == child(1).1, shared_carrier);
                if overlap {
                    assert_eq!(child(0).0.original_ordinal, child(1).0.original_ordinal);
                    assert_ne!(child(0).0, child(1).0);
                }
                let context = |view: &ArtifactView| {
                    let view = inventory
                        .empty_view()
                        .merge_compiler_inputs(
                            &CompilerInputProjection::default(),
                            view,
                            &interfaces,
                        )
                        .unwrap()
                        .0;
                    Arc::new(ExactDeclarationContext::from_facts(
                        DeclarationContextFacts {
                            producer: producer.sha256(),
                            inventory: view,
                            compiler_projection: interfaces.clone(),
                            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
                            template_imports: None,
                            original_instance_environment: OriginalInstanceEnvironment::Unknown,
                        },
                    ))
                };
                let private = std::array::from_fn::<_, 2, _>(|index| {
                    OriginalCompilerInputs::from_native_availability_with_requirements(
                        &context(&sources[index]),
                        producer,
                        std::slice::from_ref(&products[index + 1]),
                        std::slice::from_ref(&proofs[index]),
                    )
                    .unwrap()
                });
                let base_context = context(&base);
                for reverse in [false, true] {
                    for duplicate in [false, true] {
                        histories += 1;
                        let first = usize::from(reverse);
                        let mut joined = private[first].merge(&private[1 - first]).unwrap();
                        if duplicate {
                            joined = joined.merge(&private[first]).unwrap();
                        }
                        let request = ExactCompileContext::new(base_context.clone())
                            .prepare_compilation_with_private_input(
                                &root.path().join(histories.to_string()),
                                PRODUCER,
                                None,
                                Some(joined),
                            )
                            .unwrap();
                        let inputs = request.compiler_inputs();
                        assert!(inputs.metadata.retained.selected_native_groups.is_empty());
                        let roles = inputs.projection.snapshot_roles();
                        let role = &roles[&entries[0].descriptor.owner];
                        assert!(
                            role.original().is_none(),
                            "a dependency receives no native role"
                        );
                        assert_eq!(
                            wire_mode(&request, "OverlappingDependency"),
                            issued_mode(if overlap { &[7] } else { &[7, 9001] }),
                            "a union of issued dependency groups never issues Full"
                        );
                        assert!(wire_groups(&request).values().all(Vec::is_empty));
                        for index in 0..2 {
                            assert!(inputs.native_original_requirements.iter().any(|proof| {
                                proof.original == proofs[index].original
                                    && proof.issued_group_carriers(&inputs.artifacts).unwrap()
                                        == expected[index]
                            }));
                        }
                        let validation = PackageInterfaceValidation::default();
                        let selected = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
                            inputs,
                            &inputs.metadata,
                            &validation.inventory,
                        )
                        .unwrap();
                        let bind = || {
                            inputs.artifacts.inventory().bind_segment(
                                &inputs.artifacts,
                                vec![],
                                &selected,
                            )
                        };
                        for index in 0..2 {
                            let mut fixed = bind().unwrap();
                            let imports = [source_import(&products[index + 1], 7)];
                            let admitted = inventory.admit_bound_target(
                                &mut fixed,
                                &inputs.artifacts,
                                NativeArtifactDemand::CertifiedTargetImports(&imports),
                                vec![],
                                &selected,
                            );
                            if same_root && shared_carrier {
                                assert!(
                                    matches!(admitted, Err(CompileError::ArtifactInventory(error))
                                    if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact }
                                        if artifact == entries[1].descriptor.id))
                                );
                            } else {
                                let admitted = admitted.unwrap();
                                let groups = admitted
                                    .selection
                                    .groups_for_target(&admitted.artifacts, None, &imports)
                                    .unwrap();
                                let expected_index = if same_root {
                                    let graph = inputs.artifacts.capture_graph_selection();
                                    let carrier = graph
                                        .namespace
                                        .iter()
                                        .find(|carrier| {
                                            carrier.artifact == entries[1].descriptor.id
                                        })
                                        .unwrap();
                                    expected
                                        .iter()
                                        .position(|groups| {
                                            groups.iter().any(|(group, owner)| {
                                                group.binding.artifact == entries[1].descriptor.id
                                                    && owner == carrier
                                            })
                                        })
                                        .unwrap()
                                } else {
                                    index
                                };
                                assert_eq!(
                                    groups,
                                    expected[expected_index].keys().copied().collect()
                                );
                            }
                        }
                        let mut fixed = bind().unwrap();
                        let imports = [source_import(&products[0], 7)];
                        let demanded = inventory.admit_bound_target(
                            &mut fixed,
                            &inputs.artifacts,
                            NativeArtifactDemand::CertifiedTargetImports(&imports),
                            vec![],
                            &selected,
                        );
                        if overlap {
                            assert!(
                                matches!(demanded, Err(CompileError::ArtifactInventory(error))
                                if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact }
                                    if artifact == entries[0].descriptor.id))
                            );
                        } else {
                            let demanded = demanded.unwrap();
                            let groups = demanded
                                .selection
                                .groups_for_target(&demanded.artifacts, None, &imports)
                                .unwrap();
                            assert!(groups.contains(&child(0).0));
                            assert!(!groups.contains(&child(1).0));
                        }
                        let handoff =
                            OriginalCompilerInputs::from_selection(&selected, &inputs.artifacts)
                                .unwrap();
                        let next = ExactCompileContext::new(base_context.clone())
                            .prepare_compilation_with_private_input(
                                &root.path().join(format!("handoff-{histories}")),
                                PRODUCER,
                                None,
                                Some(handoff),
                            )
                            .unwrap();
                        let encoded = serde_json::to_vec(
                            &next.compiler_inputs().artifacts.capture_graph_selection(),
                        )
                        .unwrap();
                        let decoded = serde_json::from_slice(&encoded).unwrap();
                        let restored = next
                            .compiler_inputs()
                            .artifacts
                            .select_issued(
                                vec![],
                                &ExactArtifactSelection::from_recovered_graph_selection(decoded),
                            )
                            .unwrap();
                        for index in 0..2 {
                            assert_eq!(
                                proofs[index].issued_group_carriers(&restored).unwrap(),
                                expected[index]
                            );
                        }
                    }
                }
            }
        }
    }
    assert_eq!(histories, 24);
}
