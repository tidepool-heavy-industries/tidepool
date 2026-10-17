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
    crate::declaration_context::issue_fixture_compiler_support(projection, artifacts).unwrap()
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

fn ordered_offer_retention(
    context: &ArtifactView,
    offers: &[crate::artifact_inventory::IssuedNativeOffer],
) -> Result<ArtifactView, CompileError> {
    offers.iter().try_fold(context.clone(), |view, offer| {
        view.merge_fixture_selections(offer.custody())
    })
}

fn assert_retention_result_matches(
    expected: Result<ArtifactView, CompileError>,
    actual: Result<ArtifactView, CompileError>,
) {
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => {
            assert_eq!(
                actual.capture_graph_selection(),
                expected.capture_graph_selection()
            );
            assert_eq!(
                actual.binding_dependencies(),
                expected.binding_dependencies()
            );
            assert_eq!(actual.entries(), expected.entries());
            assert_eq!(
                actual.selected_native_groups(),
                expected.selected_native_groups()
            );
        }
        (
            Err(CompileError::ArtifactInventory(expected)),
            Err(CompileError::ArtifactInventory(actual)),
        ) => {
            assert_eq!(actual.failure, expected.failure);
            let evidence = |error: &crate::artifact_inventory::ArtifactInventoryError| {
                error.owner_conflict.as_ref().map(|evidence| {
                    match evidence {
                    crate::artifact_inventory::ArtifactOwnerConflictEvidence::ArtifactSelection {
                        existing, incoming, existing_in_parent,
                    } => (existing.descriptor.id, incoming.descriptor.id, *existing_in_parent),
                    _ => panic!("merge conflict must retain artifact selection evidence"),
                }
                })
            };
            assert_eq!(evidence(&actual), evidence(&expected));
        }
        (expected, actual) => panic!(
            "retention differs from ordered merge fold: expected={expected:?}, actual={actual:?}"
        ),
    }
}

fn assert_offer_retention_matches_ordered_fold(
    context: &ArtifactView,
    projection: &CompilerInputProjection,
    offers: &[crate::artifact_inventory::IssuedNativeOffer],
) {
    for end in 0..=offers.len() {
        let prefix = &offers[..end];
        assert_retention_result_matches(
            ordered_offer_retention(context, prefix),
            retain_native_offers(context, prefix),
        );
        assert_retention_result_matches(
            ordered_offer_retention(context, prefix),
            recover_fixture_compiler_support(
                projection.clone(),
                context.clone(),
                prefix.iter().cloned(),
            )
            .map(|support| support.artifacts().clone()),
        );
    }
}

fn replay_offer_retention(indices: &[usize], foreign: bool, baseline_empty: bool) {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let inventory = ArtifactInventory::default();
    let entries = (0..3)
        .map(|index| {
            Arc::new(
                ArtifactEntry::original(
                    producer.sha256(),
                    product(&format!("Retention{index}"), 91 + index),
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let baseline = inventory
        .admit_shared(&inventory.empty_view(), vec![entries[0].clone()])
        .unwrap();
    let mut offers = Vec::new();
    for entry in &entries {
        let issuer = if foreign {
            ArtifactInventory::default()
        } else {
            inventory.clone()
        };
        let source = issuer
            .admit_shared(&issuer.empty_view(), vec![entry.clone()])
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
        let proof = NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &source)
            .unwrap()
            .unwrap();
        let full = IssuedNativeOffer::issue(entry.descriptor.id, &source, Some(proof)).unwrap();
        assert!(full.is_full());
        let partial = full
            .within_projection(&projection, &issuer.empty_view())
            .unwrap()
            .unwrap();
        assert!(!partial.is_full());
        let empty = IssuedNativeOffer::issue(entry.descriptor.id, &source, None).unwrap();
        offers.extend([full, partial, empty]);
    }
    let selected = indices
        .iter()
        .map(|index| offers[*index % offers.len()].clone())
        .collect::<Vec<_>>();
    let context = if baseline_empty {
        inventory.empty_view()
    } else {
        baseline
    };
    let projection = CompilerInputProjection::from_issued_entries(&context.root_entries()).unwrap();
    assert_offer_retention_matches_ordered_fold(&context, &projection, &selected);
}

#[test]
fn issued_offer_retention_matches_ordered_fold_across_inventory_and_mode_histories() {
    for foreign in [false, true] {
        for empty in [false, true] {
            for indices in [
                vec![],
                vec![0, 1, 2],
                vec![8, 7, 6, 5, 4, 3, 2, 1, 0],
                vec![0, 0, 3, 0, 6, 3, 0],
            ] {
                replay_offer_retention(&indices, foreign, empty);
            }
        }
    }
}

#[test]
fn generated_issued_offer_retention_preserves_every_ordered_prefix() {
    use proptest::prelude::*;
    use proptest::test_runner::{contextualize_config, Config, TestCaseError, TestRunner};
    let config = Config {
        cases: 32,
        source_file: Some(file!()),
        test_name: Some(concat!(
            module_path!(),
            "::generated_issued_offer_retention_preserves_every_ordered_prefix"
        )),
        ..Config::default()
    };
    TestRunner::new(contextualize_config(config))
        .run(
            &(
                prop::collection::vec(0usize..9, 0..17),
                any::<bool>(),
                any::<bool>(),
            ),
            |(indices, foreign, empty)| {
                std::panic::catch_unwind(|| replay_offer_retention(&indices, foreign, empty))
                    .map_err(|panic| {
                        let reason = panic
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| {
                                panic
                                    .downcast_ref::<&str>()
                                    .map(|value| (*value).to_owned())
                            })
                            .unwrap_or_else(|| "non-string panic".into());
                        TestCaseError::fail(format!(
                            "indices={indices:?}, foreign={foreign}, empty={empty}: {reason}"
                        ))
                    })
            },
        )
        .unwrap();
}

#[test]
fn issued_offer_retention_records_cumulative_prefix_work() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let inventory = ArtifactInventory::default();
    let entries = (0..32)
        .map(|index| {
            Arc::new(
                ArtifactEntry::original(
                    producer.sha256(),
                    product(&format!("RetainedWork{index}"), 90),
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let source = inventory
        .admit_shared(&inventory.empty_view(), entries.clone())
        .unwrap();
    let offers = entries
        .iter()
        .map(|entry| {
            let proof = NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &source)
                .unwrap()
                .unwrap();
            IssuedNativeOffer::issue(entry.descriptor.id, &source, Some(proof)).unwrap()
        })
        .collect::<Vec<_>>();
    // Warm only immutable inputs. The cumulative result views have not been read.
    source.entries();
    for offer in &offers {
        offer.custody().entries();
    }
    let mut current = source.clone();
    let before = inventory.metrics();
    for (index, offer) in offers.iter().enumerate() {
        let previous = inventory.metrics();
        current = current.merge_fixture_selections(offer.custody()).unwrap();
        let merged = inventory.metrics();
        current.entries();
        let observed = inventory.metrics();
        eprintln!("ordered_prefix={index} merge_graph_visits={} merge_entry_copies={} owner_lookups={} result_graph_visits={} result_entry_copies={}",
            merged.graph_visits - previous.graph_visits, merged.entry_handle_copies - previous.entry_handle_copies,
            merged.admission_owner_lookups - previous.admission_owner_lookups,
            observed.graph_visits - merged.graph_visits, observed.entry_handle_copies - merged.entry_handle_copies);
    }
    let after = inventory.metrics();
    assert_eq!(
        current.capture_graph_selection().native_groups,
        source.capture_graph_selection().native_groups
    );
    eprintln!(
        "ordered_fold_total graph_visits={} entry_copies={} owner_lookups={}",
        after.graph_visits - before.graph_visits,
        after.entry_handle_copies - before.entry_handle_copies,
        after.admission_owner_lookups - before.admission_owner_lookups
    );

    let before = inventory.metrics();
    let expected = ordered_offer_retention(&source, &offers).unwrap();
    expected.entries();
    let old = inventory.metrics();
    let before_batch = inventory.metrics();
    let actual = retain_native_offers(&source, &offers).unwrap();
    actual.entries();
    let batched = inventory.metrics();
    assert_retention_result_matches(Ok(expected), Ok(actual));
    let old_visits = old.graph_visits - before.graph_visits;
    let batch_visits = batched.graph_visits - before_batch.graph_visits;
    let old_lookups = old.admission_owner_lookups - before.admission_owner_lookups;
    let batch_lookups = batched.admission_owner_lookups - before_batch.admission_owner_lookups;
    eprintln!("retention_work_comparison ordered_graph_visits={old_visits} batch_graph_visits={batch_visits} ordered_owner_lookups={old_lookups} batch_owner_lookups={batch_lookups} ordered_entry_copies={} batch_entry_copies={}", old.entry_handle_copies - before.entry_handle_copies, batched.entry_handle_copies - before_batch.entry_handle_copies);
    assert!(
        batch_visits <= old_visits,
        "batched custody retention must not add read-projection work"
    );
    assert!(
        batch_lookups < old_lookups,
        "covered prefixes must reuse their successful validation"
    );
}

#[test]
fn issued_offer_retention_preserves_historical_canonical_custody() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let current = product("RetentionConflict", 92);
    let changed = fixture_finalized_product(
        original_groups_fixture_with_interface(
            "RetentionConflict",
            vec![(7, vec![]), (9001, vec![])],
            93,
            &BTreeMap::new(),
            b"different certified interface".to_vec(),
        ),
        producer.sha256(),
    );
    let changed = recovered_witness_fixtures(&[changed]).remove(0).product;
    let inventory = ArtifactInventory::default();
    let current_entry = Arc::new(ArtifactEntry::original(producer.sha256(), current).unwrap());
    let context = inventory
        .admit_shared(&inventory.empty_view(), vec![current_entry.clone()])
        .unwrap();
    let issue = |source: &ArtifactView, entry: &ArtifactEntry| {
        let proof = NativeOriginalRequirement::from_selected_view(entry.descriptor.id, source)
            .unwrap()
            .unwrap();
        IssuedNativeOffer::issue(entry.descriptor.id, source, Some(proof)).unwrap()
    };
    let current_offer = issue(&context, &current_entry);
    let other_inventory = ArtifactInventory::default();
    let changed_entry = Arc::new(ArtifactEntry::original(producer.sha256(), changed).unwrap());
    let changed_source = other_inventory
        .admit_shared(&other_inventory.empty_view(), vec![changed_entry.clone()])
        .unwrap();
    let changed_offer = issue(&changed_source, &changed_entry);
    let offers = vec![
        current_offer.clone(),
        current_offer.clone(),
        changed_offer,
        current_offer,
    ];
    let projection = CompilerInputProjection::from_issued_entries(&[current_entry]).unwrap();
    let error = context
        .merge_fixture_selections(&changed_source)
        .unwrap_err();
    let CompileError::ArtifactInventory(error) = error else {
        panic!("expected typed conflict, got {error:?}");
    };
    assert!(matches!(
        error.failure,
        crate::artifact_inventory::ArtifactInventoryFailure::OwnerConflict { .. }
    ));
    for end in 0..=offers.len() {
        let retained = recover_fixture_compiler_support(
            projection.clone(),
            context.clone(),
            offers[..end].iter().cloned(),
        )
        .unwrap();
        assert_eq!(
            retained.artifacts().capture_graph_selection().namespace,
            context.capture_graph_selection().namespace
        );
        assert_eq!(
            retained.artifacts().capture_graph_selection().roots,
            context.capture_graph_selection().roots
        );
        for offer in &offers[..end] {
            let actual = retained.artifacts().capture_graph_selection();
            let expected = offer.custody().capture_graph_selection();
            assert!(expected
                .bindings
                .iter()
                .all(|node| actual.bindings.contains(node)));
            assert!(offer
                .custody()
                .binding_dependencies()
                .iter()
                .all(|edge| retained.artifacts().binding_dependencies().contains(edge)));
        }
    }
}

#[test]
fn issued_offer_retention_preserves_materialization_parent_precedence() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let inventory = ArtifactInventory::default();
    let entry = Arc::new(
        ArtifactEntry::original(producer.sha256(), product("RetentionFiles", 94)).unwrap(),
    );
    let source = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone()])
        .unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
        producer: producer.sha256(),
        compiler_support: recover_fixture_compiler_support(projection.clone(), source.clone(), [])
            .unwrap(),
        lexical: Default::default(),
        template_imports: None,
        original_instance_environment: OriginalInstanceEnvironment::Unknown,
    });
    let proof = NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &source)
        .unwrap()
        .unwrap();
    let offers = (0..4)
        .map(|_| {
            IssuedNativeOffer::issue(entry.descriptor.id, &source, Some(proof.clone())).unwrap()
        })
        .collect::<Vec<_>>();
    let mut completed = Vec::new();
    for offer in &offers {
        let metadata = projection
            .project_metadata(offer.custody().metadata_snapshot())
            .unwrap();
        let owner = offer
            .custody()
            .retain_materialization(&metadata, |parents| {
                context.materialize_retained_artifacts(
                    &metadata,
                    parents,
                    tempfile::tempdir().unwrap(),
                )
            })
            .unwrap();
        completed.push(owner);
    }
    let observe_parents = |view: &ArtifactView| {
        let metadata = projection
            .project_metadata(view.metadata_snapshot())
            .unwrap();
        let mut observed = None;
        let result = view.retain_materialization(&metadata, |parents| {
            observed = Some(parents);
            Err(failure(
                "observe inherited materialization owners without publishing a result",
            ))
        });
        assert!(result.is_err());
        observed.expect("new merge has no completed local materialization")
    };
    for order in [vec![0, 1, 2, 3], vec![3, 2, 1, 0], vec![1, 0, 1, 3, 2, 0]] {
        let selected = order
            .iter()
            .map(|index| offers[*index].clone())
            .collect::<Vec<_>>();
        let expected = ordered_offer_retention(&source, &selected).unwrap();
        let actual = retain_native_offers(&source, &selected).unwrap();
        let expected_parents = observe_parents(&expected);
        let actual_parents = observe_parents(&actual);
        assert_eq!(actual_parents.len(), expected_parents.len());
        assert_eq!(actual_parents.len(), completed.len());
        for (actual, expected) in actual_parents.iter().zip(&expected_parents) {
            assert!(
                Arc::ptr_eq(actual, expected),
                "exact completed file owner and precedence must match the ordered fold"
            );
            assert!(actual.directory().path().exists());
        }
        assert_retention_result_matches(Ok(expected), Ok(actual));
    }
}

