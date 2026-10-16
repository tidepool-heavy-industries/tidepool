use super::*;
use crate::certified_products::tests::{
    original_groups_fixture_with_interface, recovered_witness_fixtures,
};
use crate::certified_products::{fixture_finalized_product, fixture_module_core};

// Fixtures explicitly issue availability at their input boundary, before any
// compiler request consumes or combines their custody.
fn fixture_compiler_support(
    projection: CompilerInputProjection,
    artifacts: ArtifactView,
) -> crate::artifact_inventory::CompilerSourceSupport {
    let offers = crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(
        &projection,
        &artifacts,
        &[],
    )
    .unwrap();
    crate::artifact_inventory::CompilerSourceSupport::new(projection, artifacts, offers).unwrap()
}

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
            compiler_support: fixture_compiler_support(projection, view),
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
    let product = crate::certified_products::finalize_original_for_inventory(
        product.clone(),
        &mut PackageInterfaceValidation::default(),
    )
    .expect("fixture native import has authenticated original groups");
    let definitions =
        crate::certified_products::original_native_group_definitions(&product, ordinal)
            .expect("fixture native import names an issued original group");
    let binding = match &definitions.bindings()[0] {
        tidepool_repr::execution_schema::Group::NonRecursive(binding) => binding,
        tidepool_repr::execution_schema::Group::Recursive(bindings) => &bindings[0],
    };
    let binder = binding.identity.clone();
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
            compiler_support: fixture_compiler_support(projection, view),
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
    let make = |module: &str, version, selected: &BTreeSet<NativeGroupKey>| {
        let marker = product(module, version);
        inventory
            .admit_recovery_selection(
                &base,
                vec![Arc::new(ArtifactEntry::canonical(
                    marker.module_interface().unwrap().clone(),
                ))],
                selected,
            )
            .unwrap()
    };
    let left = make("LeftIssuance", 77, &groups);
    let right = make("RightIssuance", 78, &groups);
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
                group.binding.artifact() == native_entry.descriptor.id
                    && group.original_ordinal == 7
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
    assert!(left_proof.qualify_availability(&view).unwrap().is_some());
    let selected_proof =
        NativeOriginalRequirement::from_selected_view(native_entry.descriptor.id, &left)
            .unwrap()
            .unwrap();
    assert!(selected_proof
        .qualify_availability(&view)
        .unwrap()
        .is_some());
    let unambiguous = crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(
        &baseline,
        &left,
        std::slice::from_ref(&selected_proof),
    )
    .unwrap();
    assert_eq!(unambiguous.len(), 1);
    assert!(
        unambiguous[0].is_full(),
        "a complete selected-root proof qualifies through its own carrier edges"
    );
    assert_eq!(unambiguous[0].proof(), Some(&selected_proof));
    let offers = crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(
        &baseline,
        &view,
        std::slice::from_ref(&left_proof),
    )
    .expect("a supplied qualified proof must not be recaptured from broader custody");
    assert_eq!(offers.len(), 1);
    let retained = &offers[0];
    assert!(retained.is_full());
    assert_eq!(retained.original(), native_entry.descriptor.id);
    assert_eq!(retained.proof(), Some(&left_proof));
    assert_eq!(dependency(retained.proof().unwrap(), &view), left_group);
    let partial_views = [7, 9001].map(|ordinal| {
        make(
            &format!("PartialIssuance{ordinal}"),
            79,
            &BTreeSet::from([NativeGroupKey {
                artifact: native_entry.descriptor.id,
                original_ordinal: ordinal,
            }]),
        )
    });
    let partial_proofs = partial_views.each_ref().map(|view| {
        NativeOriginalRequirement::from_selected_view(native_entry.descriptor.id, view)
            .unwrap()
            .unwrap()
    });
    let broad = partial_views
        .iter()
        .try_fold(view.clone(), |view, partial| view.merge(partial))
        .unwrap()
        .retain_original_custody(&view)
        .unwrap();
    for (proof, ordinal) in partial_proofs.iter().zip([7, 9001]) {
        assert!(proof.qualify_availability(&broad).unwrap().is_none());
        let issued = proof.issued_groups(&broad).unwrap();
        assert_eq!(issued.len(), 1);
        assert_eq!(issued.first().unwrap().original_ordinal, ordinal);
    }
    for supplied in [
        vec![partial_proofs[0].clone()],
        vec![left_proof.clone(), partial_proofs[0].clone()],
        vec![partial_proofs[0].clone(), left_proof.clone()],
        vec![
            left_proof.clone(),
            partial_proofs[0].clone(),
            left_proof.clone(),
        ],
        vec![left_proof.clone(), right_proof.clone()],
        vec![right_proof.clone(), left_proof.clone()],
        partial_proofs.to_vec(),
        vec![partial_proofs[1].clone(), partial_proofs[0].clone()],
    ] {
        let offers = crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(
            &baseline, &broad, &supplied,
        )
        .unwrap();
        assert_eq!(offers.len(), supplied.len());
        for (offer, proof) in offers.iter().zip(&supplied) {
            assert_eq!(offer.original(), native_entry.descriptor.id);
            assert_eq!(offer.proof(), Some(proof));
            assert_eq!(
                offer.is_full(),
                proof == &left_proof || proof == &right_proof
            );
        }
    }
    let competing_roots = left.merge(&right).unwrap();
    let competing =
        NativeOriginalRequirement::from_selected_view(native_entry.descriptor.id, &competing_roots)
            .unwrap()
            .unwrap();
    assert_eq!(competing.issued_groups(&broad).unwrap().len(), 4);
    assert!(competing.qualify_availability(&broad).unwrap().is_none());
    assert!(matches!(
        crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(&baseline, &broad, std::slice::from_ref(&competing))
            .unwrap().as_slice(),
        [offer] if !offer.is_full() && offer.proof() == Some(&competing)
    ));
    let marker = product("DifferentCarrier", 80);
    let other = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            vec![
                native_entry.clone(),
                Arc::new(ArtifactEntry::canonical(
                    marker.module_interface().unwrap().clone(),
                )),
            ],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let binding = |view: &ArtifactView, id| {
        crate::artifact_inventory::ExactArtifactSelection::capture(view)
            .graph_selection()
            .namespace
            .iter()
            .find(|binding| binding.artifact == id)
            .copied()
            .unwrap()
    };
    for id in [native_entry.descriptor.id, canonical.descriptor.id] {
        assert_ne!(binding(&left, id), binding(&other, id));
    }
    let other_interface = other
        .interface_projection(std::slice::from_ref(&canonical.descriptor.owner))
        .unwrap();
    for selected in [&other, &other_interface] {
        let mismatch = left
            .merge(selected)
            .unwrap()
            .retain_original_custody(selected)
            .unwrap();
        assert!(left_proof.issued_groups(&mismatch).is_ok());
        assert!(left_proof
            .qualify_availability(&mismatch)
            .unwrap()
            .is_none());
        let offers = crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(
            &baseline,
            &mismatch,
            std::slice::from_ref(&left_proof),
        )
        .unwrap();
        assert!(matches!(
            offers.as_slice(),
            [offer] if !offer.is_full() && offer.proof() == Some(&left_proof)
        ));
        let full = crate::artifact_inventory::IssuedNativeOffer::issue(
            native_entry.descriptor.id,
            &left,
            Some(left_proof.clone()),
        )
        .unwrap();
        let narrowed = full
            .within_projection(&baseline, &mismatch)
            .unwrap()
            .unwrap();
        assert!(!narrowed.is_full());
        assert_eq!(narrowed.proof(), Some(&left_proof));
        assert!(
            !narrowed
                .within_projection(&baseline, &left)
                .unwrap()
                .unwrap()
                .is_full(),
            "propagation cannot promote a narrowed offer by rediscovery"
        );
    }
    let mut malformed = left_proof.clone();
    let mut graph = malformed.selection.graph_selection().clone();
    graph.bindings.push(graph.bindings[0]);
    malformed.selection =
        crate::artifact_inventory::ExactArtifactSelection::from_recovered_graph_selection(graph);
    for supplied in [
        vec![left_proof.clone(), malformed.clone()],
        vec![malformed, left_proof.clone()],
    ] {
        assert!(matches!(
            crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(
                &baseline, &broad, &supplied
            ),
            Err(CompileError::ExtractFailed(_))
        ));
    }
    let context = Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: fixture_compiler_support(baseline.interface_only(), view.clone()),
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
            .native_original_requirements()
            .iter()
            .find(|proof| proof.original == consumer_entry.descriptor.id)
            .cloned()
            .unwrap();
        assert_eq!(
            dependency(&issued, &input.artifacts),
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
            .native_original_requirements()
            .iter()
            .find(|proof| proof.original == consumer_entry.descriptor.id)
            .cloned()
            .unwrap();
        assert_eq!(
            dependency(&retained, &request.compiler_inputs().artifacts),
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
                                .compiler_support
                                .artifacts
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
fn original_receipt_retains_transitive_support_without_unrelated_offer() {
    use crate::artifact_inventory::{
        IssuedNativeOffer, NativeArtifactDemand, NativeOriginalRequirement, NativeRequirementRoot,
    };

    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let dependency = product("ReceiptDependency", 91);
    let ancestor = dependent_product(
        "ReceiptAncestor",
        92,
        vec![(7, vec![source_import(&dependency, 7)])],
    );
    let unrelated = product("ReceiptUnrelated", 93);
    let recovered = recovered_witness_fixtures(&[ancestor, dependency, unrelated]);
    let entries = recovered
        .iter()
        .map(|row| {
            Arc::new(ArtifactEntry::original(producer.sha256(), row.product.clone()).unwrap())
        })
        .collect::<Vec<_>>();
    let inventory = ArtifactInventory::default();
    let base = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            entries.clone(),
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let full = inventory.admit_shared(&base, entries.clone()).unwrap();
    let complete = |index: usize| {
        let canonical =
            ArtifactEntry::canonical(recovered[index].product.module_interface().unwrap().clone())
                .descriptor
                .id;
        let proof = NativeOriginalRequirement::from_issued_group_custody(
            entries[index].descriptor.id,
            canonical,
            &full,
        )
        .unwrap()
        .unwrap();
        IssuedNativeOffer::issue(entries[index].descriptor.id, &full, Some(proof)).unwrap()
    };
    let ancestor_offer = complete(0);
    let unrelated_offer = complete(2);
    let dependency_offer = IssuedNativeOffer::issue(entries[1].descriptor.id, &base, None).unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
    let metadata = projection
        .project_metadata(base.metadata_snapshot())
        .unwrap();
    let transport = NativeCompilerTransport::issue(
        &metadata,
        &full,
        &[
            ancestor_offer.clone(),
            dependency_offer,
            unrelated_offer.clone(),
        ],
    )
    .unwrap();
    let receipt = transport.availability[&entries[1].descriptor.id].clone();
    assert_eq!(
        receipt.offers().len(),
        2,
        "B retains its empty offer and the actual A proof contributing B's group"
    );
    assert!(receipt
        .offers()
        .iter()
        .any(|offer| offer.original() == entries[0].descriptor.id));
    assert!(receipt
        .offers()
        .iter()
        .all(|offer| offer.original() != entries[2].descriptor.id));
    assert!(receipt.permits(7));
    assert!(!receipt.permits(9001));
    assert!(matches!(receipt.mode, NativeTransportMode::IssuedGroups(_)));
    let unrelated_payload =
        Arc::downgrade(&full.metadata_snapshot().artifacts[&entries[2].descriptor.id]);
    let b_id = entries[1].descriptor.id;
    let a_id = entries[0].descriptor.id;
    let demand = source_import(&recovered[1].product, 7);
    // A different selected A is unrelated to the proof that supplied B. Both
    // alternatives deliberately share canonical interfaces with their peers.
    let replacement_a = product("ReceiptAncestor", 94);
    let replacement_b = product("ReceiptDependency", 95);
    assert_eq!(
        replacement_a.module_interface(),
        recovered[0].product.module_interface()
    );
    assert_eq!(
        replacement_b.module_interface(),
        recovered[1].product.module_interface()
    );
    let replacement_a =
        Arc::new(ArtifactEntry::original(producer.sha256(), replacement_a).unwrap());
    let replacement_b =
        Arc::new(ArtifactEntry::original(producer.sha256(), replacement_b).unwrap());
    assert_ne!(replacement_a.descriptor.id, a_id);
    assert_ne!(replacement_b.descriptor.id, b_id);
    let b = base.select_roots(vec![b_id]).unwrap();
    let b_projection = CompilerInputProjection::from_issued_entries(&[entries[1].clone()]).unwrap();
    let b_metadata = b_projection
        .project_metadata(b.metadata_snapshot())
        .unwrap();
    let b_transport = NativeCompilerTransport {
        metadata: b_projection
            .project_metadata(b.metadata_snapshot())
            .unwrap(),
        availability: BTreeMap::from([(b_id, receipt)]),
    };
    let inputs = RequestCompilerInputs {
        projection: b_projection.clone(),
        metadata: b_metadata,
        artifacts: b.clone(),
        native_transport: b_transport,
        declaration_semantic_sha256: [0; 32],
        original_sources: Default::default(),
    };
    let validation = PackageInterfaceValidation::default();
    let selected = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        &inputs,
        &inputs.metadata,
        &validation.inventory,
    )
    .unwrap();
    drop(inputs);
    drop(transport);
    drop(metadata);
    drop(unrelated_offer);
    drop(ancestor_offer);
    drop(full);
    drop(base);
    drop(entries);
    drop(recovered);
    assert!(
        unrelated_payload.upgrade().is_none(),
        "a B receipt must release unrelated C payload ownership"
    );

    let private = OriginalCompilerInputs::from_selection(&selected, &b).unwrap();
    let admitted = inventory
        .admit_projected_with_native_offers(
            &private.artifacts,
            vec![],
            &private.projection,
            &private.native_offers,
        )
        .unwrap();
    let graph = admitted.capture_graph_selection();
    assert!(graph
        .namespace
        .iter()
        .all(|binding| binding.artifact != a_id));
    assert!(graph
        .roots
        .iter()
        .all(|node| !matches!(node, ArtifactBindingNode::Group(_))));
    let groups = admitted
        .native_groups_for_target(None, std::slice::from_ref(&demand))
        .unwrap();
    assert_eq!(
        groups
            .iter()
            .map(|group| (group.binding.artifact(), group.original_ordinal))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([(b_id, 7)])
    );
    assert!(admitted
        .native_requirements_from_roots(&[NativeRequirementRoot::Group {
            artifact: b_id,
            original_ordinal: 9001
        }])
        .is_err());

    let context = |view, projection| {
        ExactDeclarationContext::from_facts(DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: fixture_compiler_support(projection, view),
            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        })
    };
    let selected_a = inventory
        .admit_shared_with_demand(
            &b,
            vec![replacement_a.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let selected_projection = b_projection
        .merge(&CompilerInputProjection::from_issued_entries(&[replacement_a.clone()]).unwrap())
        .unwrap();
    let request = ProtectedScaffoldRequirements::default()
        .compiler_inputs(&context(selected_a, selected_projection), Some(&private))
        .unwrap();
    assert!(request
        .metadata
        .entries
        .values()
        .any(|entry| entry.descriptor.id == replacement_a.descriptor.id));
    assert!(!request.native_transport.availability.contains_key(&a_id));
    let offers = request
        .native_transport
        .availability
        .values()
        .flat_map(|availability| availability.offers().iter().cloned())
        .collect::<Vec<_>>();
    let selected = inventory
        .admit_projected_with_native_offers(
            &request.artifacts,
            vec![],
            &request.projection,
            &offers,
        )
        .unwrap();
    let graph = selected.capture_graph_selection();
    assert!(graph
        .namespace
        .iter()
        .any(|binding| binding.artifact == replacement_a.descriptor.id));
    assert!(graph
        .namespace
        .iter()
        .all(|binding| binding.artifact != a_id));
    assert!(graph
        .roots
        .iter()
        .all(|node| !matches!(node, ArtifactBindingNode::Group(_))));
    assert_eq!(
        selected
            .native_groups_for_target(None, &[demand])
            .unwrap()
            .iter()
            .map(|group| (group.binding.artifact(), group.original_ordinal))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([(b_id, 7)])
    );
    assert!(!request.native_transport.availability[&b_id].permits(9001));

    let conflicting = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            vec![replacement_b.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let conflicting_projection =
        CompilerInputProjection::from_issued_entries(&[replacement_b]).unwrap();
    assert!(
        ProtectedScaffoldRequirements::default()
            .compiler_inputs(
                &context(conflicting, conflicting_projection),
                Some(&private)
            )
            .is_err(),
        "a different selected B must still refuse B's exact support"
    );
}

#[test]
fn optional_baseline_census_preserves_competing_exact_private_issuances() {
    use crate::artifact_inventory::{
        IssuedNativeOffer, NativeArtifactDemand, NativeOriginalRequirement, NativeRequirementRoot,
    };
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let native = product("OptionalCensus", 102);
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
    let canonical = ArtifactEntry::canonical(native.module_interface().unwrap().clone())
        .descriptor
        .id;
    let inventory = ArtifactInventory::default();
    let base = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            vec![entry.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let issue = |module, version| {
        let marker = product(module, version);
        let source = inventory
            .admit_shared(
                &base,
                vec![
                    Arc::new(ArtifactEntry::canonical(
                        marker.module_interface().unwrap().clone(),
                    )),
                    entry.clone(),
                ],
            )
            .unwrap();
        let proof = NativeOriginalRequirement::from_issued_group_custody(
            entry.descriptor.id,
            canonical,
            &source,
        )
        .unwrap()
        .unwrap();
        IssuedNativeOffer::issue(entry.descriptor.id, &source, Some(proof)).unwrap()
    };
    let left = issue("OptionalCensusLeft", 103);
    let right = issue("OptionalCensusRight", 104);
    let left_groups = left.issued_group_carriers().unwrap();
    let right_groups = right.issued_group_carriers().unwrap();
    assert_ne!(
        left_groups.keys().collect::<Vec<_>>(),
        right_groups.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        left_groups.values().collect::<Vec<_>>(),
        right_groups.values().collect::<Vec<_>>(),
        "independent group issuers retain the same artifact carrier"
    );
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    for reverse in [false, true] {
        let alternatives = if reverse {
            [right.clone(), left.clone()]
        } else {
            [left.clone(), right.clone()]
        };
        let ambiguous = retain_native_offers(&base, &alternatives).unwrap();
        assert!(ambiguous.selected_native_groups().is_empty());
        assert!(
            matches!(NativeOriginalRequirement::from_issued_group_custody(entry.descriptor.id, canonical, &ambiguous),
            Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == entry.descriptor.id)),
            "explicit complete issuance remains strict"
        );
        let context = Arc::new(ExactDeclarationContext::from_facts(
            DeclarationContextFacts {
                producer: producer.sha256(),
                compiler_support: fixture_compiler_support(projection.clone(), ambiguous),
                lexical: Default::default(),
                template_imports: None,
                original_instance_environment: OriginalInstanceEnvironment::Unknown,
            },
        ));
        let baseline = ProtectedScaffoldRequirements::default()
            .compiler_inputs(&context, None)
            .unwrap();
        let empty = &baseline.native_transport.availability[&entry.descriptor.id];
        assert!(!empty.permits(7));
        assert!(!empty.permits(9001));
        assert!(empty
            .offers()
            .iter()
            .all(|offer| offer.proof().is_none() && !offer.is_full()));
        assert!(
            matches!(baseline.artifacts.native_requirements_from_roots(&[NativeRequirementRoot::Group {
            artifact: entry.descriptor.id, original_ordinal: 7,
        }]), Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == entry.descriptor.id)),
            "a real demand without an exact choice must still refuse"
        );
        for chosen in [&left, &right] {
            let private = OriginalCompilerInputs {
                projection: projection.clone(),
                artifacts: retain_native_offers(&base, std::slice::from_ref(chosen)).unwrap(),
                native_offers: vec![chosen.clone()],
            };
            let root = tempfile::tempdir().unwrap();
            let request = ExactCompileContext::new(context.clone())
                .prepare_compilation_with_private_input(root.path(), PRODUCER, None, Some(private))
                .unwrap();
            assert_eq!(wire_mode(&request, "OptionalCensus"), full_mode());
            assert!(request.groups.is_empty());
            let requirements = request.compiler_inputs().native_original_requirements();
            assert_eq!(requirements.as_slice(), &[chosen.proof().unwrap().clone()]);
            assert_eq!(
                requirements[0]
                    .issued_group_carriers(&request.compiler_inputs().artifacts)
                    .unwrap(),
                chosen.issued_group_carriers().unwrap()
            );
        }
    }
}

#[test]
fn advertised_unselected_body_preserves_historical_generation_identity_only() {
    use crate::artifact_inventory::{
        ExactArtifactSelection, IssuedNativeOffer, NativeGroupKey, NativeOriginalRequirement,
    };
    let identity = tidepool_repr::execution_schema::SymbolIdentity {
        unit: "main".into(),
        module: "Tidepool.Session.Val.G1".into(),
        namespace: "value".into(),
        occurrence: "x".into(),
        record_parent: None,
    };
    let partial_identity = tidepool_repr::execution_schema::SymbolIdentity {
        module: "Tidepool.Session.Val.G2".into(),
        occurrence: "y".into(),
        ..identity.clone()
    };
    let mixed = dependent_product(
        "AdvertisedRetained",
        100,
        vec![
            (
                7,
                vec![crate::certified_products::PendingImportOwner::Retained {
                    identity: partial_identity.clone(),
                    generation: 2,
                }],
            ),
            (
                9001,
                vec![crate::certified_products::PendingImportOwner::Retained {
                    identity: identity.clone(),
                    generation: 1,
                }],
            ),
        ],
    );
    let mixed = recovered_witness_fixtures(&[mixed]).remove(0).product;
    let context = type_context(std::slice::from_ref(&mixed));
    // Retained imports require exact type custody, independently of whether a
    // corresponding live value or native implementation remains available.
    let value_interfaces = [&identity, &partial_identity]
        .into_iter()
        .map(|binder| {
            Arc::new(ArtifactEntry::canonical(
                crate::certified_products::fixture_module_interface(
                    context.facts.producer,
                    &binder.unit,
                    &binder.module,
                    BTreeMap::new(),
                ),
            ))
        })
        .collect::<Vec<_>>();
    let projection = context
        .facts
        .compiler_support
        .projection
        .merge(&CompilerInputProjection::from_issued_entries(&value_interfaces).unwrap())
        .unwrap();
    let view = context
        .artifact_view()
        .inventory()
        .admit_shared(context.artifact_view(), value_interfaces)
        .unwrap();
    let context = Arc::new(Arc::unwrap_or_clone(context).rebuild(|facts| {
        facts.compiler_support.artifacts = view;
        facts.compiler_support.projection = projection;
    }));
    let private = OriginalCompilerInputs::from_native_availability(
        &context,
        CanonicalProducerIdentity::from_producer_bytes(PRODUCER),
        &[mixed],
    )
    .unwrap();
    let original = private.native_offers[0].original();
    assert!(private.native_offers[0].is_full());
    let inventory = private.artifacts.inventory();
    let partial_view = inventory
        .admit_recovery_selection(
            &inventory.empty_view(),
            private.artifacts.entries(),
            &BTreeSet::from([NativeGroupKey {
                artifact: original,
                original_ordinal: 7,
            }]),
        )
        .unwrap();
    let baseline_roots = private.artifacts.capture_graph_selection().roots;
    let root = tempfile::tempdir().unwrap();
    let key = tidepool_extract_cmd::SymbolIdentity {
        unit: identity.unit,
        module: identity.module,
        namespace: identity.namespace,
        occurrence: identity.occurrence,
        record_parent: identity.record_parent,
    };
    let partial_key = tidepool_extract_cmd::SymbolIdentity {
        unit: partial_identity.unit,
        module: partial_identity.module,
        namespace: partial_identity.namespace,
        occurrence: partial_identity.occurrence,
        record_parent: partial_identity.record_parent,
    };
    for (label, offer, expected, partial_expected) in [
        ("full", private.native_offers[0].clone(), Some(1), Some(2)),
        (
            "partial",
            IssuedNativeOffer::issue(
                original,
                &partial_view,
                Some(NativeOriginalRequirement {
                    original,
                    selection: ExactArtifactSelection::capture(&partial_view),
                }),
            )
            .unwrap(),
            None,
            Some(2),
        ),
        (
            "empty",
            IssuedNativeOffer::issue(original, &private.artifacts, None).unwrap(),
            None,
            None,
        ),
    ] {
        let mut input = private.clone();
        input.native_offers = vec![offer];
        let request = ExactCompileContext::new(context.clone())
            .prepare_compilation_with_private_input(
                &root.path().join(label),
                PRODUCER,
                None,
                Some(input),
            )
            .unwrap();
        assert!(
            request.groups.is_empty(),
            "no native executable demand was selected"
        );
        assert_eq!(
            request
                .compiler_inputs()
                .artifacts
                .capture_graph_selection()
                .roots,
            baseline_roots
        );
        let mut command = tidepool_extract_cmd::ExtractCmd::with_bin(
            tidepool_extract_cmd::ResolvedExtractBin::assume_resolved("unused-fixture-extractor"),
        );
        command.input("unused-fixture-input.hs");
        request
            .apply_to(
                &mut command,
                RetainedGenerationPolicy::PreserveCertifiedDemand,
            )
            .unwrap();
        let decoded =
            tidepool_extract_cmd::ExtractRequest::decode(&command.request_bytes()).unwrap();
        assert_eq!(
            decoded.retained_generations().get(&key).copied(),
            expected,
            "{label}"
        );
        assert_eq!(
            decoded.retained_generations().get(&partial_key).copied(),
            partial_expected,
            "{label} permitted partial import"
        );
        assert_eq!(request.groups.len(), 0);
        let mut preview = tidepool_extract_cmd::ExtractCmd::with_bin(
            tidepool_extract_cmd::ResolvedExtractBin::assume_resolved("unused-fixture-extractor"),
        );
        preview.input("unused-fixture-input.hs");
        request
            .apply_to(
                &mut preview,
                RetainedGenerationPolicy::PureActivationPreview,
            )
            .unwrap();
        assert!(
            tidepool_extract_cmd::ExtractRequest::decode(&preview.request_bytes())
                .unwrap()
                .retained_generations()
                .is_empty()
        );
    }
}

#[test]
fn retained_dependency_support_refuses_a_selected_conflicting_backedge() {
    use crate::artifact_inventory::{
        IssuedNativeOffer, NativeArtifactDemand, NativeOriginalRequirement,
    };
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let b_seed = product("BackedgeDependency", 98);
    let a_preliminary = dependent_product(
        "BackedgeAncestor",
        97,
        vec![(7, vec![source_import(&b_seed, 7)]), (9001, vec![])],
    );
    let b_final = dependent_product(
        "BackedgeDependency",
        98,
        vec![
            (7, vec![source_import(&a_preliminary, 9001)]),
            (9001, vec![]),
        ],
    );
    let a_final = dependent_product(
        "BackedgeAncestor",
        97,
        vec![(7, vec![source_import(&b_final, 7)]), (9001, vec![])],
    );
    assert_eq!(
        a_preliminary.owner(),
        a_final.owner(),
        "source owner fixed point"
    );
    let recovered = recovered_witness_fixtures(&[a_final, b_final]);
    let entries = recovered
        .iter()
        .map(|row| {
            Arc::new(ArtifactEntry::original(producer.sha256(), row.product.clone()).unwrap())
        })
        .collect::<Vec<_>>();
    let inventory = ArtifactInventory::default();
    let base = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            entries.clone(),
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let full = inventory.admit_shared(&base, entries.clone()).unwrap();
    let a_id = entries[0].descriptor.id;
    let b_id = entries[1].descriptor.id;
    let canonical =
        ArtifactEntry::canonical(recovered[0].product.module_interface().unwrap().clone())
            .descriptor
            .id;
    let proof = NativeOriginalRequirement::from_issued_group_custody(a_id, canonical, &full)
        .unwrap()
        .unwrap();
    let a_offer = IssuedNativeOffer::issue(a_id, &full, Some(proof)).unwrap();
    let support = a_offer
        .dependency_group_carriers(&BTreeSet::from([b_id]))
        .unwrap();
    assert_eq!(
        support
            .keys()
            .map(|group| (group.binding.artifact(), group.original_ordinal))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([(b_id, 7), (a_id, 9001)]),
        "the independently authored B closure includes its real backedge only",
    );
    let b_offer = IssuedNativeOffer::issue(b_id, &base, None).unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&[entries[1].clone()]).unwrap();
    let b = base.select_roots(vec![b_id]).unwrap();
    let offers = vec![a_offer, b_offer];
    let private = OriginalCompilerInputs {
        projection: projection.clone(),
        artifacts: retain_native_offers(&b, &offers).unwrap(),
        native_offers: offers,
    };
    let context = |view, projection| {
        ExactDeclarationContext::from_facts(DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: fixture_compiler_support(projection, view),
            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        })
    };
    ProtectedScaffoldRequirements::default()
        .compiler_inputs(&context(b.clone(), projection.clone()), Some(&private))
        .expect("without a conflicting selected A, B retains its real A9001 dependency");
    inventory
        .admit_projected_with_native_offers(
            &private.artifacts,
            vec![],
            &projection,
            &private.native_offers,
        )
        .expect("typed admission accepts the same exact dependency closure");

    let replacement = product("BackedgeAncestor", 99);
    assert_eq!(
        replacement.module_interface(),
        recovered[0].product.module_interface()
    );
    let replacement = Arc::new(ArtifactEntry::original(producer.sha256(), replacement).unwrap());
    assert_ne!(replacement.descriptor.id, a_id);
    let selected_projection = projection
        .merge(&CompilerInputProjection::from_issued_entries(&[replacement.clone()]).unwrap())
        .unwrap();
    let selected = inventory
        .admit_shared_with_demand(
            &b,
            vec![replacement.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    assert!(
        ProtectedScaffoldRequirements::default()
            .compiler_inputs(
                &context(selected, selected_projection.clone()),
                Some(&private),
            )
            .is_err(),
        "transport must not discard B's real backedge to old A"
    );
    assert!(
        inventory
            .admit_projected_with_native_offers(
                &private.artifacts,
                vec![replacement],
                &selected_projection,
                &private.native_offers,
            )
            .is_err(),
        "typed admission must also preserve the incompatible exact backedge"
    );
}

#[test]
fn custody_only_native_offer_keeps_materialized_inputs_after_issuer_release() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    let native = product("CustodyMaterialization", 96);
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let context = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products(producer.sha256(), &[native])
            .unwrap(),
    );
    let scratch = tempfile::tempdir().unwrap();
    let completed = context
        .prepare_compilation(scratch.path(), PRODUCER)
        .unwrap();
    let materialization = completed.materialization.as_ref().unwrap();
    let directory = materialization._directory.as_ref().unwrap();
    let directory_owner = Arc::downgrade(directory);
    let directory_path = directory.path().to_path_buf();
    assert!(!materialization.rows.is_empty());
    assert!(std::fs::read_dir(&directory_path).unwrap().next().is_some());
    let entry = context
        .artifact_view()
        .entries()
        .into_iter()
        .find(|entry| matches!(entry.payload, ArtifactPayload::Original(_)))
        .unwrap();
    let canonical = context
        .facts
        .compiler_support
        .projection
        .roles()
        .into_iter()
        .find(|role| role.original() == Some(entry.descriptor.id))
        .unwrap()
        .interface();
    let proof = NativeOriginalRequirement::from_issued_group_custody(
        entry.descriptor.id,
        canonical,
        context.artifact_view(),
    )
    .unwrap()
    .unwrap();
    let offer = IssuedNativeOffer::issue(entry.descriptor.id, context.artifact_view(), Some(proof))
        .unwrap();
    drop(completed);
    drop(context);
    assert!(
        directory_owner.upgrade().is_some(),
        "exact offer custody retains materialized file leases"
    );
    assert!(std::fs::read_dir(&directory_path).unwrap().next().is_some());
    offer
        .proof()
        .unwrap()
        .issued_group_carriers(offer.custody())
        .unwrap();
    drop(offer);
    assert!(
        directory_owner.upgrade().is_none(),
        "the offer must release its last materialization lease"
    );
    assert!(!directory_path.exists());
}