#[test]
fn private_fragment_closes_baseline_only_interface_dependencies_at_request() {
    let baseline_product = product("BaselineOnly", 87);
    let baseline = type_context(std::slice::from_ref(&baseline_product));
    let dependency = identity("fixture", "BaselineOnly");
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let consumer = crate::certified_products::fixture_finalized_product_with_requirements(
        original_groups_fixture_with_interface(
            "PrivateFragment",
            vec![(7, vec![])],
            88,
            &BTreeMap::new(),
            b"private fragment interface".to_vec(),
        ),
        producer.sha256(),
        Some(BTreeMap::from([(
            (dependency.unit.clone(), dependency.module.clone()),
            baseline_product
                .module_interface()
                .unwrap()
                .interface_sha256(),
        )])),
    );
    let consumer = recovered_witness_fixtures(&[consumer]).remove(0).product;
    let private =
        OriginalCompilerInputs::from_native_availability(&baseline, producer, &[consumer])
            .expect("the private issuer may use receiving baseline type evidence");
    let offered = identity("fixture", "PrivateFragment");
    assert_eq!(
        private
            .projection
            .snapshot_roles()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([offered.clone()])
    );
    assert!(
        matches!(private.projection.validate(&private.artifacts),
        Err(CompileError::ArtifactInventory(error))
        if matches!(&error.failure, ArtifactInventoryFailure::MissingDependency { required, .. }
            if required == &dependency)),
        "the issued fragment intentionally leaves its baseline dependency unoffered"
    );
    assert_eq!(private.native_offers.len(), 1);
    let proof = private.native_offers[0]
        .proof()
        .expect("the owner issued its actual empty group");
    let groups = proof
        .issued_groups(private.native_offers[0].custody())
        .unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups.first().unwrap().original_ordinal, 7);
    assert!(private.native_offers[0].is_full());
    let before = baseline.semantic_sha256();
    let roots = baseline.artifact_view().selected_native_groups();
    let original = private.native_offers[0].original();
    let root = tempfile::tempdir().unwrap();
    let request = ExactCompileContext::new(baseline.clone())
        .prepare_compilation_with_private_input(root.path(), PRODUCER, None, Some(private.clone()))
        .expect("the final request closes the fragment against receiving baseline roles");
    let final_inputs = request.compiler_inputs();
    final_inputs
        .projection
        .validate(&final_inputs.artifacts)
        .unwrap();
    assert_eq!(
        final_inputs
            .projection
            .snapshot_roles()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([dependency.clone(), offered.clone()])
    );
    assert!(final_inputs.projection.snapshot_roles()[&dependency]
        .original()
        .is_none());
    assert_eq!(
        final_inputs.artifacts.selected_native_groups(),
        roots,
        "private availability cannot add executable body roots"
    );
    let final_proof = final_inputs
        .native_original_requirements()
        .into_iter()
        .find(|proof| proof.original == original)
        .unwrap();
    assert_eq!(
        final_proof.issued_groups(&final_inputs.artifacts).unwrap(),
        groups
    );
    assert_eq!(baseline.semantic_sha256(), before);
    assert!(
        !baseline
            .compiler_input_projection()
            .snapshot_roles()
            .contains_key(&identity("fixture", "PrivateFragment")),
        "private request composition cannot publish its fragment into declarations"
    );
    let required_interface =
        baseline.compiler_input_projection().snapshot_roles()[&dependency].interface();
    assert!(
        private
            .artifacts
            .artifact_ids()
            .contains(&required_interface),
        "the refusal control retains the exact dependency bytes privately"
    );
    let empty = ExactDeclarationContext::new(&[], &[], vec![])
        .unwrap()
        .admit_producer(producer.sha256())
        .unwrap();
    let refused = ExactCompileContext::new(Arc::new(empty)).prepare_compilation_with_private_input(
        &root.path().join("missing-baseline"),
        PRODUCER,
        None,
        Some(private),
    );
    let error = match refused {
        Err(error) => error,
        Ok(_) => panic!("private dependency custody cannot replace a receiving baseline role"),
    };
    assert!(
        matches!(error, CompileError::ArtifactInventory(error)
        if matches!(&error.failure, ArtifactInventoryFailure::MissingDependency {
            artifact, dependent, required, dependency: ArtifactDependency::Interface,
        } if *artifact == original && dependent == &offered && required == &dependency)),
        "final request must report the unclosed fragment dependency, not an owner conflict"
    );
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
        .merge_fixture_selections(&right)
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
        .try_fold(view.clone(), |view, partial| {
            view.merge_fixture_selections(partial)
        })
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
    let competing_roots = left.merge_fixture_selections(&right).unwrap();
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
    let other_support = recover_fixture_compiler_support(baseline.clone(), other.clone(), [])
        .unwrap()
        .selected_interfaces()
        .unwrap()
        .support_for_owners(vec![canonical.descriptor.owner.clone()])
        .unwrap();
    let other_interface = other_support.artifacts().clone();
    for selected in [&other, &other_interface] {
        let mismatch = left
            .merge_fixture_selections(selected)
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
                                .artifacts()
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
    let admitted = private
        .projection
        .admit_interfaces_with_native_offers(&private.artifacts, vec![], &private.native_offers)
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
    let selected = request
        .projection
        .admit_interfaces_with_native_offers(&request.artifacts, vec![], &offers)
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
        .projection()
        .merge(&CompilerInputProjection::from_issued_entries(&value_interfaces).unwrap())
        .unwrap();
    let view = context
        .artifact_view()
        .inventory()
        .admit_shared(context.artifact_view(), value_interfaces)
        .unwrap();
    let support = fixture_compiler_support(projection, view);
    let context = Arc::new(Arc::unwrap_or_clone(context).rebuild(|facts| {
        facts.compiler_support = support;
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
    projection
        .admit_interfaces_with_native_offers(&private.artifacts, vec![], &private.native_offers)
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
        selected_projection
            .admit_interfaces_with_native_offers(
                &private.artifacts,
                vec![replacement],
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
        .projection()
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
        let merged = issued.merge_fixture_selections(&current);
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
            .artifacts()
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
        &full,
    )
    .unwrap()
    .expect("complete authored census was issued before demotion");
    let offer = crate::artifact_inventory::IssuedNativeOffer::issue(
        authored_id,
        &full,
        Some(proof.clone()),
    )
    .unwrap();
    assert!(offer.is_full());
    let owners = entries
        .iter()
        .map(|entry| entry.descriptor.owner.clone())
        .collect::<Vec<_>>();
    let issued = recover_fixture_compiler_support(
        CompilerInputProjection::from_issued_entries(&entries).unwrap(),
        full.clone(),
        [],
    )
    .unwrap();
    let narrowed = issued
        .selected_interfaces()
        .unwrap()
        .support_for_owners(owners)
        .unwrap()
        .retain_offers([offer])
        .unwrap();
    let context = Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: narrowed,
            lexical: crate::exact_context::ExactLexicalSnapshot::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        },
    ));
    let before = context.as_ref().clone();
    assert!(context
        .facts
        .compiler_support
        .artifacts()
        .selected_native_groups()
        .is_empty());
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
        let updated = context
            .compiler_source_support()
            .for_source_owners(&BTreeSet::new())
            .unwrap();
        context = context.rebuild(|facts| facts.compiler_support = updated);
    }
    assert!(context
        .compiler_input_roles()
        .iter()
        .all(|role| role.original().is_none()));
    assert!(context.artifact_view().selected_native_groups().is_empty());
    assert_eq!(
        context
            .artifact_view()
            .capture_graph_selection()
            .native_groups
            .iter()
            .map(|group| group.original_ordinal)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([7, 9001]),
        "the complete issued body remains custody without executable demand",
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
    let view = changed
        .artifact_view()
        .inventory()
        .admit_shared(
            &changed.artifact_view().inventory().empty_view(),
            vec![entry.clone()],
        )
        .unwrap();
    let support = fixture_compiler_support(
        CompilerInputProjection::from_issued_entries(&[entry]).unwrap(),
        view,
    );
    changed = changed.rebuild(|facts| facts.compiler_support = support);
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
            .artifacts()
            .inventory()
            .admit_shared(
                &ambiguous.facts.compiler_support.artifacts(),
                entries.clone(),
            )
            .unwrap();
        let support = recover_fixture_compiler_support(
            ambiguous.compiler_input_projection().clone(),
            updated,
            ambiguous
                .compiler_source_support()
                .native_offers()
                .iter()
                .cloned(),
        )
        .unwrap();
        ambiguous = ambiguous.rebuild(|facts| facts.compiler_support = support);
    }
    assert!(ambiguous
        .facts
        .compiler_support
        .artifacts()
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
    let projection = CompilerInputProjection::from_issued_entries(&[entries[0].clone()]).unwrap();
    let selected = projection
        .admit_interfaces_with_native_offers(
            ambiguous.artifact_view(),
            vec![entries[0].clone()],
            &[],
        )
        .expect("the inventory issuer selects the explicit original and canonical pair");
    let support = recover_fixture_compiler_support(
        projection,
        selected,
        ambiguous
            .compiler_source_support()
            .native_offers()
            .iter()
            .cloned(),
    )
    .unwrap();
    ambiguous = ambiguous.rebuild(|facts| facts.compiler_support = support);
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
fn activation_reuses_exact_empty_sparse_and_full_authored_offers() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let target = tidepool_repr::SessionModule::lib(tidepool_repr::Generation(1)).module_name();
    let product = crate::certified_products::fixture_native_declaration_product(
        crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
            "main",
            &target,
            vec![(7, vec![]), (11, vec![])],
            1,
            &BTreeMap::new(),
            b"activation interface".to_vec(),
            &BTreeMap::new(),
            &[],
        ),
        producer.sha256(),
        1,
    );
    let product = recovered_witness_fixtures(&[product]).remove(0).product;
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), product).unwrap());
    let inventory = ArtifactInventory::default();
    let full = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone()])
        .unwrap();
    let sparse = inventory
        .admit_recovery_selection(
            &inventory.empty_view(),
            vec![entry.clone()],
            &BTreeSet::from([crate::artifact_inventory::NativeGroupKey {
                artifact: entry.descriptor.id,
                original_ordinal: 7,
            }]),
        )
        .unwrap();
    let empty = IssuedNativeOffer::issue(entry.descriptor.id, &full, None).unwrap();
    let partial = IssuedNativeOffer::issue(
        entry.descriptor.id,
        &sparse,
        NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &sparse).unwrap(),
    )
    .unwrap();
    let complete = IssuedNativeOffer::issue(
        entry.descriptor.id,
        &full,
        NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &full).unwrap(),
    )
    .unwrap();
    assert!(!partial.is_full());
    assert!(complete.is_full());
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    for (offer, issuer) in [(empty, &full), (partial, &sparse), (complete, &full)] {
        let mut selected_graph = issuer.capture_graph_selection();
        let groups = selected_graph
            .roots
            .iter()
            .copied()
            .filter(|root| {
                matches!(
                    root,
                    crate::artifact_inventory::ArtifactBindingNode::Group(_)
                )
            })
            .collect::<Vec<_>>();
        selected_graph.roots.retain(|root| {
            !matches!(
                root,
                crate::artifact_inventory::ArtifactBindingNode::Group(_)
            )
        });
        selected_graph.custody_roots.extend(groups);
        let selected = issuer
            .select_issued(
                vec![entry.descriptor.id, projection.roles()[0].interface()],
                &crate::artifact_inventory::ExactArtifactSelection::from_recovered_graph_selection(
                    selected_graph,
                ),
            )
            .unwrap();
        assert!(selected.selected_native_groups().is_empty());
        let support =
            recover_fixture_compiler_support(projection.clone(), selected.clone(), [offer.clone()])
                .unwrap();
        let context = ExactDeclarationContext::from_authenticated_execution_support(
            producer.sha256(),
            support,
            vec![ExactLexicalNode {
                owner: entry.descriptor.owner.clone(),
                imports: vec![],
            }],
            entry.descriptor.owner.clone(),
            &[],
        )
        .unwrap();
        let before = context.artifact_view().capture_graph_selection();
        let inputs =
            crate::activation_preview::original_native_declaration_inputs(&context, PRODUCER)
                .unwrap()
                .unwrap();
        assert_eq!(inputs.native_offers, vec![offer.clone()]);
        assert_eq!(
            inputs.native_offers[0].snapshot_facts().identity().unwrap(),
            offer.snapshot_facts().identity().unwrap()
        );
        assert_eq!(
            inputs.artifacts.capture_graph_selection().roots,
            before.roots
        );
        assert_eq!(context.artifact_view().capture_graph_selection(), before);
        assert_eq!(
            context.compiler_source_support().native_offers(),
            &[offer.clone()]
        );
        let directory = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(Arc::new(context.clone()))
            .prepare_compilation_with_private_input(directory.path(), PRODUCER, None, Some(inputs))
            .unwrap();
        let prepared = request.compiler_inputs();
        let carried = prepared.native_availability(entry.descriptor.id).unwrap();
        assert_eq!(carried.offers(), &[offer]);
    }
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
                .artifacts()
                .inventory()
                .admit_shared_with_demand(
                    &context.facts.compiler_support.artifacts(),
                    entries.clone(),
                    crate::artifact_inventory::NativeArtifactDemand::ScopeInterfaces,
                )
                .expect("same-canonical body variants retain legal byte custody");
            let support = recover_fixture_compiler_support(
                context.compiler_input_projection().clone(),
                updated,
                context
                    .compiler_source_support()
                    .native_offers()
                    .iter()
                    .cloned(),
            )
            .unwrap();
            context = context.rebuild(|facts| facts.compiler_support = support);
        }
        context.compiler_metadata_snapshot().unwrap();
        let custody = context
            .facts
            .compiler_support
            .artifacts()
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
            let support = recover_fixture_compiler_support(
                updated,
                unissued.artifact_view().clone(),
                unissued
                    .compiler_source_support()
                    .native_offers()
                    .iter()
                    .cloned(),
            )
            .unwrap();
            unissued = unissued.rebuild(|facts| facts.compiler_support = support);
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

        let projection =
            CompilerInputProjection::from_issued_entries(&[entries[0].clone()]).unwrap();
        let selected = projection
            .admit_interfaces_with_native_offers(
                context.artifact_view(),
                vec![entries[0].clone()],
                &[],
            )
            .expect("the inventory issuer selects the explicit original and canonical pair");
        let support = recover_fixture_compiler_support(
            projection,
            selected,
            context
                .compiler_source_support()
                .native_offers()
                .iter()
                .cloned(),
        )
        .unwrap();
        context = context.rebuild(|facts| facts.compiler_support = support);
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
            .prepare_compilation_with_private_input(
                root.path(),
                PRODUCER,
                None,
                Some(private.clone()),
            )
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

        context = context
            .retain_issued_native_offers(private.native_offers().iter().cloned())
            .unwrap();
        // The inventory issuer explicitly selects complete original censuses;
        // the compiler issuer still names exactly one of those native bodies.
        {
            let updated = context
                .facts
                .compiler_support
                .artifacts()
                .inventory()
                .admit_shared(&context.facts.compiler_support.artifacts(), entries)
                .unwrap();
            let support = recover_fixture_compiler_support(
                context.compiler_input_projection().clone(),
                updated,
                context
                    .compiler_source_support()
                    .native_offers()
                    .iter()
                    .cloned(),
            )
            .unwrap();
            context = context.rebuild(|facts| facts.compiler_support = support);
        }
        context.compiler_metadata_snapshot().unwrap();
        let groups = context
            .facts
            .compiler_support
            .artifacts()
            .selected_native_groups();
        assert_eq!(groups.len(), products.len() * 2);
        let dependencies = context.facts.compiler_support.artifacts().dependencies();
        let without_offers = recover_fixture_compiler_support(
            context.compiler_input_projection().clone(),
            context.artifact_view().clone(),
            [],
        )
        .unwrap();
        let lost_offer = context
            .clone()
            .rebuild(|facts| facts.compiler_support = without_offers);
        assert!(
            ExactCompileContext::new(Arc::new(lost_offer))
                .prepare_compilation(&root.path().join("lost-offer"), PRODUCER)
                .is_err(),
            "selected execution and body custody cannot reconstruct a lost issued offer"
        );
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
                .artifacts()
                .selected_native_groups(),
            groups
        );
        assert_eq!(
            context.facts.compiler_support.artifacts().dependencies(),
            dependencies
        );
        let mut canonical_only = context.clone();
        {
            let updated = canonical_only
                .compiler_source_support()
                .for_source_owners(&BTreeSet::new())
                .unwrap();
            canonical_only = canonical_only.rebuild(|facts| facts.compiler_support = updated);
        }
        let exact = OriginalCompilerInputs::from_selected_authored_declarations(
            &canonical_only,
            producer,
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            exact
                .projection
                .roles()
                .iter()
                .filter_map(CompilerInputRole::original)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([current_native]),
            "one exact issued offer wins over unoffered equal-canonical custody"
        );
        let other = OriginalCompilerInputs::from_native_availability(
            &type_context(std::slice::from_ref(&products[1])),
            producer,
            std::slice::from_ref(&products[1]),
        )
        .unwrap();
        assert!(other.native_offers().iter().any(|offer| offer.is_full()));
        for reverse in [false, true] {
            let mut offers = other.native_offers().to_vec();
            if reverse {
                offers.reverse();
            }
            let mut competing = canonical_only.clone();
            competing.facts.compiler_support = competing
                .compiler_source_support()
                .retain_offers(offers)
                .unwrap();
            let error = OriginalCompilerInputs::from_selected_authored_declarations(
                &competing,
                producer,
                &[],
            )
            .unwrap_err();
            assert!(
                matches!(error, CompileError::ArtifactInventory(error)
                if matches!(error.failure, crate::artifact_inventory::ArtifactInventoryFailure::CompilerOriginalOfferConflict { .. })),
                "competing eligible issued products refuse in either offer order"
            );
        }
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
            .artifacts()
            .inventory()
            .admit_shared_with_demand(
                &context.facts.compiler_support.artifacts(),
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
                .artifacts()
                .selected_native_groups(),
            groups
        );
        assert_eq!(
            context.facts.compiler_support.artifacts().dependencies(),
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
    let support = native_context
        .compiler_source_support()
        .for_source_owners(&BTreeSet::new())
        .unwrap();
    native_context = native_context.rebuild(|facts| facts.compiler_support = support);
    let producer = CanonicalProducerIdentity::from_producer_bytes(producer_bytes);
    let authored_root = native_context.authored_native_root(generation).unwrap();
    let selected_before = native_context
        .facts
        .compiler_support
        .artifacts()
        .selected_native_groups();
    let assert_exact_offer = |context: &ExactDeclarationContext, name: &str, expected: bool| {
        let before = context
            .facts
            .compiler_support
            .artifacts()
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
                .artifacts()
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
    let support = captured
        .compiler_source_support()
        .selected_interfaces()
        .unwrap()
        .support_for_owners(
            captured
                .compiler_input_projection()
                .snapshot_roles()
                .keys()
                .cloned()
                .collect(),
        )
        .unwrap()
        .retain_custody(captured.artifact_view())
        .unwrap()
        .retain_offers(
            captured
                .compiler_source_support()
                .native_offers()
                .iter()
                .cloned(),
        )
        .unwrap();
    captured = captured.rebuild(|facts| facts.compiler_support = support);
    assert!(captured
        .facts
        .compiler_support
        .artifacts()
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
        .artifacts()
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
            .artifacts()
            .inventory()
            .admit_recovery_selection(
                &partial
                    .facts
                    .compiler_support
                    .artifacts()
                    .inventory()
                    .empty_view(),
                partial.facts.compiler_support.artifacts().entries(),
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
            let native_roles =
                CompilerInputProjection::from_issued_entries(&view.entries()).unwrap();
            let support = fixture_compiler_support(native_roles, view)
                .for_source_owners(&BTreeSet::new())
                .unwrap();
            partial = partial.rebuild(|facts| facts.compiler_support = support);
        }
        assert_eq!(
            partial
                .facts
                .compiler_support
                .artifacts()
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
                .artifacts()
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
                    .merge_fixture_selections(&sources[1])
                    .unwrap()
                    .merge_fixture_selections(&ambient)
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
                    .merge_fixture_selections(&sources[index])
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
            .merge_fixture_compiler_inputs(&CompilerInputProjection::default(), view, &interfaces)
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
    joined.artifacts = private[0]
        .artifacts
        .retain_custody(&joined.artifacts)
        .unwrap();
    assert_offer_retention_matches_ordered_fold(
        base_context.artifact_view(),
        base_context.compiler_input_projection(),
        &joined.native_offers,
    );
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
        let admitted = inputs.artifacts.inventory().admit_bound_target(
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
    let demanded = inputs.artifacts.inventory().admit_bound_target(
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
    let support = private[0]
        .artifacts
        .inventory()
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
            .merge_fixture_selections(&selected)
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
        let issued = recover_fixture_compiler_support(
            CompilerInputProjection::from_issued_entries(&[entry.clone(), unrelated_entry.clone()])
                .unwrap(),
            full.clone(),
            [],
        )
        .unwrap();
        let interfaces = issued
            .selected_interfaces()
            .unwrap()
            .support_for_owners(vec![
                entry.descriptor.owner.clone(),
                unrelated_entry.descriptor.owner.clone(),
            ])
            .unwrap()
            .retain_custody(&custody)
            .unwrap();
        let context = Arc::new(ExactDeclarationContext::from_facts(
            DeclarationContextFacts {
                producer: producer.sha256(),
                compiler_support: interfaces,
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
            let mut fixed = inputs
                .artifacts
                .inventory()
                .bind_segment(&inputs.artifacts, vec![], &selection)
                .unwrap();
            let imports = [source_import(&native, demanded)];
            let admitted = inputs.artifacts.inventory().admit_bound_target(
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
        let mut fixed = inputs
            .artifacts
            .inventory()
            .bind_segment(&inputs.artifacts, vec![], &selection)
            .unwrap();
        let imports = [source_import(&unrelated, 7)];
        let denied = inputs.artifacts.inventory().admit_bound_target(
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
fn matching_native_offer_role_preserves_issuance_without_rewalking_proof() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};

    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let native = product("MatchingOfferRole", 91);
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), native).unwrap());
    let inventory = ArtifactInventory::default();
    let artifacts = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone()])
        .unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let proof = NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &artifacts)
        .unwrap()
        .unwrap();
    let full = IssuedNativeOffer::issue(entry.descriptor.id, &artifacts, Some(proof)).unwrap();
    assert!(full.is_full());
    let partial = full
        .within_projection(&projection, &inventory.empty_view())
        .unwrap()
        .unwrap();
    assert!(!partial.is_full());
    let empty = IssuedNativeOffer::issue(entry.descriptor.id, &artifacts, None).unwrap();

    for offer in [&full, &partial, &empty] {
        let expected = offer.snapshot_facts();
        for _ in 0..8 {
            let before = inventory.metrics();
            let retained = offer
                .within_projection(&projection, &artifacts)
                .unwrap()
                .unwrap();
            let after = inventory.metrics();
            assert_eq!(
                after.graph_visits, before.graph_visits,
                "an exact matching role propagates its opaque issuance without proving it again"
            );
            assert_eq!(after.entry_handle_copies, before.entry_handle_copies);
            assert_eq!(retained.snapshot_facts(), expected);
        }
    }
    assert!(
        full.within_projection(&CompilerInputProjection::default(), &artifacts)
            .unwrap()
            .is_none(),
        "unrelated projection still excludes the offer"
    );

    let changed = product("MatchingOfferRole", 92);
    let changed = Arc::new(ArtifactEntry::original(producer.sha256(), changed).unwrap());
    let competing = CompilerInputProjection::from_issued_entries(&[changed]).unwrap();
    let error = full.within_projection(&competing, &artifacts).unwrap_err();
    let diagnostic = crate::failclass::classify_compile(&error);
    assert_eq!(
        diagnostic.class,
        crate::failclass::FailureClass::VersionSkew
    );
    assert_eq!(diagnostic.phase, crate::failclass::Phase::Compile);
    let CompileError::ArtifactInventory(error) = error else {
        panic!("the native inventory must refuse the competing role: {error:?}");
    };
    let offered = full.snapshot_facts();
    let selected = &competing.snapshot_roles()[&offered.owner];
    assert_eq!(
        &error.failure,
        &crate::artifact_inventory::ArtifactInventoryFailure::NativeOfferSelectionConflict {
            owner: offered.owner,
            offered_original: offered.original,
            offered_interface: offered.interface,
            selected_original: selected.original().unwrap(),
            selected_interface: selected.interface(),
        }
    );
    assert_eq!(
        diagnostic.cause,
        Some(crate::failclass::CompileFailureCause::ArtifactInventory {
            failure: error.failure,
        })
    );
}

#[test]
fn recovered_native_offer_preserves_full_partial_and_empty_issuance_modes() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
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
    let canonical_support = recover_fixture_compiler_support(
        projection.interface_only(),
        artifacts.clone(),
        [empty.clone()],
    )
    .unwrap()
    .source_support(&BTreeSet::from([entry.descriptor.owner.clone()]))
    .unwrap();
    assert!(canonical_support
        .projection()
        .roles()
        .iter()
        .all(|role| role.original().is_none()));
    assert_eq!(canonical_support.native_offers().len(), 1);
    let empty_restored = canonical_support.native_offers()[0]
        .snapshot_facts()
        .restore(&canonical_support.artifacts())
        .unwrap();
    assert_eq!(empty_restored.snapshot_facts(), empty.snapshot_facts());
    let support = recover_fixture_compiler_support(projection.clone(), artifacts.clone(), [])
        .unwrap()
        .select_support_roots(vec![canonical])
        .unwrap()
        .finalize_namespace()
        .unwrap();
    assert_eq!(support.projection(), &projection);
    let artifacts = support.artifacts().clone();
    assert!(artifacts.selected_native_groups().is_empty());
    assert!(proof.qualify_availability(&artifacts).unwrap().is_some());
    let root = tempfile::tempdir().unwrap();

    for (index, offer) in [full, partial, empty].into_iter().enumerate() {
        let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: recover_fixture_compiler_support(
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
    use crate::artifact_inventory::{IssuedNativeOffer, NativeArtifactDemand};
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
    assert_eq!(
        left_offer.custody(),
        right_offer.custody(),
        "payload/group view equality cannot identify issuance"
    );
    assert_ne!(
        left_offer, right_offer,
        "scoped Empty facts survive live offer deduplication"
    );
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
        .merge_fixture_selections(&right)
        .unwrap()
        .retain_original_custody(&right)
        .unwrap();
    let restored_left = left_offer.snapshot_facts().restore(&artifacts).unwrap();
    assert_eq!(restored_left.snapshot_facts(), left_offer.snapshot_facts());
    assert!(restored_left.proof().is_none());
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()])
        .unwrap()
        .interface_only();
    let support = recover_fixture_compiler_support(
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

#[test]
fn public_bound_recovery_preserves_selected_full_offer_after_execution_withdrawal() {
    use crate::artifact_inventory::IssuedNativeOffer;
    use crate::exact_context::{hydrate_context, Materializer};
    use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};

    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let entry =
        Arc::new(ArtifactEntry::original(producer.sha256(), product("BoundOffer", 91)).unwrap());
    let inventory = ArtifactInventory::default();
    let artifacts = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone()])
        .unwrap();
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let initial = ExactDeclarationContext::from_facts(DeclarationContextFacts {
        producer: producer.sha256(),
        compiler_support: crate::declaration_context::issue_fixture_compiler_support(
            projection.clone(),
            artifacts,
        )
        .unwrap(),
        lexical: Default::default(),
        template_imports: None,
        original_instance_environment: OriginalInstanceEnvironment::Unknown,
    });
    let acquire = |context: &ExactDeclarationContext, path: &Path| {
        std::fs::create_dir_all(path).unwrap();
        let (products, interfaces) = context
            .materialize_recovery_products_and_interfaces(path)
            .unwrap();
        let graph = context.snapshot().graph_selection().unwrap();
        RecoveredArtifactInventory::capture_bound(
            path,
            &products,
            &interfaces,
            &[],
            &[],
            &context.artifact_view().descriptors(),
            &graph.bindings,
            &context.artifact_view().binding_dependencies(),
        )
        .unwrap()
    };
    let root = tempfile::tempdir().unwrap();
    let captured = acquire(&initial, &root.path().join("initial"));
    assert!(
        captured
            .context_all_groups(&[entry.descriptor.id], Vec::new())
            .is_err(),
        "omitting the required canonical artifact cannot restore the recorded closure"
    );
    let issued = captured
        .context_all_groups(&initial.artifact_view().artifact_ids(), Vec::new())
        .unwrap();
    assert!(issued.facts.compiler_support.native_offers()[0].is_full());
    assert_eq!(issued.artifact_view().selected_native_groups().len(), 2);
    let projection = issued.facts.compiler_support.projection().clone();
    let canonical = projection
        .snapshot_roles()
        .get(&entry.descriptor.owner)
        .unwrap()
        .interface();
    let support = issued
        .compiler_source_support()
        .select_support_roots(vec![canonical])
        .unwrap()
        .finalize_namespace()
        .unwrap();
    assert_eq!(support.projection(), &projection);
    let artifacts = support.artifacts().clone();
    assert!(artifacts.selected_native_groups().is_empty());
    let full = issued.facts.compiler_support.native_offers()[0].clone();
    let empty = IssuedNativeOffer::issue(entry.descriptor.id, &artifacts, None).unwrap();
    for (index, offer) in [full, empty].into_iter().enumerate() {
        let support = recover_fixture_compiler_support(
            projection.clone(),
            artifacts.clone(),
            [offer.clone()],
        )
        .unwrap()
        .source_support(&BTreeSet::from([entry.descriptor.owner.clone()]))
        .unwrap();
        assert_eq!(support.native_offers()[0].is_full(), index == 0);
        let context = ExactDeclarationContext::from_facts(DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: support,
            lexical: Default::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        });
        assert!(context.artifact_view().selected_native_groups().is_empty());
        let store = MemoryBlockstore::new();
        let mut materializer = Materializer::new(&store);
        let receipt = materializer
            .materialize_context(None, &context.snapshot())
            .unwrap();
        for (cid, bytes) in materializer.finish().unwrap().into_blocks() {
            store.put_keyed(&cid, &bytes).unwrap();
        }
        let receipt = hydrate_context(&store, receipt.root()).unwrap();
        let recovered = acquire(&context, &root.path().join(format!("recovered-{index}")));
        let mut restored = recovered.context_with_snapshot(receipt.snapshot()).unwrap();
        restored.bind_snapshot_baseline(&receipt).unwrap();
        assert_eq!(
            restored.facts.compiler_support.native_offers()[0].snapshot_facts(),
            offer.snapshot_facts()
        );
        assert!(restored.artifact_view().selected_native_groups().is_empty());
        assert!(restored.lexical_graph().is_empty());
        let request = ExactCompileContext::new(Arc::new(restored))
            .prepare_compilation(&root.path().join(format!("request-{index}")), PRODUCER)
            .unwrap();
        assert_eq!(
            wire_mode(&request, "BoundOffer"),
            if index == 0 {
                full_mode()
            } else {
                issued_mode(&[])
            }
        );
        let availability =
            &request.compiler_inputs().native_transport.availability[&entry.descriptor.id];
        for ordinal in [7, 9001] {
            assert_eq!(availability.permits(ordinal), index == 0);
        }
        assert!(request
            .context()
            .artifact_view()
            .selected_native_groups()
            .is_empty());
    }
}

#[test]
fn historical_canonical_offer_histories_preserve_selected_request_and_recovery() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    use crate::exact_context::{hydrate_context, Materializer};
    use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let root = tempfile::tempdir().unwrap();
    let mut histories = 0;
    for changed_canonical in [false, true] {
        for changed_body in [false, true] {
            for mode in 0..3 {
                for reverse in [false, true] {
                    for duplicate in [false, true] {
                        let old = product("HistoricalCanonical", 91);
                        let new = fixture_finalized_product(
                            original_groups_fixture_with_interface(
                                "HistoricalCanonical",
                                vec![(7, vec![]), (9001, vec![])],
                                if changed_body { 92 } else { 91 },
                                &BTreeMap::new(),
                                if changed_canonical {
                                    b"independent current canonical interface".to_vec()
                                } else {
                                    b"HistoricalCanonical".to_vec()
                                },
                            ),
                            producer.sha256(),
                        );
                        let new = recovered_witness_fixtures(&[new]).remove(0).product;
                        let entries = [old, new]
                            .into_iter()
                            .map(|product| {
                                Arc::new(
                                    ArtifactEntry::original(producer.sha256(), product).unwrap(),
                                )
                            })
                            .collect::<Vec<_>>();
                        let sources = entries
                            .iter()
                            .map(|entry| {
                                let inventory = ArtifactInventory::default();
                                inventory
                                    .admit_shared(&inventory.empty_view(), vec![entry.clone()])
                                    .unwrap()
                            })
                            .collect::<Vec<_>>();
                        let old_projection =
                            CompilerInputProjection::from_issued_entries(&[entries[0].clone()])
                                .unwrap();
                        let proof = NativeOriginalRequirement::from_selected_view(
                            entries[0].descriptor.id,
                            &sources[0],
                        )
                        .unwrap()
                        .unwrap();
                        let full = IssuedNativeOffer::issue(
                            entries[0].descriptor.id,
                            &sources[0],
                            Some(proof),
                        )
                        .unwrap();
                        let partial = full
                            .within_projection(
                                &old_projection,
                                &sources[0].inventory().empty_view(),
                            )
                            .unwrap()
                            .unwrap();
                        let offer = match mode {
                            0 => IssuedNativeOffer::issue(
                                entries[0].descriptor.id,
                                &sources[0],
                                None,
                            )
                            .unwrap(),
                            1 => partial,
                            _ => full,
                        };
                        let issued = recover_fixture_compiler_support(
                            CompilerInputProjection::from_issued_entries(&[entries[1].clone()])
                                .unwrap(),
                            sources[1].clone(),
                            [],
                        )
                        .unwrap();
                        let selected_support = issued
                            .selected_interfaces()
                            .unwrap()
                            .support_for_owners(vec![entries[1].descriptor.owner.clone()])
                            .unwrap();
                        let selected = selected_support.artifacts().clone();
                        let projection = selected_support.projection().clone();
                        if changed_canonical {
                            let incompatible_projection = old_projection.interface_only();
                            assert!(
                                matches!(selected.merge_fixture_compiler_inputs(
                                &projection, &sources[0], &incompatible_projection,
                            ), Err(CompileError::ArtifactInventory(error))
                                if matches!(error.failure, crate::artifact_inventory::ArtifactInventoryFailure::OwnerConflict { .. })),
                                "two explicitly selected canonical interfaces must refuse"
                            );
                        }
                        let mut offers = vec![offer.clone()];
                        if duplicate {
                            offers.push(offer.clone());
                        }
                        // A covered prefix followed by another authenticated scope.
                        let auxiliary =
                            IssuedNativeOffer::issue(entries[1].descriptor.id, &sources[1], None)
                                .unwrap();
                        if reverse {
                            offers.insert(0, auxiliary);
                        } else {
                            offers.push(auxiliary);
                        }
                        let support = recover_fixture_compiler_support(
                            projection.clone(),
                            selected.clone(),
                            offers,
                        )
                        .unwrap();
                        assert_eq!(
                            support.artifacts().capture_graph_selection().namespace,
                            selected.capture_graph_selection().namespace
                        );
                        assert_eq!(
                            support.artifacts().capture_graph_selection().roots,
                            selected.capture_graph_selection().roots
                        );
                        assert!(support.artifacts().selected_native_groups().is_empty());
                        let graph = support.artifacts().capture_graph_selection();
                        assert!(offer
                            .custody()
                            .capture_graph_selection()
                            .bindings
                            .iter()
                            .all(|node| graph.bindings.contains(node)));
                        assert!(offer
                            .custody()
                            .binding_dependencies()
                            .iter()
                            .all(|edge| support.artifacts().binding_dependencies().contains(edge)));
                        let (joined, joined_projection) = selected
                            .merge_fixture_compiler_inputs(
                                &projection,
                                &support.artifacts(),
                                &projection,
                            )
                            .unwrap();
                        assert_eq!(joined_projection, projection);
                        assert_eq!(
                            joined.capture_graph_selection().namespace,
                            selected.capture_graph_selection().namespace
                        );
                        assert_eq!(
                            joined.capture_graph_selection().roots,
                            selected.capture_graph_selection().roots
                        );
                        assert!(joined.selected_native_groups().is_empty());
                        let expected = support.native_offer_observations().unwrap();
                        let context =
                            ExactDeclarationContext::from_facts(DeclarationContextFacts {
                                producer: producer.sha256(),
                                compiler_support: support,
                                lexical: Default::default(),
                                template_imports: None,
                                original_instance_environment: OriginalInstanceEnvironment::Unknown,
                            });
                        if changed_canonical {
                            let support = recover_fixture_compiler_support(
                                CompilerInputProjection::from_issued_entries(&[entries[1].clone()])
                                    .unwrap(),
                                sources[1].clone(),
                                context
                                    .compiler_source_support()
                                    .native_offers()
                                    .iter()
                                    .cloned(),
                            )
                            .unwrap();
                            let incompatible = context
                                .clone()
                                .rebuild(|facts| facts.compiler_support = support);
                            assert!(ExactCompileContext::new(Arc::new(incompatible)).prepare_compilation(
                                &root.path().join(format!("incompatible-{histories}")), PRODUCER,
                            ).is_err(), "historical issuance cannot bypass current native role compatibility");
                        }
                        let store = MemoryBlockstore::new();
                        let mut materializer = Materializer::new(&store);
                        let receipt = materializer
                            .materialize_context(None, &context.snapshot())
                            .unwrap();
                        for (cid, bytes) in materializer.finish().unwrap().into_blocks() {
                            store.put_keyed(&cid, &bytes).unwrap();
                        }
                        let receipt = hydrate_context(&store, receipt.root()).unwrap();
                        let path = root.path().join(format!("recovery-{histories}"));
                        std::fs::create_dir_all(&path).unwrap();
                        let (products, interfaces) = context
                            .materialize_recovery_products_and_interfaces(&path)
                            .unwrap();
                        let graph = context.snapshot().graph_selection().unwrap();
                        let recovered = RecoveredArtifactInventory::capture_bound(
                            &path,
                            &products,
                            &interfaces,
                            &[],
                            &[],
                            &context.artifact_view().descriptors(),
                            &graph.bindings,
                            &context.artifact_view().binding_dependencies(),
                        )
                        .unwrap();
                        let restored = recovered.context_with_snapshot(receipt.snapshot()).unwrap();
                        assert_eq!(
                            restored
                                .facts
                                .compiler_support
                                .native_offer_observations()
                                .unwrap(),
                            expected
                        );
                        assert_eq!(
                            restored.artifact_view().capture_graph_selection(),
                            context.artifact_view().capture_graph_selection()
                        );
                        assert!(restored.lexical_graph().is_empty());
                        // Publish a fresh authored cohort after durable recovery, while
                        // retaining the historical offer without selecting its namespace.
                        let ArtifactPayload::Original(current) = &entries[1].payload else {
                            unreachable!()
                        };
                        let fresh = product("FreshPublishedConsumer", 94);
                        let compiled = certified_product_artifact_view(
                            producer.sha256(),
                            &[current.clone(), fresh],
                            &[],
                            None,
                        )
                        .unwrap();
                        let native_entries = compiled
                            .entries()
                            .into_iter()
                            .filter(|entry| matches!(&entry.payload, ArtifactPayload::Original(_)))
                            .collect::<Vec<_>>();
                        let fresh_projection =
                            CompilerInputProjection::from_issued_entries(&native_entries).unwrap();
                        let fresh_selection = crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                            &fresh_projection, &compiled.metadata_snapshot(),
                            &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
                        ).unwrap();
                        let originals = fresh_selection
                            .selected_original_closure(&compiled)
                            .unwrap();
                        let source_support = fresh_projection
                            .source_support(
                                &compiled,
                                &native_entries
                                    .iter()
                                    .map(|entry| entry.descriptor.owner.clone())
                                    .collect(),
                            )
                            .unwrap();
                        let publication_context = restored
                            .clone()
                            .extend_issued_compiler_support(&source_support)
                            .unwrap();
                        let published =
                            certified_artifact_view(&originals, &publication_context).unwrap();
                        assert_eq!(published.native_offer_observations().unwrap(), expected);
                        assert!(originals
                            .native_closure()
                            .capture_graph_selection()
                            .roots
                            .iter()
                            .filter(|root| matches!(
                                root,
                                crate::artifact_inventory::ArtifactBindingNode::Group(_)
                            ))
                            .all(|root| published
                                .artifacts()
                                .capture_graph_selection()
                                .roots
                                .contains(root)));
                        assert!(originals
                            .native_closure()
                            .binding_dependencies()
                            .iter()
                            .all(|edge| published
                                .artifacts()
                                .binding_dependencies()
                                .contains(edge)));
                        assert_eq!(
                            published.projection(),
                            publication_context.compiler_input_projection()
                        );
                        let request = ExactCompileContext::new(Arc::new(restored))
                            .prepare_compilation(
                                &root.path().join(format!("request-{histories}")),
                                PRODUCER,
                            )
                            .unwrap();
                        assert!(request
                            .compiler_inputs()
                            .artifacts
                            .selected_native_groups()
                            .is_empty());
                        assert_eq!(request.compiler_inputs().projection, projection);
                        assert!(request
                            .compiler_inputs()
                            .native_transport
                            .availability
                            .is_empty());
                        histories += 1;
                    }
                }
            }
        }
    }
    assert_eq!(histories, 48);
}