#[test]
fn private_transport_full_offer_cannot_borrow_another_offers_root() {
    use crate::artifact_inventory::{
        ExactArtifactSelection, NativeArtifactDemand, NativeOriginalRequirement,
    };

    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let native = product("FullOfferRoot", 85);
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
    let canonical = ArtifactEntry::canonical(native.module_interface().unwrap().clone());
    let inventory = ArtifactInventory::default();
    let base = inventory
        .admit_shared_with_demand(
            &inventory.empty_view(),
            vec![entry.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let full = inventory.admit_shared(&base, vec![entry.clone()]).unwrap();
    let issued = NativeOriginalRequirement::from_issued_group_custody(
        entry.descriptor.id,
        canonical.descriptor.id,
        &full,
    )
    .unwrap()
    .unwrap();
    // An authentic graph without native roots cannot construct Full. Another
    // offer's complete graph does not change that owner's qualification.
    let no_groups = NativeOriginalRequirement {
        original: entry.descriptor.id,
        selection: ExactArtifactSelection::capture(&base),
    };
    let unqualified = crate::artifact_inventory::IssuedNativeOffer::issue(
        entry.descriptor.id,
        &full,
        Some(no_groups),
    )
    .unwrap();
    assert!(!unqualified.is_full());
    let complete = crate::artifact_inventory::IssuedNativeOffer::issue(
        entry.descriptor.id,
        &full,
        Some(issued),
    )
    .unwrap();
    assert!(complete.is_full());
    let no_demand = CompilerInputProjection::from_issued_entries(&[entry.clone()])
        .unwrap()
        .project_metadata(base.metadata_snapshot())
        .unwrap();
    for other in [false, true] {
        let mut offers = vec![unqualified.clone()];
        if other {
            offers.push(complete.clone());
        }
        let transport = NativeCompilerTransport::issue(&no_demand, &full, &offers).unwrap();
        assert_eq!(
            transport.availability[&entry.descriptor.id].permits(7),
            other
        );
        assert!(!offers[0].is_full());
    }
    let empty_offer =
        crate::artifact_inventory::IssuedNativeOffer::issue(entry.descriptor.id, &full, None)
            .unwrap();
    let empty = NativeCompilerTransport::issue(&no_demand, &full, &[empty_offer]).unwrap();
    assert!(!empty.availability[&entry.descriptor.id].permits(7));
}

#[test]
fn private_transport_refuses_selected_dependency_product_and_interface_conflicts() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let dependency = product("CurrentDependency", 86);
    let consumer = dependent_product(
        "CurrentConsumer",
        87,
        vec![(7, vec![source_import(&dependency, 7)])],
    );
    for interface_drift in [false, true] {
        let replacement = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "CurrentDependency",
                vec![(7, vec![]), (9001, vec![])],
                88,
                &BTreeMap::new(),
                if interface_drift {
                    b"different dependency interface".to_vec()
                } else {
                    b"CurrentDependency".to_vec()
                },
            ),
            producer.sha256(),
        );
        let recovered =
            recovered_witness_fixtures(&[dependency.clone(), consumer.clone(), replacement]);
        let entries = recovered
            .iter()
            .map(|row| {
                Arc::new(ArtifactEntry::original(producer.sha256(), row.product.clone()).unwrap())
            })
            .collect::<Vec<_>>();
        let original = &recovered[0].product;
        let consumer = &recovered[1].product;
        let replacement = &recovered[2].product;
        assert_eq!(
            original.module_interface() != replacement.module_interface(),
            interface_drift
        );
        assert_ne!(entries[0].descriptor.id, entries[2].descriptor.id);
        let inventory = ArtifactInventory::default();
        let issued = inventory
            .admit_shared(&inventory.empty_view(), entries[..2].to_vec())
            .unwrap();
        let canonical = ArtifactEntry::canonical(consumer.module_interface().unwrap().clone());
        let proof =
            crate::artifact_inventory::NativeOriginalRequirement::from_issued_group_custody(
                entries[1].descriptor.id,
                canonical.descriptor.id,
                &issued,
            )
            .unwrap()
            .unwrap();
        let current = inventory
            .admit_shared(&inventory.empty_view(), vec![entries[2].clone()])
            .unwrap();
        let merged = issued.merge(&current);
        if interface_drift {
            let error = merged.expect_err("different interfaces cannot share an owner namespace");
            assert!(matches!(
                error,
                CompileError::ArtifactInventory(error)
                    if error.failure
                        == crate::artifact_inventory::ArtifactInventoryFailure::OwnerConflict {
                            owner: entries[0].descriptor.owner.clone(),
                        }
            ));
            continue;
        }
        let view = merged.unwrap();
        let metadata =
            CompilerInputProjection::from_issued_entries(&[entries[1].clone(), entries[2].clone()])
                .unwrap()
                .project_metadata(view.metadata_snapshot())
                .unwrap();
        let error = NativeCompilerTransport::issue(
            &metadata,
            &view,
            &[crate::artifact_inventory::IssuedNativeOffer::issue(
                entries[1].descriptor.id,
                &issued,
                Some(proof),
            )
            .unwrap()],
        )
        .map(|_| ())
        .expect_err("dependency availability cannot replace a selected product or interface");
        assert_eq!(
            error.to_string(),
            "extractor contract failure: artifact inventory: issued native dependency conflicts with request interface"
        );
    }
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
            .compiler_support
            .artifacts
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
            compiler_support: fixture_compiler_support(projection, narrowed),
            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        },
    ));
    let before = context.as_ref().clone();
    assert!(context
        .facts
        .compiler_support
        .artifacts
        .selected_native_groups()
        .is_empty());
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
        &context.facts.compiler_support.artifacts,
    )
    .unwrap()
    .expect("complete authored census was issued before demotion");
    let groups = &proof.selection.graph_selection().native_groups;
    assert!(groups
        .iter()
        .any(|group| group.binding.artifact() == helper_id && group.original_ordinal == 7));
    assert!(
        !groups
            .iter()
            .any(|group| group.binding.artifact() == helper_id && group.original_ordinal == 9001),
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
        .native_original_requirements()
        .iter()
        .find(|requirement| requirement.original == authored_id)
        .cloned()
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
        let updated = context.facts.compiler_support.projection.interface_only();
        context = context.rebuild(|facts| facts.compiler_support.projection = updated);
    }
    assert!(context
        .compiler_input_roles()
        .iter()
        .all(|role| role.original().is_none()));
    let keys = context
        .facts
        .compiler_support
        .artifacts
        .selected_native_groups();
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
            .compiler_support
            .artifacts
            .inventory()
            .admit_shared(
                &changed
                    .facts
                    .compiler_support
                    .artifacts
                    .inventory()
                    .empty_view(),
                vec![Arc::clone(&entry)],
            )
            .unwrap();
        changed = changed.rebuild(|facts| facts.compiler_support.artifacts = updated);
    }
    {
        let updated = CompilerInputProjection::from_issued_entries(&[entry]).unwrap();
        changed = changed.rebuild(|facts| facts.compiler_support.projection = updated);
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
            .compiler_support
            .artifacts
            .inventory()
            .admit_shared(&ambiguous.facts.compiler_support.artifacts, entries.clone())
            .unwrap();
        ambiguous = ambiguous.rebuild(|facts| facts.compiler_support.artifacts = updated);
    }
    assert!(ambiguous
        .facts
        .compiler_support
        .artifacts
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
        ambiguous = ambiguous.rebuild(|facts| facts.compiler_support.projection = updated);
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
                .compiler_support
                .artifacts
                .inventory()
                .admit_shared_with_demand(
                    &context.facts.compiler_support.artifacts,
                    entries.clone(),
                    crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
                )
                .expect("same-canonical body variants retain legal byte custody");
            context = context.rebuild(|facts| facts.compiler_support.artifacts = updated);
        }
        context.compiler_metadata_snapshot().unwrap();
        let custody = context
            .facts
            .compiler_support
            .artifacts
            .selected_native_groups();
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
            unissued = unissued.rebuild(|facts| facts.compiler_support.projection = updated);
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
            context = context.rebuild(|facts| facts.compiler_support.projection = updated);
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
                .compiler_support
                .artifacts
                .inventory()
                .admit_shared(&context.facts.compiler_support.artifacts, entries)
                .unwrap();
            context = context.rebuild(|facts| facts.compiler_support.artifacts = updated);
        }
        context.compiler_metadata_snapshot().unwrap();
        let groups = context
            .facts
            .compiler_support
            .artifacts
            .selected_native_groups();
        assert_eq!(groups.len(), products.len() * 2);
        let dependencies = context.facts.compiler_support.artifacts.dependencies();
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
        assert_eq!(
            context
                .facts
                .compiler_support
                .artifacts
                .selected_native_groups(),
            groups
        );
        assert_eq!(
            context.facts.compiler_support.artifacts.dependencies(),
            dependencies
        );
        let mut canonical_only = context.clone();
        {
            let updated = canonical_only
                .facts
                .compiler_support
                .projection
                .interface_only();
            canonical_only =
                canonical_only.rebuild(|facts| facts.compiler_support.projection = updated);
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
            .compiler_support
            .artifacts
            .inventory()
            .admit_shared_with_demand(
                &context.facts.compiler_support.artifacts,
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
        assert_eq!(
            context
                .facts
                .compiler_support
                .artifacts
                .selected_native_groups(),
            groups
        );
        assert_eq!(
            context.facts.compiler_support.artifacts.dependencies(),
            dependencies
        );
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
    let projection = native_context
        .facts
        .compiler_support
        .projection
        .interface_only();
    native_context = native_context.rebuild(|facts| facts.compiler_support.projection = projection);
    let producer = CanonicalProducerIdentity::from_producer_bytes(producer_bytes);
    let authored_root = native_context.authored_native_root(generation).unwrap();
    let selected_before = native_context
        .facts
        .compiler_support
        .artifacts
        .selected_native_groups();
    let assert_exact_offer = |context: &ExactDeclarationContext, name: &str, expected: bool| {
        let before = context
            .facts
            .compiler_support
            .artifacts
            .selected_native_groups();
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
        assert_eq!(
            context
                .facts
                .compiler_support
                .artifacts
                .selected_native_groups(),
            before
        );
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
    let interface_roles = captured.facts.compiler_support.projection.interface_only();
    let (custody, projection) = captured
        .facts
        .compiler_support
        .artifacts
        .inventory()
        .empty_view()
        .merge_compiler_inputs(
            &CompilerInputProjection::default(),
            &captured.facts.compiler_support.artifacts,
            &interface_roles,
        )
        .unwrap();
    captured = captured.rebuild(|facts| {
        facts.compiler_support.artifacts = custody;
        facts.compiler_support.projection = projection;
    });
    assert!(captured
        .facts
        .compiler_support
        .artifacts
        .selected_native_groups()
        .is_empty());
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
    assert!(fresh
        .facts
        .compiler_support
        .artifacts
        .selected_native_groups()
        .is_empty());
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
            .compiler_support
            .artifacts
            .inventory()
            .admit_recovery_selection(
                &partial
                    .facts
                    .compiler_support
                    .artifacts
                    .inventory()
                    .empty_view(),
                partial.facts.compiler_support.artifacts.entries(),
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
            partial = partial.rebuild(|facts| facts.compiler_support.artifacts = updated);
        }
        assert_eq!(
            partial
                .facts
                .compiler_support
                .artifacts
                .selected_native_groups(),
            selected
        );
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
                .compiler_support
                .artifacts
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
            compiler_support: fixture_compiler_support(
                CompilerInputProjection::from_issued_entries(&[entry]).unwrap(),
                artifacts,
            ),
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

#[derive(Clone, Debug)]
struct NativeTransportHistory {
    same_root: bool,
    overlap: bool,
    shared_carrier: bool,
    reverse: bool,
    duplicate: bool,
    custody: Vec<NativeCustodyOp>,
}

#[derive(Clone, Debug)]
enum NativeCustodyOp {
    Expand,
    Capture { right: bool },
}

#[derive(Debug, Default)]
struct NativeTransportCoverage {
    callbacks: usize,
    completed: usize,
    equal_payload_scopes: usize,
    shared_carriers: usize,
    distinct_carriers: usize,
    full_without_demand: usize,
    partial_with_ambient_full: usize,
    custody_expansions: usize,
    captures: usize,
    duplicate_joins: usize,
    reversed_joins: usize,
    preparations: usize,
    dependent_issuances: usize,
    continuations: usize,
    successful_demands: usize,
    ambiguous_demands: usize,
    recoveries: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct LogicalNativeGroup {
    issuance: usize,
    payload: usize,
    ordinal: u32,
}

#[derive(Default)]
struct NativeTransportModel {
    groups: BTreeMap<LogicalNativeGroup, crate::artifact_inventory::NativeGroupBinding>,
    carriers: BTreeMap<LogicalNativeGroup, ArtifactBinding>,
    edges: BTreeMap<LogicalNativeGroup, BTreeSet<LogicalNativeGroup>>,
}

impl NativeTransportModel {
    fn observe_issuance(
        &mut self,
        issuance: usize,
        root_payload: usize,
        dependency_ordinal: u32,
        entries: &[Arc<ArtifactEntry>; 3],
        source: &ArtifactView,
    ) -> LogicalNativeGroup {
        let root = LogicalNativeGroup {
            issuance,
            payload: root_payload,
            ordinal: 7,
        };
        let dependency = LogicalNativeGroup {
            issuance,
            payload: 0,
            ordinal: dependency_ordinal,
        };
        let graph = source.capture_graph_selection();
        assert_eq!(
            graph.native_groups.len(),
            2,
            "fixture issues exactly the authored pair"
        );
        for logical in [root, dependency] {
            let id = entries[logical.payload].descriptor.id;
            let handles = graph
                .native_groups
                .iter()
                .copied()
                .filter(|group| {
                    group.binding.artifact() == id && group.original_ordinal == logical.ordinal
                })
                .collect::<Vec<_>>();
            let [handle] = handles.as_slice() else {
                panic!("one issued identity per logical group")
            };
            assert!(
                !self.groups.values().any(|prior| prior == handle),
                "independent group identities cannot alias"
            );
            let carriers = graph
                .namespace
                .iter()
                .copied()
                .filter(|binding| binding.artifact == id)
                .collect::<Vec<_>>();
            let [carrier] = carriers.as_slice() else {
                panic!("one issued carrier per logical payload")
            };
            self.groups.insert(logical, *handle);
            self.carriers.insert(logical, *carrier);
        }
        // Expected topology comes from the authored imports, not captured edges.
        self.edges.insert(root, BTreeSet::from([dependency]));
        self.edges.insert(dependency, BTreeSet::new());
        let native_edges = source
            .binding_dependencies()
            .into_iter()
            .filter_map(|(from, to, label)| match (from, to, label) {
                (
                    ArtifactBindingNode::Group(from),
                    ArtifactBindingNode::Group(to),
                    ArtifactDependency::NativeGroup { .. },
                ) => Some((from, to)),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            native_edges,
            BTreeSet::from([(self.groups[&root], self.groups[&dependency])])
        );
        root
    }

    fn closure(
        &self,
        root: LogicalNativeGroup,
    ) -> BTreeMap<crate::artifact_inventory::NativeGroupBinding, ArtifactBinding> {
        let mut reachable = BTreeSet::from([root]);
        loop {
            let next = reachable
                .iter()
                .flat_map(|group| self.edges[group].iter().copied())
                .chain(reachable.iter().copied())
                .collect::<BTreeSet<_>>();
            if next == reachable {
                break;
            }
            reachable = next;
        }
        reachable
            .into_iter()
            .map(|group| (self.groups[&group], self.carriers[&group]))
            .collect()
    }
}

fn replay_native_transport_history(
    history: &NativeTransportHistory,
    coverage: &mut NativeTransportCoverage,
) {
    use crate::artifact_inventory::{NativeArtifactDemand, NativeOriginalRequirement};
    let NativeTransportHistory {
        same_root,
        overlap,
        shared_carrier,
        reverse,
        duplicate,
        custody,
    } = history;
    let (same_root, overlap, shared_carrier, reverse, duplicate) =
        (*same_root, *overlap, *shared_carrier, *reverse, *duplicate);
    assert!(
        !same_root || overlap,
        "one root has one authored dependency"
    );
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let root = tempfile::tempdir().unwrap();
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
            .admit_recovery_selection(
                &base,
                vec![
                    entries[0].clone(),
                    entries[index].clone(),
                    Arc::new(ArtifactEntry::canonical(
                        marker.module_interface().unwrap().clone(),
                    )),
                ],
                &BTreeSet::from([
                    NativeGroupKey {
                        artifact: entries[index].descriptor.id,
                        original_ordinal: 7,
                    },
                    NativeGroupKey {
                        artifact: entries[0].descriptor.id,
                        original_ordinal: if index == 1 || overlap { 7 } else { 9001 },
                    },
                ]),
            )
            .unwrap()
    };
    let sources = [issue(1, "LeftScope", 83), issue(2, "RightScope", 84)];
    let proofs = std::array::from_fn::<_, 2, _>(|index| {
        let canonical =
            ArtifactEntry::canonical(products[index + 1].module_interface().unwrap().clone());
        NativeOriginalRequirement::from_issued_group_custody(
            entries[index + 1].descriptor.id,
            canonical.descriptor.id,
            &sources[index],
        )
        .unwrap()
        .unwrap()
    });
    let mut model = NativeTransportModel::default();
    let logical_roots = std::array::from_fn::<_, 2, _>(|index| {
        model.observe_issuance(
            index,
            if same_root { 1 } else { index + 1 },
            if index == 0 || overlap { 7 } else { 9001 },
            &entries,
            &sources[index],
        )
    });
    let expected = logical_roots.map(|root| model.closure(root));
    for index in 0..2 {
        assert_eq!(
            proofs[index]
                .issued_group_carriers(&sources[index])
                .unwrap(),
            expected[index]
        );
    }
    let child = |index: usize| {
        expected[index]
            .iter()
            .find(|(group, _)| group.binding.artifact() == entries[0].descriptor.id)
            .map(|(group, carrier)| (*group, *carrier))
            .unwrap()
    };
    assert_eq!(child(0).1 == child(1).1, shared_carrier);
    coverage.shared_carriers += usize::from(shared_carrier);
    coverage.distinct_carriers += usize::from(!shared_carrier);
    if overlap {
        assert_eq!(child(0).0.original_ordinal, child(1).0.original_ordinal);
        assert_ne!(child(0).0, child(1).0);
        coverage.equal_payload_scopes += 1;
    }
    let partial = std::array::from_fn::<_, 2, _>(|index| {
        NativeOriginalRequirement::from_selected_view(entries[0].descriptor.id, &sources[index])
            .unwrap()
            .unwrap()
    });
    let ambient = inventory
        .admit_recovery_selection(
            &sources[0],
            vec![],
            &BTreeSet::from([7, 9001].map(|original_ordinal| NativeGroupKey {
                artifact: entries[0].descriptor.id,
                original_ordinal,
            })),
        )
        .unwrap();
    let dependency_interface =
        ArtifactEntry::canonical(products[0].module_interface().unwrap().clone());
    let ambient_proof = NativeOriginalRequirement::from_issued_group_custody(
        entries[0].descriptor.id,
        dependency_interface.descriptor.id,
        &ambient,
    )
    .unwrap()
    .unwrap();
    let mut expanded = sources[0].clone();
    let mut has_ambient_full = false;
    for op in custody {
        match *op {
            NativeCustodyOp::Expand => {
                let before = expanded.capture_graph_selection();
                expanded = expanded
                    .merge(&sources[1])
                    .unwrap()
                    .merge(&ambient)
                    .unwrap()
                    .retain_original_custody(&sources[0])
                    .unwrap();
                let after = expanded.capture_graph_selection();
                assert_eq!(before.namespace, after.namespace);
                assert_eq!(before.roots, after.roots);
                has_ambient_full = true;
                coverage.custody_expansions += 1;
            }
            NativeCustodyOp::Capture { right } => {
                let index = usize::from(right);
                // Namespace restoration below must not repair lost proof nodes.
                // Observe exact custody before rejoining any source payloads.
                for required in 0..if has_ambient_full { 2 } else { 1 } {
                    assert_eq!(
                        proofs[required].issued_group_carriers(&expanded).unwrap(),
                        expected[required]
                    );
                }
                if has_ambient_full {
                    assert!(ambient_proof
                        .qualify_availability(&expanded)
                        .unwrap()
                        .is_some());
                }
                let capture_projection = CompilerInputProjection::from_issued_entries(&[
                    entries[0].clone(),
                    entries[index + 1].clone(),
                ])
                .unwrap();
                let supplied = [proofs[index].clone(), partial[index].clone()];
                let before = crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(
                    &capture_projection,
                    &sources[index],
                    &supplied,
                )
                .unwrap();
                // Restore this source's complete selected namespace, including
                // its marker payload, before comparing custody-only additions.
                let receiving = expanded
                    .merge(&sources[index])
                    .unwrap()
                    .retain_original_custody(&sources[index])
                    .unwrap();
                assert_eq!(
                    receiving.capture_graph_selection().namespace,
                    sources[index].capture_graph_selection().namespace
                );
                let after = crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(
                    &capture_projection,
                    &receiving,
                    &supplied,
                )
                .unwrap();
                assert_eq!(after, before, "custody expansion cannot change supplied evidence with fixed canonical bindings");
                assert!(after
                    .iter()
                    .any(|offer| offer.is_full() && offer.proof() == Some(&proofs[index])));
                assert!(after
                    .iter()
                    .any(|offer| !offer.is_full() && offer.proof() == Some(&partial[index])));
                if index == 0 && has_ambient_full {
                    assert!(ambient_proof
                        .qualify_availability(&receiving)
                        .unwrap()
                        .is_some());
                    coverage.partial_with_ambient_full += 1;
                }
                coverage.captures += 1;
            }
        }
    }
    let context = |view: &ArtifactView| {
        let view = inventory
            .empty_view()
            .merge_compiler_inputs(&CompilerInputProjection::default(), view, &interfaces)
            .unwrap()
            .0;
        Arc::new(ExactDeclarationContext::from_facts(
            DeclarationContextFacts {
                producer: producer.sha256(),
                compiler_support: fixture_compiler_support(interfaces.clone(), view),
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
    let first = usize::from(reverse);
    let mut joined = private[first].merge(&private[1 - first]).unwrap();
    coverage.reversed_joins += usize::from(reverse);
    if duplicate {
        joined = joined.merge(&private[first]).unwrap();
        coverage.duplicate_joins += 1;
    }
    // Join order must not change demand when the caller pins the same namespace.
    joined.artifacts = joined
        .artifacts
        .retain_original_custody(&private[0].artifacts)
        .unwrap();
    let request = ExactCompileContext::new(base_context.clone())
        .prepare_compilation_with_private_input(
            &root.path().join("request"),
            PRODUCER,
            None,
            Some(joined),
        )
        .unwrap();
    coverage.preparations += 1;
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
    assert_eq!(wire_mode(&request, "IndependentLeft"), full_mode());
    coverage.full_without_demand += 1;
    for index in 0..2 {
        assert!(inputs.native_original_requirements().iter().any(|proof| {
            proof.original == proofs[index].original
                && proof.issued_group_carriers(&inputs.artifacts).unwrap() == expected[index]
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
        inputs
            .artifacts
            .inventory()
            .bind_segment(&inputs.artifacts, vec![], &selected)
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
            coverage.ambiguous_demands += 1;
        } else {
            let admitted = admitted.unwrap();
            let groups = admitted
                .selection
                .groups_for_target(&admitted.artifacts, None, &imports)
                .unwrap();
            let expected_index = if same_root { 0 } else { index };
            assert_eq!(groups, expected[expected_index].keys().copied().collect());
            coverage.successful_demands += 1;
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
        coverage.ambiguous_demands += 1;
    } else {
        let demanded = demanded.unwrap();
        let groups = demanded
            .selection
            .groups_for_target(&demanded.artifacts, None, &imports)
            .unwrap();
        assert_eq!(groups, BTreeSet::from([child(0).0]));
        coverage.successful_demands += 1;
    }
    let handoff = OriginalCompilerInputs::from_selection(&selected, &inputs.artifacts).unwrap();
    let next = ExactCompileContext::new(base_context.clone())
        .prepare_compilation_with_private_input(
            &root.path().join("handoff"),
            PRODUCER,
            None,
            Some(handoff),
        )
        .unwrap();
    let encoded = serde_json::to_vec(&(
        next.compiler_inputs().artifacts.capture_graph_selection(),
        next.compiler_inputs().artifacts.binding_dependencies(),
    ))
    .unwrap();
    let (graph, edges): (ArtifactGraphSelection, Vec<_>) =
        serde_json::from_slice(&encoded).unwrap();
    let recovery = ArtifactInventory::default();
    assert_eq!(recovery.node_count(), 0);
    let restored = recovery
        .recover_selection(
            &recovery.empty_view(),
            next.compiler_inputs().artifacts.entries(),
            &graph,
            &edges,
        )
        .unwrap();
    coverage.recoveries += 1;
    for index in 0..2 {
        assert_eq!(
            proofs[index].issued_group_carriers(&restored).unwrap(),
            expected[index]
        );
    }
    assert_eq!(restored.capture_graph_selection(), graph);
    assert_eq!(restored.binding_dependencies(), edges);

    // A dependent product earns its new graph before continuation returns to
    // older support that owns its bytes but none of its issued group nodes.
    let dependent = dependent_product(
        "LaterDependent",
        85,
        vec![(7, vec![source_import(&products[1], 7)])],
    );
    let dependent =
        recovered_witness_fixtures(&[products[0].clone(), products[1].clone(), dependent])
            .into_iter()
            .find(|row| row.product.owner().module == "LaterDependent")
            .unwrap()
            .product;
    let dependent_entry =
        Arc::new(ArtifactEntry::original(producer.sha256(), dependent.clone()).unwrap());
    let support = inventory
        .admit_shared_with_demand(
            &private[0].artifacts,
            vec![dependent_entry.clone()],
            NativeArtifactDemand::ScopeInterfaces,
        )
        .unwrap();
    let produced = OriginalCompilerInputs::issue_complete_native_availability(
        &support,
        &private[0].projection,
        producer.sha256(),
        std::slice::from_ref(&dependent),
        std::slice::from_ref(&proofs[0]),
    )
    .unwrap();
    coverage.dependent_issuances += 1;
    let proof = produced
        .native_original_requirements()
        .iter()
        .find(|proof| proof.original == dependent_entry.descriptor.id)
        .unwrap()
        .clone();
    assert!(
        proof.issued_groups(&support).is_err(),
        "older support cannot already contain the newly issued group"
    );
    let issued_graph = proof.selection.graph_selection();
    let dependent_groups = issued_graph
        .native_groups
        .iter()
        .copied()
        .filter(|group| group.binding.artifact() == dependent_entry.descriptor.id)
        .collect::<Vec<_>>();
    let [dependent_group] = dependent_groups.as_slice() else {
        panic!("one new dependent group")
    };
    assert_eq!(dependent_group.original_ordinal, 7);
    let dependent_carrier = issued_graph
        .namespace
        .iter()
        .copied()
        .find(|binding| binding.artifact == dependent_entry.descriptor.id)
        .unwrap();
    let mut dependent_expected = expected[0].clone();
    assert!(dependent_expected
        .insert(*dependent_group, dependent_carrier)
        .is_none());
    assert_eq!(
        proof.issued_group_carriers(&produced.artifacts).unwrap(),
        dependent_expected
    );
    let (admission, source) =
        generated_source_admission_fixture(&root.path().join("receipt"), &request);
    let continued = produced
        .for_program_continuation(&support, &[admission], &source)
        .unwrap();
    coverage.continuations += 1;
    assert_eq!(
        continued.artifacts.capture_graph_selection().roots,
        support.capture_graph_selection().roots
    );
    assert_eq!(
        continued.artifacts.capture_graph_selection().namespace,
        support.capture_graph_selection().namespace
    );
    assert_eq!(
        proof.issued_group_carriers(&continued.artifacts).unwrap(),
        dependent_expected
    );
    let continued_request = ExactCompileContext::new(base_context)
        .prepare_compilation_with_private_input(
            &root.path().join("continued"),
            PRODUCER,
            None,
            Some(continued),
        )
        .unwrap();
    let continued_inputs = continued_request.compiler_inputs();
    assert!(continued_inputs
        .metadata
        .retained
        .selected_native_groups
        .is_empty());
    let continued_selection =
        crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            continued_inputs,
            &continued_inputs.metadata,
            &validation.inventory,
        )
        .unwrap();
    let (continued_graph, continued_edges): (ArtifactGraphSelection, Vec<_>) =
        serde_json::from_slice(
            &serde_json::to_vec(&(
                continued_inputs.artifacts.capture_graph_selection(),
                continued_inputs.artifacts.binding_dependencies(),
            ))
            .unwrap(),
        )
        .unwrap();
    let fresh = ArtifactInventory::default();
    assert_eq!(fresh.node_count(), 0);
    let reopened = fresh
        .recover_selection(
            &fresh.empty_view(),
            continued_inputs.artifacts.entries(),
            &continued_graph,
            &continued_edges,
        )
        .unwrap();
    coverage.recoveries += 1;
    for view in [&continued_inputs.artifacts, &reopened] {
        assert_eq!(
            proof.issued_group_carriers(view).unwrap(),
            dependent_expected
        );
        let mut fixed = view
            .inventory()
            .bind_segment(view, vec![], &continued_selection)
            .unwrap();
        let imports = [source_import(&dependent, 7)];
        let admitted = view
            .inventory()
            .admit_bound_target(
                &mut fixed,
                view,
                NativeArtifactDemand::CertifiedTargetImports(&imports),
                vec![],
                &continued_selection,
            )
            .unwrap();
        let demanded = admitted
            .selection
            .groups_for_target(&admitted.artifacts, None, &imports)
            .unwrap();
        assert_eq!(demanded, dependent_expected.keys().copied().collect());
        coverage.successful_demands += 1;
    }
}

#[test]
fn private_transport_preserves_independent_closures_until_exact_demand() {
    let mut coverage = NativeTransportCoverage::default();
    for same_root in [false, true] {
        for overlap in [false, true] {
            if same_root && !overlap {
                continue;
            }
            for shared_carrier in [false, true] {
                for reverse in [false, true] {
                    for duplicate in [false, true] {
                        replay_native_transport_history(
                            &NativeTransportHistory {
                                same_root,
                                overlap,
                                shared_carrier,
                                reverse,
                                duplicate,
                                custody: vec![
                                    NativeCustodyOp::Capture { right: false },
                                    NativeCustodyOp::Expand,
                                    NativeCustodyOp::Capture { right: false },
                                    NativeCustodyOp::Capture { right: true },
                                ],
                            },
                            &mut coverage,
                        );
                        coverage.completed += 1;
                    }
                }
            }
        }
    }
    assert_eq!(coverage.completed, 24);
    assert!(coverage.shared_carriers > 0 && coverage.distinct_carriers > 0);
    assert_eq!(coverage.equal_payload_scopes, 16);
    assert_eq!(coverage.captures, 72);
    assert_eq!(coverage.partial_with_ambient_full, 24);
    assert_eq!(coverage.full_without_demand, 24);
    assert_eq!(coverage.preparations, 24);
    assert_eq!(coverage.dependent_issuances, 24);
    assert_eq!(coverage.continuations, 24);
    assert_eq!(coverage.recoveries, 48);
    assert!(coverage.successful_demands > 0 && coverage.ambiguous_demands > 0);
    eprintln!("native transport deterministic support: {coverage:?}");
}

#[test]
fn generated_private_transport_histories_preserve_issued_graphs() {
    use proptest::prelude::*;
    use proptest::test_runner::{
        contextualize_config, Config, FileFailurePersistence, TestCaseError, TestRunner,
    };

    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 32;
    }
    if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
        config.max_shrink_iters = 4096;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    let mut config = contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::generated_private_transport_histories_preserve_issued_graphs"
    ));
    let cases = config.cases;
    let strategy = (
        0u8..3,
        any::<[bool; 3]>(),
        prop::collection::vec(
            prop_oneof![
                Just(NativeCustodyOp::Expand),
                any::<bool>().prop_map(|right| NativeCustodyOp::Capture { right })
            ],
            0..9,
        ),
    )
        .prop_map(|(shape, [shared_carrier, reverse, duplicate], tail)| {
            let mut custody = vec![
                NativeCustodyOp::Capture { right: false },
                NativeCustodyOp::Expand,
                NativeCustodyOp::Capture { right: false },
                NativeCustodyOp::Capture { right: true },
            ];
            custody.extend(tail);
            NativeTransportHistory {
                same_root: shape == 2,
                overlap: shape != 0,
                shared_carrier,
                reverse,
                duplicate,
                custody,
            }
        });
    let coverage = std::cell::RefCell::new(NativeTransportCoverage::default());
    let result = TestRunner::new(config).run(&strategy, |history| {
        coverage.borrow_mut().callbacks += 1;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            replay_native_transport_history(&history, &mut coverage.borrow_mut());
        }));
        if let Err(panic) = outcome {
            let reason = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    panic
                        .downcast_ref::<&str>()
                        .map(|message| (*message).to_owned())
                })
                .unwrap_or_else(|| "non-string panic".into());
            return Err(TestCaseError::fail(format!(
                "history={history:?}: {reason}"
            )));
        }
        coverage.borrow_mut().completed += 1;
        Ok(())
    });
    eprintln!(
        "native transport campaign configured_cases={cases} coverage={:?}",
        coverage.borrow()
    );
    result.unwrap();
}

#[test]
fn request_subset_and_empty_availability_cannot_borrow_custody_during_target_admission() {
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let native = product("BoundedRequest", 117);
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
    let unrelated = product("UnofferedCustody", 118);
    let unrelated_entry =
        Arc::new(ArtifactEntry::original(producer.sha256(), unrelated.clone()).unwrap());
    let inventory = ArtifactInventory::default();
    let full = inventory
        .admit_shared(
            &inventory.empty_view(),
            vec![entry.clone(), unrelated_entry.clone()],
        )
        .unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let root = tempfile::tempdir().unwrap();
    for ordinal in [None, Some(7)] {
        let selected = inventory
            .admit_recovery_selection(
                &inventory.empty_view(),
                vec![entry.clone()],
                &ordinal
                    .into_iter()
                    .map(|original_ordinal| NativeGroupKey {
                        artifact: entry.descriptor.id,
                        original_ordinal,
                    })
                    .collect(),
            )
            .unwrap();
        let custody = full
            .merge(&selected)
            .unwrap()
            .retain_original_custody(&selected)
            .unwrap();
        let requirements = crate::artifact_inventory::NativeOriginalRequirement::for_projection(
            &projection,
            &selected,
            &[],
        )
        .unwrap();
        let offer = crate::artifact_inventory::IssuedNativeOffer::issue(
            entry.descriptor.id,
            &selected,
            requirements.first().cloned(),
        )
        .unwrap();
        let private = OriginalCompilerInputs {
            projection: projection.clone(),
            artifacts: custody.clone(),
            native_offers: vec![offer],
        };
        let context = Arc::new(ExactDeclarationContext::from_facts(
            DeclarationContextFacts {
                producer: producer.sha256(),
                compiler_support: fixture_compiler_support(
                    projection
                        .interface_only()
                        .merge(
                            &CompilerInputProjection::from_issued_entries(&[
                                unrelated_entry.clone()
                            ])
                            .unwrap()
                            .interface_only(),
                        )
                        .unwrap(),
                    custody,
                ),
                lexical: Default::default(),
                template_imports: None,
                original_instance_environment: OriginalInstanceEnvironment::Unknown,
            },
        ));
        let request = ExactCompileContext::new(context)
            .prepare_compilation_with_private_input(
                &root.path().join(format!("{ordinal:?}")),
                PRODUCER,
                None,
                Some(private),
            )
            .unwrap();
        let inputs = request.compiler_inputs();
        let validation = PackageInterfaceValidation::default();
        let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            inputs,
            &inputs.metadata,
            &validation.inventory,
        )
        .unwrap();
        for demanded in [7, 9001] {
            let mut fixed = inventory
                .bind_segment(&inputs.artifacts, vec![], &selection)
                .unwrap();
            let imports = [source_import(&native, demanded)];
            let admitted = inventory.admit_bound_target(
                &mut fixed,
                &inputs.artifacts,
                crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(&imports),
                vec![],
                &selection,
            );
            let admitted = admitted.map(|_| ());
            if ordinal == Some(demanded) {
                assert!(admitted.is_ok());
            } else {
                assert!(
                    matches!(&admitted, Err(CompileError::ArtifactInventory(error))
                    if matches!(error.failure, ArtifactInventoryFailure::NativeGroupUnavailable { artifact, original_ordinal }
                        if artifact == entry.descriptor.id && original_ordinal == demanded)),
                    "offer={ordinal:?}, demand={demanded}, actual={admitted:?}"
                );
            }
        }
        let mut fixed = inventory
            .bind_segment(&inputs.artifacts, vec![], &selection)
            .unwrap();
        let imports = [source_import(&unrelated, 7)];
        let denied = inventory.admit_bound_target(
            &mut fixed,
            &inputs.artifacts,
            crate::artifact_inventory::NativeArtifactDemand::CertifiedTargetImports(&imports),
            vec![],
            &selection,
        );
        assert!(
            matches!(denied, Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::NativeGroupUnavailable { artifact, original_ordinal: 7 }
                if artifact == unrelated_entry.descriptor.id)),
            "unoffered dependency custody cannot authorize native demand"
        );
    }
}