#[test]
fn namespace_empty_full_offer_restores_its_own_exact_qualification() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let entry = Arc::new(
        ArtifactEntry::original(producer.sha256(), product("EmptyNamespaceFull", 91)).unwrap(),
    );
    let inventory = ArtifactInventory::default();
    let source = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone()])
        .unwrap();
    let proof = NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &source)
        .unwrap()
        .unwrap();
    assert!(proof.selection.graph_selection().namespace.is_empty());
    let full = IssuedNativeOffer::issue(entry.descriptor.id, &source, Some(proof)).unwrap();
    assert!(full.is_full());
    let facts = full.snapshot_facts();
    let support = recover_fixture_compiler_support(
        CompilerInputProjection::default(),
        inventory.empty_view(),
        [full],
    )
    .unwrap();
    assert!(support
        .artifacts()
        .capture_graph_selection()
        .namespace
        .is_empty());
    let restored = facts.restore(&support.artifacts()).unwrap();
    assert!(restored.is_full());
    assert_eq!(restored.snapshot_facts(), facts);
    assert!(
        facts.restore(&inventory.empty_view()).is_err(),
        "missing recorded carrier/canonical custody must refuse"
    );
    let mut wrong = facts;
    wrong.interface = crate::artifact_inventory::ArtifactId([0; 32]);
    assert!(
        wrong.restore(&support.artifacts()).is_err(),
        "a different recorded canonical identity must refuse"
    );
}

#[test]
fn private_input_consumers_preserve_role_issuer_over_historical_canonical_custody() {
    use crate::artifact_inventory::IssuedNativeOffer;
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let old = product("PrivateCustody", 91);
    let changed = fixture_finalized_product(
        original_groups_fixture_with_interface(
            "PrivateCustody",
            vec![(7, vec![]), (9001, vec![])],
            92,
            &BTreeMap::new(),
            b"current private interface".to_vec(),
        ),
        producer.sha256(),
    );
    let changed = recovered_witness_fixtures(&[changed]).remove(0).product;
    let inventory = ArtifactInventory::default();
    let old_entry = Arc::new(ArtifactEntry::original(producer.sha256(), old).unwrap());
    let old_source = inventory
        .admit_shared(&inventory.empty_view(), vec![old_entry.clone()])
        .unwrap();
    let offer = IssuedNativeOffer::issue(old_entry.descriptor.id, &old_source, None).unwrap();
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), changed).unwrap());
    let current_source = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone()])
        .unwrap();
    let issued = recover_fixture_compiler_support(
        CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap(),
        current_source,
        [],
    )
    .unwrap();
    let current_support = issued
        .selected_interfaces()
        .unwrap()
        .support_for_owners(vec![entry.descriptor.owner.clone()])
        .unwrap();
    let current = current_support.artifacts().clone();
    let projection = current_support.projection().clone();
    let context = |offers: Vec<IssuedNativeOffer>| {
        Arc::new(ExactDeclarationContext::from_facts(
            DeclarationContextFacts {
                producer: producer.sha256(),
                compiler_support: recover_fixture_compiler_support(
                    projection.clone(),
                    current.clone(),
                    offers,
                )
                .unwrap(),
                lexical: Default::default(),
                template_imports: None,
                original_instance_environment: OriginalInstanceEnvironment::Unknown,
            },
        ))
    };
    let other_inventory = ArtifactInventory::default();
    let other_source = other_inventory
        .admit_shared(
            &other_inventory.empty_view(),
            vec![
                entry.clone(),
                Arc::new(
                    ArtifactEntry::original(producer.sha256(), product("PrivateScopeSibling", 93))
                        .unwrap(),
                ),
            ],
        )
        .unwrap();
    let other_issued =
        recover_fixture_compiler_support(projection.clone(), other_source, []).unwrap();
    let other_support = other_issued
        .selected_interfaces()
        .unwrap()
        .support_for_owners(vec![entry.descriptor.owner.clone()])
        .unwrap();
    let other_current = other_support.artifacts().clone();
    assert_ne!(
        current.capture_graph_selection().namespace,
        other_current.capture_graph_selection().namespace
    );
    let baseline = Arc::new(ExactDeclarationContext::from_facts(
        DeclarationContextFacts {
            producer: producer.sha256(),
            compiler_support: recover_fixture_compiler_support(
                projection.clone(),
                other_current,
                [],
            )
            .unwrap(),
            lexical: Default::default(),
            template_imports: None,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
        },
    ));

    let root = tempfile::tempdir().unwrap();
    let issue = |context, name: &str| {
        ExactCompileContext::new(context)
            .prepare_compilation(&root.path().join(name), PRODUCER)
            .unwrap()
    };
    let request = issue(context(vec![offer]), "custodied");
    let inputs = request.compiler_inputs();
    let validation = PackageInterfaceValidation::default();
    let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        inputs,
        &inputs.metadata,
        &validation.inventory,
    )
    .unwrap();
    let private = OriginalCompilerInputs::from_selection(&selection, &inputs.artifacts).unwrap();
    assert!(private
        .artifacts
        .artifact_ids()
        .contains(&old_entry.descriptor.id));
    let clean = issue(baseline.clone(), "clean");
    let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
        clean.compiler_inputs(),
        &clean.compiler_inputs().metadata,
        &validation.inventory,
    )
    .unwrap();
    let clean =
        OriginalCompilerInputs::from_selection(&selection, &clean.compiler_inputs().artifacts)
            .unwrap();
    let merged = clean.merge(&private);
    let protected =
        ProtectedScaffoldRequirements::default().compiler_inputs(&baseline, Some(&private));
    assert!(
        merged.is_ok() && protected.is_ok(),
        "private merge error={:?}, protected error={:?}",
        merged.as_ref().err(),
        protected.as_ref().err()
    );
    let merged = merged.unwrap();

    assert_eq!(
        merged.artifacts.capture_graph_selection().namespace,
        clean.artifacts.capture_graph_selection().namespace
    );
    assert!(merged.artifacts.selected_native_groups().is_empty());
    let request = protected.unwrap();
    assert_eq!(
        request.artifacts.capture_graph_selection().namespace,
        baseline.artifact_view().capture_graph_selection().namespace
    );
    assert!(request.artifacts.selected_native_groups().is_empty());
}