#[test]
fn recovered_native_offer_preserves_full_partial_and_empty_issuance_modes() {
    use crate::artifact_inventory::{
        CompilerSourceSupport, IssuedNativeOffer, NativeOriginalRequirement,
    };
    use crate::exact_context::{hydrate_context, Materializer, NativeOfferSnapshotBody};
    use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};

    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let native = product("RestoredOffer", 87);
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), native).unwrap());
    let inventory = ArtifactInventory::default();
    let artifacts = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone()])
        .unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let proof = NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &artifacts)
        .unwrap()
        .unwrap();
    let full =
        IssuedNativeOffer::issue(entry.descriptor.id, &artifacts, Some(proof.clone())).unwrap();
    assert!(full.is_full());
    // Namespace withdrawal narrows an actual issuance. Restoring the original
    // complete namespace later must not reverse that decision.
    let partial = full
        .within_projection(&projection, &inventory.empty_view())
        .unwrap()
        .unwrap();
    assert!(!partial.is_full());
    assert!(proof.qualify_availability(&artifacts).unwrap().is_some());
    let empty = IssuedNativeOffer::issue(entry.descriptor.id, &artifacts, None).unwrap();
    let canonical = ArtifactEntry::canonical(match &entry.payload {
        ArtifactPayload::Original(product) => product.module_interface().unwrap().clone(),
        _ => unreachable!(),
    })
    .descriptor
    .id;
    assert!(empty
        .custody()
        .artifact_ids()
        .contains(&entry.descriptor.id));
    assert!(empty.custody().artifact_ids().contains(&canonical));
    assert!(empty.custody().selected_native_groups().is_empty());
    assert!(empty
        .custody()
        .capture_graph_selection()
        .native_groups
        .is_empty());
    assert!(empty.custody().capture_graph_selection().roots.is_empty());
    assert!(empty
        .custody()
        .capture_graph_selection()
        .namespace
        .is_empty());
    let canonical_support = CompilerSourceSupport::new(
        projection.interface_only(),
        artifacts.clone(),
        [empty.clone()],
    )
    .unwrap()
    .source_support(&BTreeSet::from([entry.descriptor.owner.clone()]))
    .unwrap();
    assert!(canonical_support
        .projection
        .roles()
        .iter()
        .all(|role| role.original().is_none()));
    assert_eq!(canonical_support.native_offers().len(), 1);
    let empty_restored = canonical_support.native_offers()[0]
        .snapshot_facts()
        .restore(&canonical_support.artifacts)
        .unwrap();
    assert_eq!(empty_restored.snapshot_facts(), empty.snapshot_facts());
    let artifacts = artifacts
        .interface_projection(&[entry.descriptor.owner.clone()])
        .unwrap()
        .with_compiler_projection(&projection)
        .unwrap();
    assert!(artifacts.selected_native_groups().is_empty());
    assert!(proof.qualify_availability(&artifacts).unwrap().is_some());
    let root = tempfile::tempdir().unwrap();

    for (index, offer) in [full, partial, empty].into_iter().enumerate() {
        let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: CompilerSourceSupport::new(
                projection.clone(),
                artifacts.clone(),
                [offer.clone()],
            )
            .unwrap(),
            lexical: Default::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        });
        let snapshot = context.snapshot();
        let store = MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let receipt = materializer.materialize_context(None, &snapshot).unwrap();
        for (cid, bytes) in materializer.finish().unwrap().into_blocks() {
            store.put_keyed(&cid, &bytes).unwrap();
        }
        let receipt = hydrate_context(&store, receipt.root()).unwrap();
        let snapshot = receipt.snapshot();
        let graph = snapshot.graph_selection().unwrap();
        let view = context.artifact_view();
        // A fresh acquisition reconstructs the exact graph from authenticated
        // fixture entries, then restores the decoded offer through production.
        let recovered = RecoveredArtifactInventory {
            producer: producer.sha256(),
            entries: view
                .entries()
                .into_iter()
                .map(|entry| (entry.descriptor.id, entry))
                .collect(),
            recorded_inventory: true,
            bindings: graph.bindings.iter().copied().collect(),
            binding_edges: snapshot.binding_dependencies().unwrap(),
            interfaces: view.interface_dependencies(),
        };
        let mut restored = recovered.context_with_snapshot(snapshot).unwrap();
        restored.bind_snapshot_baseline(&receipt).unwrap();
        let restored_offer = &restored.facts.compiler_support.native_offers()[0];
        assert_eq!(restored_offer.snapshot_facts(), offer.snapshot_facts());
        assert_eq!(restored_offer.is_full(), index == 0);
        assert!(proof
            .qualify_availability(restored.artifact_view())
            .unwrap()
            .is_some());
        assert!(match (&restored_offer.snapshot_facts().body, index) {
            (NativeOfferSnapshotBody::Full(_), 0)
            | (NativeOfferSnapshotBody::Partial(_), 1)
            | (NativeOfferSnapshotBody::Empty(_), 2) => true,
            _ => false,
        });
        let request = ExactCompileContext::new(Arc::new(restored))
            .prepare_compilation(&root.path().join(format!("restored-{index}")), PRODUCER)
            .unwrap();
        assert_eq!(
            wire_mode(&request, "RestoredOffer"),
            if index == 0 {
                full_mode()
            } else if index == 1 {
                issued_mode(&[7, 9001])
            } else {
                issued_mode(&[])
            }
        );
        assert_eq!(
            request.context().artifact_view().selected_native_groups(),
            context.artifact_view().selected_native_groups()
        );
    }
}