#[test]
fn retained_template_capture_uses_selected_canonical_with_historical_custody() {
    use crate::artifact_inventory::IssuedNativeOffer;
    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER).sha256();
    let owner = ExactModuleIdentity {
        unit: "main".into(),
        module: "TemplateOwner".into(),
    };
    for changed_canonical in [false, true] {
        let entries = [
            b"old canonical".to_vec(),
            if changed_canonical {
                b"current canonical".to_vec()
            } else {
                b"old canonical".to_vec()
            },
        ]
        .into_iter()
        .enumerate()
        .map(|(index, bytes)| {
            let finalized = fixture_finalized_product(
                original_groups_fixture_with_interface(
                    &owner.module,
                    vec![(7, vec![])],
                    index as u8 + 1,
                    &BTreeMap::new(),
                    bytes,
                ),
                producer,
            );
            let product = recovered_witness_fixtures(&[finalized]).remove(0).product;
            Arc::new(ArtifactEntry::original(producer, product).unwrap())
        })
        .collect::<Vec<_>>();
        let views = entries
            .iter()
            .map(|entry| {
                let inventory = ArtifactInventory::default();
                inventory
                    .admit_shared(&inventory.empty_view(), vec![entry.clone()])
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let owner = entries[0].descriptor.owner.clone();
        let mut contexts = Vec::new();
        for selected in 0..2 {
            let projection =
                CompilerInputProjection::from_issued_entries(&[entries[selected].clone()]).unwrap();
            let expected = projection.snapshot_roles().get(&owner).unwrap().interface();
            let historical = IssuedNativeOffer::issue(
                entries[1 - selected].descriptor.id,
                &views[1 - selected],
                None,
            )
            .unwrap();
            let support = recover_fixture_compiler_support(
                projection,
                views[selected].clone(),
                [historical.clone(), historical],
            )
            .unwrap();
            let expected_view = views[selected].select_roots(vec![expected]).unwrap();
            let issued_graph = views[selected].capture_graph_selection();
            let canonical_binding = *issued_graph
                .namespace
                .iter()
                .find(|binding| binding.artifact == expected)
                .expect("the selected role has its issuer's exact canonical binding");
            let issued_edges = views[selected].binding_dependencies();
            let mut expected_namespace = BTreeSet::from([canonical_binding]);
            let mut pending = vec![canonical_binding];
            while let Some(binding) = pending.pop() {
                for (from, to, dependency) in &issued_edges {
                    if *from != ArtifactBindingNode::Artifact(binding)
                        || *dependency != ArtifactDependency::Interface
                    {
                        continue;
                    }
                    if let ArtifactBindingNode::Artifact(target) = to {
                        if expected_namespace.insert(*target) {
                            pending.push(*target);
                        }
                    }
                }
            }
            // Type selection narrows only the namespace. The raw selection
            // independently checks every retained proof, binding and custody seed.
            let mut expected_graph = expected_view.capture_graph_selection();
            expected_graph.namespace = expected_namespace.into_iter().collect();
            if !changed_canonical {
                let other_graph = views[1 - selected].capture_graph_selection();
                let other_canonical = other_graph
                    .namespace
                    .iter()
                    .find(|binding| binding.artifact == expected)
                    .expect("equal canonical bytes retain their other issuer's binding");
                assert_ne!(
                    &canonical_binding, other_canonical,
                    "equal canonical payloads must have independently issued bindings"
                );
            }
            let original = ExactDeclarationContext::from_authenticated_execution_support(
                producer,
                support,
                vec![ExactLexicalNode {
                    owner: owner.clone(),
                    imports: vec![],
                }],
                owner.clone(),
                std::slice::from_ref(&owner),
            )
            .unwrap();
            let before = original.artifact_view().capture_graph_selection();
            let before_lexical = original.lexical_graph().to_vec();
            let retained =
                RetainedTemplateImports::capture(&original, &["import TemplateOwner\n".into()])
                    .unwrap()
                    .unwrap();
            assert_eq!(retained.graph[&owner].canonical, expected);
            assert_eq!(
                retained
                    .compiler_support
                    .artifacts()
                    .capture_graph_selection(),
                expected_graph
            );
            assert_eq!(
                retained.compiler_support.projection().roles(),
                vec![CompilerInputRole::InterfaceOnly {
                    interface: expected
                }]
            );
            assert_eq!(original.lexical_graph(), before_lexical);
            assert!(retained
                .compiler_support
                .artifacts()
                .selected_native_groups()
                .is_empty());
            assert_eq!(original.artifact_view().capture_graph_selection(), before);
            let retained_again = retained.retain_in(original.artifact_view()).unwrap();
            assert_eq!(
                retained_again.capture_graph_selection().namespace,
                before.namespace
            );
            assert_eq!(retained_again.capture_graph_selection().roots, before.roots);
            assert!(!original.artifact_view().selected_native_groups().is_empty());
            assert!(before.bindings.iter().all(|binding| retained_again
                .capture_graph_selection()
                .bindings
                .contains(binding)));
            assert!(original
                .artifact_view()
                .binding_dependencies()
                .iter()
                .all(|edge| retained_again.binding_dependencies().contains(edge)));
            let empty = original.artifact_view().inventory().empty_view();
            let custody_only = retained.retain_in(&empty).unwrap();
            let custody_graph = custody_only.capture_graph_selection();
            assert!(custody_graph.namespace.is_empty());
            assert!(custody_graph.roots.is_empty());
            let incoming = retained
                .compiler_support
                .artifacts()
                .capture_graph_selection();
            assert!(incoming
                .roots
                .iter()
                .chain(&incoming.custody_roots)
                .all(|seed| custody_graph.custody_roots.contains(seed)));
            assert_eq!(
                retained
                    .retain_in(&custody_only)
                    .unwrap()
                    .capture_graph_selection(),
                custody_graph
            );
            let extended = original
                .clone()
                .extend_interface_context(&original)
                .unwrap();
            contexts.push(original.clone());
            assert_eq!(extended.lexical_graph(), original.lexical_graph());
            assert_eq!(
                extended
                    .compiler_source_support()
                    .native_offer_observations()
                    .unwrap(),
                original
                    .compiler_source_support()
                    .native_offer_observations()
                    .unwrap()
            );
            assert_eq!(
                extended.artifact_view().capture_graph_selection().namespace,
                before.namespace
            );
            assert_eq!(
                extended.compiler_input_projection(),
                original.compiler_input_projection()
            );
            assert_eq!(
                extended.artifact_view().capture_graph_selection().roots,
                before.roots
            );
            let mut roles = CompilerInputProjection::default();
            roles
                .admit_role(
                    ExactModuleIdentity {
                        unit: owner.unit.clone(),
                        module: "WrongOwner".into(),
                    },
                    CompilerInputRole::InterfaceOnly {
                        interface: expected,
                    },
                )
                .unwrap();
            assert!(
                roles
                    .validate_associations(original.artifact_view())
                    .is_err(),
                "the owning association validator refuses a mismatched raw role key"
            );
            retained
                .validate(producer, &retained.compiler_support.artifacts())
                .unwrap();
            let mut mismatched = retained.as_ref().clone();
            mismatched
                .graph
                .get_mut(&owner)
                .unwrap()
                .interface
                .interface_sha256[0] ^= 1;
            assert!(mismatched
                .validate(producer, original.artifact_view())
                .is_err());
        }
        if changed_canonical {
            assert!(
                matches!(contexts[0].clone().extend_interface_context(&contexts[1]),
                Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, crate::artifact_inventory::ArtifactInventoryFailure::OwnerConflict { .. })
                && matches!(error.owner_conflict, Some(crate::artifact_inventory::ArtifactOwnerConflictEvidence::CompilerRole { .. })))
            );
        }
    }
}