#[test]
fn empty_offer_snapshot_retains_exact_carrier_beside_competing_namespace() {
    use crate::artifact_inventory::{
        CompilerSourceSupport, IssuedNativeOffer, NativeArtifactDemand,
    };
    use crate::exact_context::{hydrate_context, Materializer};
    use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};

    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let entry =
        Arc::new(ArtifactEntry::original(producer.sha256(), product("EmptyCarrier", 88)).unwrap());
    let inventory = ArtifactInventory::default();
    let issue = |marker: &str, version| {
        inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![
                    entry.clone(),
                    Arc::new(ArtifactEntry::canonical(
                        product(marker, version).module_interface().unwrap().clone(),
                    )),
                ],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap()
    };
    let left = issue("EmptyLeft", 89);
    let right = issue("EmptyRight", 90);
    let left_offer = IssuedNativeOffer::issue(entry.descriptor.id, &left, None).unwrap();
    let right_offer = IssuedNativeOffer::issue(entry.descriptor.id, &right, None).unwrap();
    assert_ne!(
        left_offer.custody().capture_graph_selection(),
        right_offer.custody().capture_graph_selection()
    );
    assert_ne!(
        left_offer.snapshot_facts().identity().unwrap(),
        right_offer.snapshot_facts().identity().unwrap()
    );
    assert!(
        left_offer.snapshot_facts().restore(&right).is_err(),
        "a competing carrier cannot satisfy exact Empty custody"
    );
    let artifacts = left
        .merge(&right)
        .unwrap()
        .retain_original_custody(&right)
        .unwrap();
    let restored_left = left_offer.snapshot_facts().restore(&artifacts).unwrap();
    assert_eq!(restored_left.snapshot_facts(), left_offer.snapshot_facts());
    assert!(restored_left.proof().is_none());
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()])
        .unwrap()
        .interface_only();
    let support = CompilerSourceSupport::new(
        projection,
        artifacts,
        [left_offer.clone(), right_offer.clone()],
    )
    .unwrap()
    .source_support(&BTreeSet::from([entry.descriptor.owner.clone()]))
    .unwrap();
    let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
        producer: producer.sha256(),
        compiler_support: support,
        lexical: Default::default(),
        template_imports: None,
        original_instance_environment: OriginalInstanceEnvironment::Unknown,
    });
    let store = MemoryBlockstore::new();
    let mut materializer = Materializer::new(&store);
    let receipt = materializer
        .materialize_context(None, &context.snapshot())
        .unwrap();
    for (cid, bytes) in materializer.finish().unwrap().into_blocks() {
        store.put_keyed(&cid, &bytes).unwrap();
    }
    let receipt = hydrate_context(&store, receipt.root()).unwrap();
    assert_eq!(receipt.snapshot().native_offers().count(), 2);
    let mut shared_scopes = 0;
    for fact in receipt.snapshot().native_offers() {
        let crate::exact_context::NativeOfferSnapshotBody::Empty(custody) = &fact.body else {
            panic!("Empty issuance");
        };
        assert!(!Arc::ptr_eq(
            custody.snapshot(),
            receipt.snapshot().selection()
        ));
        for (scope, proof) in &custody.snapshot().scopes {
            let context_proof = &receipt.snapshot().selection().scopes[scope];
            assert!(Arc::ptr_eq(proof.proof(), context_proof.proof()));
            shared_scopes += 1;
        }
    }
    assert!(
        shared_scopes >= 2,
        "distinct offer selections must share their overlapping scope owners"
    );

    let graph = receipt.snapshot().graph_selection().unwrap();
    let view = context.artifact_view();
    let recovered = RecoveredArtifactInventory {
        producer: producer.sha256(),
        entries: view
            .entries()
            .into_iter()
            .map(|entry| (entry.descriptor.id, entry))
            .collect(),
        recorded_inventory: true,
        bindings: graph.bindings.iter().copied().collect(),
        binding_edges: receipt.snapshot().binding_dependencies().unwrap(),
        interfaces: view.interface_dependencies(),
    };
    let mut restored = recovered.context_with_snapshot(receipt.snapshot()).unwrap();
    restored.bind_snapshot_baseline(&receipt).unwrap();
    let expected = [left_offer.snapshot_facts(), right_offer.snapshot_facts()]
        .into_iter()
        .map(|fact| (fact.identity().unwrap(), fact))
        .collect::<BTreeMap<_, _>>();
    let actual = restored
        .facts
        .compiler_support
        .native_offers()
        .iter()
        .map(|offer| {
            assert!(offer.proof().is_none());
            assert!(!offer.is_full());
            assert!(offer.custody().selected_native_groups().is_empty());
            let fact = offer.snapshot_facts();
            (fact.identity().unwrap(), fact)
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual, expected);
    assert!(restored
        .compiler_input_roles()
        .iter()
        .all(|role| role.original().is_none()));
    assert!(restored.artifact_view().selected_native_groups().is_empty());
}