#[test]
fn candidate_native_availability_preserves_unselected_historical_offers() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeOriginalRequirement};

    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let historical = product("HistoricalCandidateSupport", 123);
    let fresh = product("FreshCandidateProduct", 124);
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), historical.clone()).unwrap());
    let fresh_entry = Arc::new(ArtifactEntry::original(producer.sha256(), fresh.clone()).unwrap());
    let inventory = ArtifactInventory::default();
    let issued = inventory
        .admit_shared(&inventory.empty_view(), vec![entry.clone()])
        .unwrap();
    let proof = NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &issued)
        .unwrap()
        .unwrap();
    let full = IssuedNativeOffer::issue(entry.descriptor.id, &issued, Some(proof)).unwrap();
    assert!(full.is_full());
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let partial = full
        .within_projection(&projection, &inventory.empty_view())
        .unwrap()
        .unwrap();
    assert!(!partial.is_full());
    let empty = IssuedNativeOffer::issue(entry.descriptor.id, &issued, None).unwrap();
    let baseline = type_context(&[historical, fresh.clone()]);
    let candidates = crate::module_candidates::CandidateSet {
        manifest_path: PathBuf::from("candidate-manifest-not-consumed-by-native-issuance"),
        by_owner: BTreeMap::new(),
        native_availability: vec![fresh],
        catalog_owner: None,
        input_transport: Vec::new(),
    };
    let mut histories = 0;
    for offers in [
        vec![],
        vec![full.clone()],
        vec![empty],
        vec![partial],
        vec![full.clone(), full],
    ] {
        let support = baseline
            .compiler_source_support()
            .retain_offers(offers)
            .unwrap();
        let context = baseline
            .as_ref()
            .clone()
            .rebuild(|facts| facts.compiler_support = support);
        let expected_offers = context
            .compiler_source_support()
            .native_offer_observations()
            .unwrap();
        let expected_custody = context.artifact_view().capture_graph_selection();
        let expected_edges = context.artifact_view().binding_dependencies();
        assert!(context
            .compiler_input_projection()
            .roles()
            .iter()
            .all(|role| role.original().is_none()));
        assert!(context.artifact_view().selected_native_groups().is_empty());

        let inputs = OriginalCompilerInputs::from_candidates(&context, producer, &candidates)
            .expect("historical availability outside current native roles remains custody")
            .unwrap();
        assert_eq!(
            context
                .compiler_source_support()
                .native_offer_observations()
                .unwrap(),
            expected_offers,
            "candidate issuance cannot alter the retained offer mode or exact proof",
        );
        let retained = inputs.artifacts.capture_graph_selection();
        assert!(expected_custody
            .bindings
            .iter()
            .all(|binding| retained.bindings.contains(binding)));
        assert!(expected_edges
            .iter()
            .all(|edge| inputs.artifacts.binding_dependencies().contains(edge)));
        assert!(inputs.artifacts.selected_native_groups().is_empty());
        assert!(context.lexical_graph().is_empty());
        assert_eq!(
            inputs
                .projection
                .roles()
                .iter()
                .filter_map(CompilerInputRole::original)
                .collect::<Vec<_>>(),
            vec![fresh_entry.descriptor.id]
        );
        let [offered] = inputs.native_offers.as_slice() else {
            panic!("one fresh native offer")
        };
        assert_eq!(offered.original(), fresh_entry.descriptor.id);
        assert!(offered.is_full());
        assert_eq!(
            offered
                .issued_group_carriers()
                .unwrap()
                .keys()
                .map(|group| (group.binding.artifact(), group.original_ordinal))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                (fresh_entry.descriptor.id, 7),
                (fresh_entry.descriptor.id, 9001)
            ]),
            "the new body earns its own exact groups without promoting historical support",
        );
        histories += 1;
    }
    assert_eq!(histories, 5);
}

#[test]
fn candidate_reuse_preserves_selected_issuer_beside_historical_offer() {
    use crate::artifact_inventory::{IssuedNativeOffer, NativeGroupKey, NativeOriginalRequirement};

    let producer = CanonicalProducerIdentity::from_producer_bytes(PRODUCER);
    let native = product("SelectedIssuerHistory", 131);
    let entry = Arc::new(ArtifactEntry::original(producer.sha256(), native.clone()).unwrap());
    let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
    let inventory = ArtifactInventory::default();
    let issue = |marker, version| {
        let view = inventory
            .admit_shared(
                &inventory.empty_view(),
                vec![
                    entry.clone(),
                    Arc::new(ArtifactEntry::canonical(
                        product(marker, version).module_interface().unwrap().clone(),
                    )),
                ],
            )
            .unwrap();
        let proof = NativeOriginalRequirement::from_selected_view(entry.descriptor.id, &view)
            .unwrap()
            .expect("the actual issuer selected native groups");
        let offer = IssuedNativeOffer::issue(entry.descriptor.id, &view, Some(proof)).unwrap();
        assert!(offer.is_full());
        (view, offer)
    };
    let (left, a) = issue("HistoricalIssuerA", 132);
    let (right, b) = issue("CurrentIssuerB", 133);
    let exact_groups = |view: &ArtifactView| {
        view.capture_graph_selection()
            .roots
            .into_iter()
            .filter_map(|node| match node {
                crate::artifact_inventory::ArtifactBindingNode::Group(group)
                    if group.binding.artifact() == entry.descriptor.id =>
                {
                    Some(group)
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>()
    };
    let a_groups = exact_groups(&left);
    let b_groups = exact_groups(&right);
    assert_eq!(a_groups.len(), 2);
    assert_eq!(b_groups.len(), 2);
    assert!(a_groups.is_disjoint(&b_groups));
    let support = fixture_compiler_support(projection.clone(), right.clone());
    let original_graph = support.artifacts().capture_graph_selection();
    for extra in [
        vec![b.clone()],
        vec![a.clone(), b.clone()],
        vec![b.clone(), a.clone()],
    ] {
        let includes_a = extra.iter().any(|offer| offer == &a);
        let carried = support.retain_offers(extra).unwrap();
        let graph = carried.artifacts().capture_graph_selection();
        assert_eq!(
            graph.roots, original_graph.roots,
            "adding availability cannot change selected demand"
        );
        assert_eq!(
            graph.namespace, original_graph.namespace,
            "adding availability cannot change selected namespace"
        );
        assert_eq!(exact_groups(carried.artifacts()), b_groups);
        if includes_a {
            assert!(a
                .custody()
                .binding_dependencies()
                .iter()
                .all(|edge| carried.artifacts().binding_dependencies().contains(edge)));
        }
        assert_eq!(
            NativeOriginalRequirement::from_selected_view(entry.descriptor.id, carried.artifacts())
                .unwrap()
                .as_ref(),
            b.proof()
        );
        let context = Arc::new(ExactDeclarationContext::from_facts(
            DeclarationContextFacts {
                producer: producer.sha256(),
                compiler_support: carried,
                lexical: Default::default(),
                template_imports: None,
                original_instance_environment: OriginalInstanceEnvironment::Unknown,
            },
        ));
        let baseline = ProtectedScaffoldRequirements::default()
            .compiler_inputs(&context, None)
            .expect("independently selected B remains valid with A only in availability custody");
        assert_eq!(exact_groups(&baseline.artifacts), b_groups);
        let candidates = crate::module_candidates::CandidateSet {
            manifest_path: PathBuf::from("native-issuance-does-not-read-manifest"),
            by_owner: BTreeMap::new(),
            native_availability: vec![native.clone()],
            catalog_owner: None,
            input_transport: Vec::new(),
        };
        let reused = OriginalCompilerInputs::from_candidates(&context, producer, &candidates)
            .expect("reusing a candidate cannot promote historical A into a second root")
            .unwrap();
        assert!(reused.artifacts.selected_native_groups().is_empty());
        let root = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(context)
            .prepare_compilation_with_private_input(root.path(), PRODUCER, None, Some(reused))
            .unwrap();
        assert_eq!(exact_groups(&request.compiler_inputs().artifacts), b_groups);
    }
    // Both issuer proofs explicitly supplied as root authority remain a real conflict.
    let both = right
        .retain_issued_offer_custody([a.clone(), b.clone()])
        .unwrap();
    let error = projection
        .admit_interfaces_with_native_offers(&both, vec![entry.clone()], &[a, b])
        .unwrap_err();
    assert!(matches!(error, CompileError::ArtifactInventory(error)
        if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact }
            if artifact == entry.descriptor.id)));
    assert_eq!(
        right
            .selected_native_groups()
            .into_iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            NativeGroupKey {
                artifact: entry.descriptor.id,
                original_ordinal: 7
            },
            NativeGroupKey {
                artifact: entry.descriptor.id,
                original_ordinal: 9001
            }
        ])
    );
}
